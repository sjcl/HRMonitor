//! Same-origin enforcement, shared by api-backend and ws-gateway.
//!
//! Everything the browser talks to is served from a single origin through
//! nginx, so there is no CORS layer anywhere in this workspace and no reason
//! for an `Origin` header ever to differ from `PUBLIC_ORIGIN`. That makes exact
//! string comparison against one canonicalised origin both sufficient and the
//! strictest option available — no wildcards, no subdomain matching.
//!
//! Two entry points, because the two services need different strictness:
//!
//! * [`require_origin_unsafe`] skips safe methods. It backs the HTTP API, where
//!   `GET` is not state-changing and where the Discord OAuth callback *must*
//!   work as a top-level navigation that carries no `Origin` at all.
//! * [`require_origin_always`] checks every request. It backs the WebSocket
//!   upgrade, which is nominally a `GET` but opens a stateful channel and is
//!   therefore not "safe" in the CSRF sense.
//!
//! Both fail closed: a missing or non-UTF-8 `Origin` is rejected exactly like a
//! mismatched one.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Application state that knows which origin is allowed.
pub trait OriginContext: Send + Sync + 'static {
    fn allowed_origin(&self) -> &str;
}

impl<T: OriginContext> OriginContext for Arc<T> {
    fn allowed_origin(&self) -> &str {
        self.as_ref().allowed_origin()
    }
}

/// Reads `PUBLIC_ORIGIN` and canonicalises it.
///
/// Panics when unset or unparseable. A service that cannot tell its own origin
/// cannot enforce same-origin, and starting up in that state would silently
/// accept cross-site requests — the one failure mode worth crashing over.
pub fn load_allowed_origin() -> String {
    let raw = std::env::var("PUBLIC_ORIGIN").unwrap_or_default();
    if raw.trim().is_empty() {
        panic!(
            "PUBLIC_ORIGIN must be set. It is the canonical browser origin used \
             to validate the Origin header and to derive cookie settings."
        );
    }
    canonical_origin(raw.trim())
}

/// Normalises a URL to its browser origin (`scheme://host[:port]`, default
/// ports elided) using the WHATWG serialisation, so that a configured
/// `https://example.com:443/path` still matches an `Origin: https://example.com`.
pub fn canonical_origin(raw: &str) -> String {
    let parsed = url::Url::parse(raw)
        .unwrap_or_else(|e| panic!("PUBLIC_ORIGIN is not a valid URL ({raw:?}): {e}"));
    let origin = parsed.origin();
    if !origin.is_tuple() {
        panic!("PUBLIC_ORIGIN has no host or has an opaque origin ({raw:?})");
    }
    origin.ascii_serialization()
}

/// `Ok(())` if the request may proceed, `Err(reason)` otherwise. The reason is
/// a static string, safe to log.
pub fn check_origin(header: Option<&str>, allowed: &str) -> Result<(), &'static str> {
    match header {
        None => Err("missing or invalid Origin header"),
        Some(o) if o == allowed => Ok(()),
        Some(_) => Err("disallowed Origin"),
    }
}

/// True for methods that cannot change state, and which browsers may therefore
/// issue as top-level navigations without an `Origin` header.
fn is_safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn origin_header(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
}

/// Enforces same-origin on state-changing methods only.
///
/// The exemption for safe methods is what lets `GET /api/auth/callback/discord`
/// work without a path-based special case: Discord redirects the browser there
/// as a top-level navigation, which carries no `Origin`. That callback's CSRF
/// defence is the single-use `state` bound to a browser nonce cookie, plus
/// PKCE — not this header.
pub async fn require_origin_unsafe<T: OriginContext>(
    State(state): State<Arc<T>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if is_safe_method(req.method()) {
        return Ok(next.run(req).await);
    }
    let origin = origin_header(&req);
    match check_origin(origin, state.allowed_origin()) {
        Ok(()) => Ok(next.run(req).await),
        Err(reason) => {
            tracing::warn!(
                origin = ?origin,
                method = %req.method(),
                path = %req.uri().path(),
                reason,
                "Rejecting cross-origin request"
            );
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Enforces same-origin on every request regardless of method.
///
/// Used for WebSocket upgrades: the handshake is a `GET`, but it opens a
/// long-lived authenticated channel, so the safe-method exemption must not
/// apply to it.
pub async fn require_origin_always<T: OriginContext>(
    State(state): State<Arc<T>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let origin = origin_header(&req);
    match check_origin(origin, state.allowed_origin()) {
        Ok(()) => Ok(next.run(req).await),
        Err(reason) => {
            tracing::warn!(origin = ?origin, reason, "Rejecting WS upgrade");
            Err(StatusCode::FORBIDDEN)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{canonical_origin, check_origin, is_safe_method};
    use axum::http::Method;

    #[test]
    fn rejects_missing_origin() {
        assert!(check_origin(None, "http://localhost:3000").is_err());
    }

    #[test]
    fn accepts_exact_match() {
        assert!(check_origin(Some("http://localhost:3000"), "http://localhost:3000").is_ok());
    }

    #[test]
    fn rejects_mismatched_origin() {
        assert!(check_origin(Some("http://evil.example"), "http://localhost:3000").is_err());
    }

    #[test]
    fn rejects_subdomain_and_prefix_lookalikes() {
        let allowed = "https://example.com";
        for candidate in [
            "https://evil.example.com",
            "https://example.com.evil.test",
            "http://example.com",
            "https://example.com:8443",
        ] {
            assert!(
                check_origin(Some(candidate), allowed).is_err(),
                "{candidate} must not match {allowed}"
            );
        }
    }

    #[test]
    fn canonicalises_origin() {
        assert_eq!(
            canonical_origin("http://localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(
            canonical_origin("https://example.com:8443"),
            "https://example.com:8443"
        );
    }

    #[test]
    fn strips_default_ports() {
        assert_eq!(
            canonical_origin("http://example.com:80"),
            "http://example.com"
        );
        assert_eq!(
            canonical_origin("https://example.com:443"),
            "https://example.com"
        );
        assert_eq!(
            canonical_origin("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn strips_path_and_query() {
        assert_eq!(
            canonical_origin("https://example.com/path?x=1"),
            "https://example.com"
        );
    }

    #[test]
    #[should_panic(expected = "has no host or has an opaque origin")]
    fn rejects_opaque_origin() {
        canonical_origin("file:///etc/passwd");
    }

    #[test]
    #[should_panic(expected = "is not a valid URL")]
    fn rejects_unparseable_origin() {
        canonical_origin("not a url");
    }

    #[test]
    fn safe_methods_are_exempt_from_the_unsafe_check() {
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert!(is_safe_method(&m), "{m} should be treated as safe");
        }
        for m in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::CONNECT,
            Method::TRACE,
        ] {
            assert!(!is_safe_method(&m), "{m} must require an Origin check");
        }
    }
}
