use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use std::borrow::Cow;
use std::collections::BTreeMap;
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

impl<T: AuthContext> AuthContext for Arc<T> {
    fn db(&self) -> &sqlx::PgPool {
        self.as_ref().db()
    }
    fn auth_config(&self) -> &AuthConfig {
        self.as_ref().auth_config()
    }
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
    .bind(token.as_ref())
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

/// Parses a cookie value out of a `Cookie` header.
///
/// Handles Auth.js v5 chunked cookies: when a session cookie exceeds ~3936
/// bytes Auth.js splits it into `<name>.0`, `<name>.1`, ... and omits the
/// unchunked form. An exact-name match always takes precedence; chunks are
/// reassembled only when indices form a contiguous `0..n` sequence, so a
/// partial/gapped set falls through to `None` rather than synthesising a
/// token the server never issued.
fn parse_cookie<'a>(header: &'a str, name: &str) -> Option<Cow<'a, str>> {
    let mut chunks: Option<BTreeMap<usize, &'a str>> = None;

    for pair in header.split(';') {
        let pair = pair.trim();
        let Some(rest) = pair.strip_prefix(name) else {
            continue;
        };
        if let Some(value) = rest.strip_prefix('=') {
            return Some(Cow::Borrowed(value));
        }
        if let Some(after_dot) = rest.strip_prefix('.')
            && let Some((idx_str, value)) = after_dot.split_once('=')
            && let Ok(idx) = idx_str.parse::<usize>()
        {
            chunks.get_or_insert_with(BTreeMap::new).insert(idx, value);
        }
    }

    let chunks = chunks?;
    for (expected, actual) in chunks.keys().copied().enumerate() {
        if expected != actual {
            return None;
        }
    }
    Some(Cow::Owned(chunks.into_values().collect()))
}

#[cfg(test)]
mod tests {
    use super::parse_cookie;
    use std::borrow::Cow;

    const NAME: &str = "authjs.session-token";

    #[test]
    fn returns_borrowed_for_unchunked_match() {
        let header = "other=x; authjs.session-token=abc; foo=bar";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "abc");
        assert!(matches!(got, Cow::Borrowed(_)));
    }

    #[test]
    fn reassembles_ordered_chunks() {
        let header = "authjs.session-token.0=ab; authjs.session-token.1=cd";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "abcd");
        assert!(matches!(got, Cow::Owned(_)));
    }

    #[test]
    fn reassembles_out_of_order_chunks() {
        let header = "authjs.session-token.1=cd; authjs.session-token.0=ab";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "abcd");
    }

    #[test]
    fn unchunked_wins_over_chunks() {
        let header =
            "authjs.session-token=full; authjs.session-token.0=foo; authjs.session-token.1=bar";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "full");
        assert!(matches!(got, Cow::Borrowed(_)));
    }

    #[test]
    fn missing_cookie_returns_none() {
        assert!(parse_cookie("other=x; foo=bar", NAME).is_none());
        assert!(parse_cookie("", NAME).is_none());
    }

    #[test]
    fn rejects_chunks_not_starting_at_zero() {
        let header = "authjs.session-token.1=cd; authjs.session-token.2=ef";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn rejects_chunks_with_gap() {
        let header = "authjs.session-token.0=ab; authjs.session-token.2=ef";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn rejects_prefix_false_positive() {
        let header = "authjs.session-token-extra=foo";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn rejects_non_numeric_suffix() {
        let header = "authjs.session-token.sig=foo";
        assert!(parse_cookie(header, NAME).is_none());
    }
}
