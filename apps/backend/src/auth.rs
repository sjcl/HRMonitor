use axum::extract::{Request, State};
use axum::http::StatusCode;
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

pub fn ensure_self(auth_user: &AuthenticatedUser, target_id: &str) -> Result<(), AppError> {
    if auth_user.id != target_id {
        return Err(AppError::Forbidden(
            "Cannot modify another user's resources".into(),
        ));
    }
    Ok(())
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
    // TODO: when groups are implemented, allow access for `group_default` users
    // if auth_user is in the target's group.
    match vis.as_deref() {
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
