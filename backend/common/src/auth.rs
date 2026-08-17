use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::AppError;
use crate::jwt::{Claims, JwtVerifier};

/// Cookie names and attributes.
///
/// Production uses the `__Host-` prefix, which browsers only honour when the
/// cookie is `Secure`, has `Path=/`, and has no `Domain` — exactly the shape we
/// want, and one the browser itself enforces.
///
/// That prefix is unusable over plain HTTP, so local development needs
/// different names. See [`AuthConfig::resolve`] for how the two modes are kept
/// from bleeding into each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub access_cookie_name: String,
    pub refresh_cookie_name: String,
    /// Short-lived cookie binding an in-flight OAuth `state` to this browser.
    pub oauth_cookie_name: String,
    pub cookie_secure: bool,
    /// Always `None`. Present so that the "no Domain attribute" rule is visible
    /// at the type level rather than being an unwritten assumption.
    pub cookie_domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieConfigError {
    /// `AUTH_INSECURE_DEV_COOKIES` was set for an origin that is not loopback.
    InsecureCookiesNotAllowed(String),
}

impl std::fmt::Display for CookieConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CookieConfigError::InsecureCookiesNotAllowed(origin) => write!(
                f,
                "AUTH_INSECURE_DEV_COOKIES=1 is only permitted when PUBLIC_ORIGIN is \
                 http://localhost, http://127.0.0.1 or http://[::1] (got {origin:?})"
            ),
        }
    }
}

impl std::error::Error for CookieConfigError {}

impl AuthConfig {
    fn secure() -> Self {
        Self {
            access_cookie_name: "__Host-hrmonitor_session".into(),
            refresh_cookie_name: "__Host-hrmonitor_refresh".into(),
            oauth_cookie_name: "__Host-hrmonitor_oauth".into(),
            cookie_secure: true,
            cookie_domain: None,
        }
    }

    fn insecure_dev() -> Self {
        Self {
            access_cookie_name: "hrmonitor_session_dev".into(),
            refresh_cookie_name: "hrmonitor_refresh_dev".into(),
            oauth_cookie_name: "hrmonitor_oauth_dev".into(),
            cookie_secure: false,
            cookie_domain: None,
        }
    }

    /// Decide cookie naming from an explicit opt-in plus the public origin.
    ///
    /// Deliberately *not* keyed on `cfg!(debug_assertions)`: every Dockerfile in
    /// this repo builds `--release`, including the one the development compose
    /// file uses, so a debug-gated dev mode would make local development
    /// impossible to start.
    ///
    /// Instead the insecure mode requires both an explicit, hard-to-set-by-
    /// accident environment variable *and* a loopback origin. Pointing a real
    /// deployment at it fails to start rather than quietly issuing cookies
    /// without `Secure`.
    ///
    /// Changing the cookie *names* alongside the `Secure` flag also makes it
    /// structurally impossible to emit a `__Host-` cookie without `Secure` —
    /// which browsers silently discard, a confusing failure this avoids.
    pub fn resolve(
        insecure_dev: bool,
        public_origin: &url::Url,
    ) -> Result<Self, CookieConfigError> {
        if !insecure_dev {
            return Ok(Self::secure());
        }
        let host = public_origin.host_str().unwrap_or("");
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
        if public_origin.scheme() == "http" && loopback {
            Ok(Self::insecure_dev())
        } else {
            Err(CookieConfigError::InsecureCookiesNotAllowed(
                public_origin.to_string(),
            ))
        }
    }

    /// Panics on an unsafe combination — see [`AuthConfig::resolve`].
    pub fn from_env(public_origin: &url::Url) -> Self {
        let insecure_dev = std::env::var("AUTH_INSECURE_DEV_COOKIES").as_deref() == Ok("1");
        match Self::resolve(insecure_dev, public_origin) {
            Ok(cfg) => {
                if insecure_dev {
                    tracing::warn!(
                        "AUTH_INSECURE_DEV_COOKIES=1: issuing non-Secure development \
                         cookies. Never use this outside local development."
                    );
                }
                cfg
            }
            Err(e) => panic!("{e}"),
        }
    }
}

