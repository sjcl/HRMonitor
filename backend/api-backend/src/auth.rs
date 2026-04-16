use axum::extract::{FromRef, FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

use crate::AppState;
use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub cookie_name: String,
    pub cookie_name_secure: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            cookie_name: "authjs.session-token".into(),
            cookie_name_secure: "__Secure-authjs.session-token".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub id: String,
    pub display_name: Option<String>,
}

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cookie_header = req
        .headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let session_token = parse_cookie(cookie_header, &state.auth_config.cookie_name)
        .or_else(|| parse_cookie(cookie_header, &state.auth_config.cookie_name_secure));

    let token = match session_token {
        Some(t) => t,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    let user: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT u.id, u.display_name FROM sessions s
         JOIN users u ON s.user_id = u.id
         WHERE s.session_token = $1 AND s.expires > now()",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("Auth session lookup failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match user {
        Some((id, display_name)) => {
            req.extensions_mut()
                .insert(AuthenticatedUser { id, display_name });
            Ok(next.run(req).await)
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Extracts a user ID from the `{id}` path parameter, resolving `"me"` to the
/// authenticated user's ID.
pub struct UserIdParam(pub String);

impl<S: Send + Sync> FromRequestParts<S> for UserIdParam {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(id) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::BadRequest("Missing user id".into()))?;

        if id == "me" {
            let auth_user = parts
                .extensions
                .get::<AuthenticatedUser>()
                .ok_or_else(|| AppError::Unauthorized("Not authenticated".into()))?;
            Ok(UserIdParam(auth_user.id.clone()))
        } else {
            Ok(UserIdParam(id))
        }
    }
}

pub struct ViewableUserId(pub String);

impl<S> FromRequestParts<S> for ViewableUserId
where
    S: Send + Sync,
    Arc<AppState>: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = <Arc<AppState>>::from_ref(state);
        let auth_user = parts
            .extensions
            .get::<AuthenticatedUser>()
            .cloned()
            .ok_or_else(|| AppError::Unauthorized("Not authenticated".into()))?;
        let UserIdParam(target_id) = UserIdParam::from_request_parts(parts, state).await?;
        ensure_can_view_user(&app_state.db, &auth_user, &target_id).await?;
        Ok(ViewableUserId(target_id))
    }
}

/// Check whether `auth_user` is allowed to view `target_id`'s heart rate data.
///
/// `heart_rate_visibility` controls access:
/// - `group_default`: follow the group's visibility settings. Since groups
///   are not yet implemented, this currently denies non-self access.
/// - `private`: always deny non-self access regardless of group settings.
///
/// Returns `NotFound` if the target user does not exist, `Forbidden` if not
/// allowed.
pub async fn ensure_can_view_user(
    db: &sqlx::PgPool,
    auth_user: &AuthenticatedUser,
    target_id: &str,
) -> Result<(), AppError> {
    if auth_user.id == target_id {
        return Ok(());
    }
    let vis: Option<String> =
        sqlx::query_scalar("SELECT heart_rate_visibility FROM users WHERE id = $1")
            .bind(target_id)
            .fetch_optional(db)
            .await?;
    match vis.as_deref() {
        Some("private") => Err(AppError::Forbidden("Not allowed to view this user".into())),
        Some("group_default") => {
            let shared: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM group_members gm1
                    JOIN group_members gm2 ON gm1.group_id = gm2.group_id
                    WHERE gm1.user_id = $1 AND gm1.status = 'active'
                      AND gm2.user_id = $2 AND gm2.status = 'active' AND gm2.sharing = true
                )",
            )
            .bind(&auth_user.id)
            .bind(target_id)
            .fetch_one(db)
            .await?;
            if shared {
                Ok(())
            } else {
                Err(AppError::Forbidden("Not allowed to view this user".into()))
            }
        }
        Some(_) => Err(AppError::Forbidden("Not allowed to view this user".into())),
        None => Err(AppError::NotFound("User not found".into())),
    }
}

fn parse_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(name)
            && let Some(value) = value.strip_prefix('=')
        {
            return Some(value);
        }
    }
    None
}

/// Rejects WebSocket upgrade requests whose `Origin` header is not the
/// configured public origin. Browsers always send `Origin` on WS handshakes
/// (RFC 6455 §10.2), and `Origin` cannot be forged from JavaScript, so an
/// exact-match allowlist is the standard defense against cross-site WS
/// connection attempts. Non-browser clients (curl, tests, server-to-server)
/// legitimately omit `Origin`; we allow that case because the session cookie
/// still gates the endpoint and an attacker without the victim's browser
/// cannot replay the victim's cookie.
pub async fn require_ws_origin(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    match origin {
        None => Ok(next.run(req).await),
        Some(o) if o == state.allowed_ws_origin => Ok(next.run(req).await),
        Some(o) => {
            tracing::warn!(origin = %o, "Rejecting WS upgrade from disallowed origin");
            Err(StatusCode::FORBIDDEN)
        }
    }
}
