use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

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

/// Abstracts the bits of application state that `common::auth` / `common::access`
/// need: the database pool and the auth cookie configuration. Each downstream
/// crate (api-backend, ws-gateway) implements this for its local `AppState` /
/// `WsState` so the generic middleware and extractors can share an
/// implementation without depending on a specific state struct.
pub trait AuthContext: Send + Sync + 'static {
    fn db(&self) -> &sqlx::PgPool;
    fn auth_config(&self) -> &AuthConfig;
}

pub async fn require_auth<T: AuthContext>(
    State(state): State<Arc<T>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cookie_header = req
        .headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let auth_config = state.auth_config();
    let session_token = parse_cookie(cookie_header, &auth_config.cookie_name)
        .or_else(|| parse_cookie(cookie_header, &auth_config.cookie_name_secure));

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
    .fetch_optional(state.db())
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