/// The authenticated caller.
///
/// Only the id: everything else (display name, avatar, timezone, visibility) is
/// read fresh from Postgres by the handlers that need it, so nothing
/// user-visible can go stale for the 30 minutes an access token stays valid.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub id: String,
}

/// Abstracts the bits of application state that `common::auth` /
/// `common::access` need. Each downstream crate (api-backend, ws-gateway)
/// implements this for its local `AppState` / `WsState` so the generic
/// middleware and extractors can share an implementation without depending on
/// a specific state struct.
pub trait AuthContext: Send + Sync + 'static {
    /// Still required: *authorisation* (group membership, heart-rate
    /// visibility) must reflect the current database, not a token snapshot.
    fn db(&self) -> &sqlx::PgPool;
    fn auth_config(&self) -> &AuthConfig;
    fn jwt_verifier(&self) -> &JwtVerifier;
}

impl<T: AuthContext> AuthContext for Arc<T> {
    fn db(&self) -> &sqlx::PgPool {
        self.as_ref().db()
    }
    fn auth_config(&self) -> &AuthConfig {
        self.as_ref().auth_config()
    }
    fn jwt_verifier(&self) -> &JwtVerifier {
        self.as_ref().jwt_verifier()
    }
}

/// Read a named cookie out of a `Cookie` header.
///
/// Exposed so the auth handlers can read the refresh and OAuth cookies with
/// exactly the same parser that authenticates requests.
pub fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<Cow<'a, str>> {
    parse_cookie(cookie_header, name)
}

/// Pull the raw access token out of a `Cookie` header.
pub fn access_token_from_cookies<'a>(
    cookie_header: &'a str,
    cfg: &AuthConfig,
) -> Option<Cow<'a, str>> {
    parse_cookie(cookie_header, &cfg.access_cookie_name)
}

/// Read and verify the access token from a request's headers.
///
/// Exposed separately from [`require_auth`] because `/api/auth/logout` and
/// `/api/auth/session` must inspect a possibly-expired token without being
/// rejected by middleware.
pub fn verify_request_token<T: AuthContext>(
    state: &T,
    headers: &axum::http::HeaderMap,
) -> Option<Claims> {
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = access_token_from_cookies(cookie_header, state.auth_config())?;
    state.jwt_verifier().verify(&token).ok()
}

