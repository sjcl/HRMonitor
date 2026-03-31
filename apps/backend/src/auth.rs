use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

use crate::AppState;

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub cookie_name: String,
    pub cookie_name_secure: String,
}

impl AuthConfig {
    pub fn from_env() -> Self {
        Self {
            cookie_name: std::env::var("AUTH_SESSION_COOKIE_NAME")
                .unwrap_or_else(|_| "authjs.session-token".into()),
            cookie_name_secure: std::env::var("AUTH_SESSION_COOKIE_NAME_SECURE")
                .unwrap_or_else(|_| "__Secure-authjs.session-token".into()),
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
    .bind(&token)
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

fn parse_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(name) {
            if let Some(value) = value.strip_prefix('=') {
                return Some(value);
            }
        }
    }
    None
}
