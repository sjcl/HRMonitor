//! `Set-Cookie` construction.
//!
//! Every cookie this service issues goes through here, and so does every
//! deletion. That is the point: "clear a cookie with exactly the attributes it
//! was set with" is a rule that is easy to state and easy to get wrong, and
//! routing both through one [`AuthConfig`] makes it structural rather than
//! something to remember at each call site.
//!
//! Hand-built rather than pulling in `axum-extra`'s cookie feature — the
//! strings are short, fully specified below, and covered by tests.

use axum::http::HeaderValue;
use common::auth::AuthConfig;
use common::jwt::ACCESS_TOKEN_TTL_SECS;

/// Which cookie a builder call refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieKind {
    Access,
    Refresh,
    /// Binds an in-flight OAuth `state` to the browser that started it.
    OAuth,
}

impl CookieKind {
    fn name(self, cfg: &AuthConfig) -> &str {
        match self {
            CookieKind::Access => &cfg.access_cookie_name,
            CookieKind::Refresh => &cfg.refresh_cookie_name,
            CookieKind::OAuth => &cfg.oauth_cookie_name,
        }
    }
}

/// All three cookies, for the paths that clear everything.
pub const ALL_KINDS: [CookieKind; 3] = [CookieKind::Access, CookieKind::Refresh, CookieKind::OAuth];

/// Default lifetime of the access cookie: the token's own lifetime.
pub fn access_max_age() -> i64 {
    ACCESS_TOKEN_TTL_SECS
}

fn build(cfg: &AuthConfig, kind: CookieKind, value: &str, max_age: i64) -> String {
    let mut s = String::with_capacity(160);
    s.push_str(kind.name(cfg));
    s.push('=');
    s.push_str(value);
    s.push_str("; Path=/; HttpOnly; SameSite=Lax");
    if cfg.cookie_secure {
        s.push_str("; Secure");
    }
    // `Domain` is deliberately never emitted: the `__Host-` prefix forbids it,
    // and a host-only cookie is what we want in development too.
    debug_assert!(cfg.cookie_domain.is_none(), "cookies must stay host-only");
    s.push_str("; Max-Age=");
    s.push_str(&max_age.to_string());
    s
}

/// A cookie carrying `value`, valid for `max_age` seconds.
///
/// `max_age` is clamped at zero so a session that is already past its hard
/// expiry cannot produce a negative age (which browsers treat as
/// delete-immediately, silently logging the user out mid-request).
pub fn set(cfg: &AuthConfig, kind: CookieKind, value: &str, max_age: i64) -> HeaderValue {
    let cookie = build(cfg, kind, value, max_age.max(0));
    HeaderValue::from_str(&cookie).expect("cookie values are base64url/JWT, always header-safe")
}

/// A deletion cookie: same name and attributes, empty value, `Max-Age=0`.
pub fn clear(cfg: &AuthConfig, kind: CookieKind) -> HeaderValue {
    let cookie = build(cfg, kind, "", 0);
    HeaderValue::from_str(&cookie).expect("static attributes are always header-safe")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secure_cfg() -> AuthConfig {
        AuthConfig::resolve(false, &url::Url::parse("https://hr.example.com").unwrap()).unwrap()
    }

    fn dev_cfg() -> AuthConfig {
        AuthConfig::resolve(true, &url::Url::parse("http://localhost:3000").unwrap()).unwrap()
    }

    fn as_str(v: &HeaderValue) -> &str {
        v.to_str().unwrap()
    }

    #[test]
    fn production_access_cookie_has_every_required_attribute() {
        let cfg = secure_cfg();
        let c = set(&cfg, CookieKind::Access, "token123", 1800);
        let s = as_str(&c);
        assert!(s.starts_with("__Host-hrmonitor_session=token123;"), "{s}");
        assert!(s.contains("; Path=/"), "{s}");
        assert!(s.contains("; HttpOnly"), "{s}");
        assert!(s.contains("; SameSite=Lax"), "{s}");
        assert!(s.contains("; Secure"), "{s}");
        assert!(s.contains("; Max-Age=1800"), "{s}");
        assert!(!s.contains("Domain"), "{s}");
    }

    #[test]
    fn development_cookies_drop_secure_and_the_host_prefix() {
        let cfg = dev_cfg();
        let s = as_str(&set(&cfg, CookieKind::Access, "t", 60)).to_string();
        assert!(s.starts_with("hrmonitor_session_dev=t;"), "{s}");
        assert!(!s.contains("Secure"), "{s}");
        assert!(!s.contains("Domain"), "{s}");
        // Still HttpOnly and SameSite even in development.
        assert!(s.contains("; HttpOnly"), "{s}");
        assert!(s.contains("; SameSite=Lax"), "{s}");
    }

    #[test]
    fn each_kind_uses_its_own_name() {
        let cfg = secure_cfg();
        assert!(
            as_str(&set(&cfg, CookieKind::Access, "v", 1)).starts_with("__Host-hrmonitor_session=")
        );
        assert!(
            as_str(&set(&cfg, CookieKind::Refresh, "v", 1))
                .starts_with("__Host-hrmonitor_refresh=")
        );
        assert!(
            as_str(&set(&cfg, CookieKind::OAuth, "v", 1)).starts_with("__Host-hrmonitor_oauth=")
        );
    }

    #[test]
    fn deletion_matches_the_issuing_attributes_exactly() {
        // A cookie cleared with different attributes is not cleared at all.
        let cfg = secure_cfg();
        for kind in ALL_KINDS {
            let set_c = as_str(&set(&cfg, kind, "value", 1800)).to_string();
            let clear_c = as_str(&clear(&cfg, kind)).to_string();

            let attrs = |s: &str| {
                s.split("; ")
                    .skip(1)
                    .filter(|a| !a.starts_with("Max-Age="))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            };
            assert_eq!(attrs(&set_c), attrs(&clear_c), "{kind:?}");
            assert!(clear_c.contains("; Max-Age=0"), "{clear_c}");
            assert!(
                clear_c.starts_with(&format!("{}=;", kind.name(&cfg))),
                "{clear_c}"
            );
        }
    }

    #[test]
    fn deletion_also_matches_in_development_mode() {
        let cfg = dev_cfg();
        let c = as_str(&clear(&cfg, CookieKind::Refresh)).to_string();
        assert!(c.starts_with("hrmonitor_refresh_dev=;"), "{c}");
        assert!(!c.contains("Secure"), "{c}");
        assert!(c.contains("; Max-Age=0"), "{c}");
    }

    #[test]
    fn negative_max_age_is_clamped_rather_than_emitted() {
        // An already-expired session must not silently become a delete cookie
        // on a path that meant to set one.
        let cfg = secure_cfg();
        let s = as_str(&set(&cfg, CookieKind::Refresh, "v", -50)).to_string();
        assert!(s.contains("; Max-Age=0"), "{s}");
        assert!(!s.contains("-50"), "{s}");
    }

    #[test]
    fn access_cookie_lifetime_tracks_the_token_lifetime() {
        assert_eq!(access_max_age(), ACCESS_TOKEN_TTL_SECS);
    }
}