/// Authenticates a request from the access-token cookie.
///
/// This is a pure signature check — no database query, no Redis lookup. The
/// verified [`Claims`] are inserted alongside [`AuthenticatedUser`] so handlers
/// such as logout can reach `sid` without re-parsing the cookie.
pub async fn require_auth<T: AuthContext>(
    State(state): State<Arc<T>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let cookie_header = req
        .headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let Some(token) = access_token_from_cookies(cookie_header, state.auth_config()) else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    match state.jwt_verifier().verify(&token) {
        Ok(claims) => {
            req.extensions_mut().insert(AuthenticatedUser {
                id: claims.sub.clone(),
            });
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(e) => {
            // Expired tokens are the common, expected case (the SPA refreshes
            // and retries), so this is debug, not warn — and never logs the
            // token itself.
            tracing::debug!(reason = %e, "Rejecting request: invalid access token");
            Err(StatusCode::UNAUTHORIZED)
        }
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
/// The chunk-reassembly logic dates from Auth.js v5, which split session
/// cookies larger than ~3936 bytes into `<name>.0`, `<name>.1`, ... Our own
/// tokens are far below that limit so the chunked path never fires in practice,
/// but it is harmless, well tested, and keeps the parser tolerant of any
/// leftover cookies from before the migration. An exact-name match always takes
/// precedence; chunks are reassembled only when indices form a contiguous
/// `0..n` sequence, so a partial set falls through to `None` rather than
/// synthesising a token the server never issued.
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
    use super::{AuthConfig, CookieConfigError, parse_cookie};
    use std::borrow::Cow;

    const NAME: &str = "__Host-hrmonitor_session";

    #[test]
    fn returns_borrowed_for_unchunked_match() {
        let header = "other=x; __Host-hrmonitor_session=abc; foo=bar";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "abc");
        assert!(matches!(got, Cow::Borrowed(_)));
    }

    #[test]
    fn reassembles_ordered_chunks() {
        let header = "__Host-hrmonitor_session.0=ab; __Host-hrmonitor_session.1=cd";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "abcd");
        assert!(matches!(got, Cow::Owned(_)));
    }

    #[test]
    fn reassembles_out_of_order_chunks() {
        let header = "__Host-hrmonitor_session.1=cd; __Host-hrmonitor_session.0=ab";
        let got = parse_cookie(header, NAME).unwrap();
        assert_eq!(got, "abcd");
    }

    #[test]
    fn unchunked_wins_over_chunks() {
        let header = "__Host-hrmonitor_session=full; __Host-hrmonitor_session.0=foo; \
                      __Host-hrmonitor_session.1=bar";
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
        let header = "__Host-hrmonitor_session.1=cd; __Host-hrmonitor_session.2=ef";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn rejects_chunks_with_gap() {
        let header = "__Host-hrmonitor_session.0=ab; __Host-hrmonitor_session.2=ef";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn rejects_prefix_false_positive() {
        let header = "__Host-hrmonitor_session-extra=foo";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn rejects_non_numeric_suffix() {
        let header = "__Host-hrmonitor_session.sig=foo";
        assert!(parse_cookie(header, NAME).is_none());
    }

    #[test]
    fn does_not_confuse_the_refresh_cookie_with_the_access_cookie() {
        // The two names share a long prefix; a sloppy match would swap them.
        let header = "__Host-hrmonitor_refresh=refresh-value";
        assert!(parse_cookie(header, NAME).is_none());
        assert_eq!(
            parse_cookie(header, "__Host-hrmonitor_refresh").unwrap(),
            "refresh-value"
        );
    }

    // --- cookie mode resolution -------------------------------------------

    fn origin(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn defaults_to_secure_host_prefixed_cookies() {
        let cfg = AuthConfig::resolve(false, &origin("https://hr.example.com")).unwrap();
        assert_eq!(cfg.access_cookie_name, "__Host-hrmonitor_session");
        assert_eq!(cfg.refresh_cookie_name, "__Host-hrmonitor_refresh");
        assert_eq!(cfg.oauth_cookie_name, "__Host-hrmonitor_oauth");
        assert!(cfg.cookie_secure);
        assert_eq!(cfg.cookie_domain, None);
    }

    #[test]
    fn secure_mode_even_for_a_loopback_origin_without_the_opt_in() {
        let cfg = AuthConfig::resolve(false, &origin("http://localhost:3000")).unwrap();
        assert!(cfg.cookie_secure);
        assert_eq!(cfg.access_cookie_name, "__Host-hrmonitor_session");
    }

    #[test]
    fn allows_insecure_dev_cookies_on_loopback() {
        for o in [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
        ] {
            let cfg = AuthConfig::resolve(true, &origin(o)).unwrap();
            assert!(!cfg.cookie_secure, "{o}");
            assert_eq!(cfg.access_cookie_name, "hrmonitor_session_dev");
            assert_eq!(cfg.refresh_cookie_name, "hrmonitor_refresh_dev");
            assert_eq!(cfg.oauth_cookie_name, "hrmonitor_oauth_dev");
            assert_eq!(cfg.cookie_domain, None);
        }
    }

    #[test]
    fn refuses_insecure_dev_cookies_for_a_real_deployment() {
        // The whole point: an accidental opt-in in production must not start.
        for o in [
            "http://hr.example.com",
            "https://hr.example.com",
            "http://192.168.1.10:3000",
            "http://localhost.evil.example",
        ] {
            assert!(
                matches!(
                    AuthConfig::resolve(true, &origin(o)),
                    Err(CookieConfigError::InsecureCookiesNotAllowed(_))
                ),
                "{o} must not get insecure cookies"
            );
        }
    }

    #[test]
    fn refuses_insecure_dev_cookies_over_https_loopback() {
        // https + __Host- works fine; there is no reason to downgrade.
        assert!(AuthConfig::resolve(true, &origin("https://localhost:3000")).is_err());
    }
}
