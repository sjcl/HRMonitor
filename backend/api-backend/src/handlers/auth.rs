//! Discord login, token refresh, logout, and session probing.
//!
//! These are the only endpoints that mint or revoke credentials. Everything
//! else in this service authenticates with a local signature check and never
//! touches Redis, so the cost of session management is confined to this file.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Json, http::HeaderValue};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use common::auth::{AuthConfig, access_token_from_cookies};
use common::error::AppError;
use common::redis_keys::{
    OAUTH_STATE_TTL_SECS, discord_oauth_state_key, refresh_session_key, rotation_recovery_key,
};
use common::time::unix_now_secs;

use crate::AppState;
use crate::cookies::{self, ALL_KINDS, CookieKind};
use crate::sessions::{self, RotationOutcome, RotationReply, new_session, split_cookie};

/// Where an unspecified `return_to` lands.
const DEFAULT_RETURN_TO: &str = "/me";

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

/// Every response from this module is uncacheable.
///
/// These bodies and `Set-Cookie` headers are per-user credentials; a shared
/// cache holding one would hand someone else's session out.
fn no_store(resp: &mut Response) {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp.headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

fn append_cookie(resp: &mut Response, value: HeaderValue) {
    resp.headers_mut().append(header::SET_COOKIE, value);
}

/// Clear all three cookies. Used on logout and on any failed-credential path,
/// so a browser is never left holding a token the server has rejected.
fn clear_all_cookies(resp: &mut Response, cfg: &AuthConfig) {
    for kind in ALL_KINDS {
        append_cookie(resp, cookies::clear(cfg, kind));
    }
}

// ---------------------------------------------------------------------------
// return_to validation
// ---------------------------------------------------------------------------

/// Resolve a caller-supplied `return_to` against our own origin.
///
/// Parsing against a fixed base and comparing origins is the reliable check;
/// substring tests for `//` or `://` miss encodings that browsers still follow.
/// Backslashes and control characters are rejected outright because browsers
/// disagree about how to normalise them, and a disagreement between our parser
/// and the browser's is exactly what an open-redirect needs.
///
/// Returns a path (plus query), never an absolute URL, so the caller cannot
/// echo attacker-controlled text straight into `Location`.
pub fn validate_return_to(base: &url::Url, candidate: Option<&str>) -> String {
    let Some(raw) = candidate else {
        return DEFAULT_RETURN_TO.to_string();
    };
    if raw.is_empty()
        || raw.contains('\\')
        || raw.chars().any(|c| c.is_control())
        || !raw.starts_with('/')
    {
        return DEFAULT_RETURN_TO.to_string();
    }
    let Ok(resolved) = base.join(raw) else {
        return DEFAULT_RETURN_TO.to_string();
    };
    if resolved.origin() != base.origin() {
        return DEFAULT_RETURN_TO.to_string();
    }
    match resolved.query() {
        Some(q) => format!("{}?{q}", resolved.path()),
        None => resolved.path().to_string(),
    }
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

fn random_b64(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::fill(bytes.as_mut_slice());
    URL_SAFE_NO_PAD.encode(bytes)
}

/// S256 challenge for a verifier (RFC 7636 §4.2).
pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

// ---------------------------------------------------------------------------
// GET /api/auth/login/discord
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub return_to: Option<String>,
    /// Browser IANA timezone, used to seed `users.timezone` on first login.
    pub tz: Option<String>,
}

/// Ticket held in Redis for the duration of an authorization request.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscordOAuthTicket {
    pub code_verifier: String,
    /// HMAC of the nonce placed in the browser's cookie. Storing the hash means
    /// a Redis dump cannot be replayed as a valid callback.
    pub nonce_hash: String,
    pub return_to: String,
    pub timezone: Option<String>,
    pub created_at: i64,
}

pub async fn login_discord(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let return_to = validate_return_to(&state.public_origin, q.return_to.as_deref());
    // Validated here so a bad value never reaches the database, and re-validated
    // after the round trip through Redis.
    let timezone =
        q.tz.as_deref()
            .and_then(|tz| crate::validation::validate_timezone(tz).ok())
            .map(str::to_string);

    let oauth_state = random_b64(32);
    let nonce = random_b64(32);
    let code_verifier = random_b64(32);
    let challenge = pkce_challenge(&code_verifier);

    let ticket = DiscordOAuthTicket {
        code_verifier,
        nonce_hash: state.session_keys.hash_secret(&nonce),
        return_to,
        timezone,
        created_at: unix_now_secs(),
    };

    let mut redis = state.redis.clone();
    let payload = serde_json::to_string(&ticket).expect("ticket serialisation is infallible");
    // NX guards against a colliding `state`; astronomically unlikely, free to check.
    let stored: Option<String> = redis::cmd("SET")
        .arg(discord_oauth_state_key(&oauth_state))
        .arg(payload)
        .arg("NX")
        .arg("EX")
        .arg(OAUTH_STATE_TTL_SECS)
        .query_async(&mut redis)
        .await
        .map_err(|e| {
            tracing::error!("Failed to store OAuth ticket: {e}");
            AppError::Internal("Login temporarily unavailable".into())
        })?;
    if stored.is_none() {
        return Err(AppError::Internal("Login temporarily unavailable".into()));
    }

    let url = state
        .discord_oauth
        .authorization_url(&oauth_state, &challenge);

    let mut resp = Redirect::to(&url).into_response();
    no_store(&mut resp);
    // Binds this authorization request to this browser (RFC 9700 §2.1): without
    // it, an attacker could complete their own login in a victim's session by
    // getting the victim to follow the callback URL.
    append_cookie(
        &mut resp,
        cookies::set(
            &state.auth_config,
            CookieKind::OAuth,
            &nonce,
            OAUTH_STATE_TTL_SECS,
        ),
    );
    Ok(resp)
}

// ---------------------------------------------------------------------------
// GET /api/auth/callback/discord
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

pub async fn callback_discord(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let outcome = callback_inner(&state, &headers, q).await;

    // One exit point, so the short-lived OAuth cookie is cleared on every path,
    // success or failure — including the early returns inside `callback_inner`.
    let mut resp = match outcome {
        Ok(done) => {
            let mut resp = Redirect::to(&done.return_to).into_response();
            append_cookie(
                &mut resp,
                cookies::set(
                    &state.auth_config,
                    CookieKind::Access,
                    &done.access_token,
                    cookies::access_max_age(),
                ),
            );
            append_cookie(
                &mut resp,
                cookies::set(
                    &state.auth_config,
                    CookieKind::Refresh,
                    &done.refresh_cookie,
                    done.refresh_max_age,
                ),
            );
            resp
        }
        Err(reason) => {
            tracing::warn!(reason, "Discord login failed");
            Redirect::to(&format!("/login?error={reason}")).into_response()
        }
    };
    append_cookie(
        &mut resp,
        cookies::clear(&state.auth_config, CookieKind::OAuth),
    );
    no_store(&mut resp);
    resp
}

struct LoginComplete {
    return_to: String,
    access_token: String,
    refresh_cookie: String,
    refresh_max_age: i64,
}

async fn callback_inner(
    state: &AppState,
    headers: &HeaderMap,
    q: CallbackQuery,
) -> Result<LoginComplete, &'static str> {
    if q.error.is_some() {
        return Err("denied");
    }
    let (Some(code), Some(oauth_state)) = (q.code, q.state) else {
        return Err("invalid_request");
    };

    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let nonce = common::auth::cookie_value(cookie_header, &state.auth_config.oauth_cookie_name)
        .ok_or("missing_state_cookie")?;

    let mut redis = state.redis.clone();
    // GETDEL: the ticket is consumed exactly once. A replayed callback finds
    // nothing, so "already used" and "expired" collapse into one failure.
    let raw: Option<String> = redis
        .get_del(discord_oauth_state_key(&oauth_state))
        .await
        .map_err(|e| {
            tracing::error!("Failed to consume OAuth ticket: {e}");
            "unavailable"
        })?;
    let ticket: DiscordOAuthTicket =
        serde_json::from_str(&raw.ok_or("invalid_state")?).map_err(|_| "invalid_state")?;

    // Constant-time: this is the check that binds the flow to this browser.
    if !sessions::hashes_match(&state.session_keys.hash_secret(&nonce), &ticket.nonce_hash) {
        return Err("invalid_state");
    }

    let access = state
        .discord_oauth
        .exchange_code(&code, &ticket.code_verifier)
        .await
        .map_err(|e| {
            tracing::warn!("Discord code exchange failed: {e}");
            "exchange_failed"
        })?;
    let user = state.discord_oauth.fetch_user(&access).await.map_err(|e| {
        tracing::warn!("Discord identify failed: {e}");
        "exchange_failed"
    })?;
    // `access` is dropped here: Discord tokens are never persisted.

    // Re-validate: Redis is our own store, but a timezone that fails validation
    // now (e.g. after a tzdata change) must not reach the DB.
    let timezone = ticket
        .timezone
        .as_deref()
        .and_then(|tz| crate::validation::validate_timezone(tz).ok())
        .map(str::to_string)
        .unwrap_or_else(|| "UTC".to_string());

    let user_id = upsert_discord_user(&state.db, &user, &timezone)
        .await
        .map_err(|e| {
            tracing::error!("Discord user upsert failed: {e}");
            "unavailable"
        })?;

    let now = unix_now_secs();
    let sid = sessions::generate_sid();
    let token = sessions::generate_token(&state.session_keys, &sid);
    let session = new_session(&user_id, token.hash.clone(), now);

    let payload = serde_json::to_string(&session).expect("session serialisation is infallible");
    let _: () = redis::cmd("SET")
        .arg(refresh_session_key(&sid))
        .arg(payload)
        .arg("EXAT")
        .arg(session.exp)
        .query_async(&mut redis)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create refresh session: {e}");
            "unavailable"
        })?;

    let (access_token, _) = state
        .jwt_signer
        .sign(&user_id, &sid, now)
        .map_err(|_| "unavailable")?;

    Ok(LoginComplete {
        return_to: ticket.return_to,
        access_token,
        refresh_cookie: token.cookie_value,
        refresh_max_age: session.exp - now,
    })
}

/// Find or create the local user behind a Discord account.
///
/// The advisory lock serialises concurrent *first* logins for the same Discord
/// account. Without it the unique constraint on
/// `accounts (provider, provider_account_id)` protects the data but one of the
/// two racers gets a 500 instead of logging in.
///
/// Existing users keep their `timezone`: it is theirs to change in settings,
/// and a login should not overwrite it from a browser hint.
async fn upsert_discord_user(
    db: &sqlx::PgPool,
    user: &common::discord_oauth::DiscordUser,
    timezone: &str,
) -> Result<String, sqlx::Error> {
    let display_name = user.display_name();
    let avatar_url = user.avatar_url();

    let mut tx = db.begin().await?;

    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('discord:' || $1, 0))")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;

    let existing: Option<String> = sqlx::query_scalar(
        "SELECT user_id FROM accounts WHERE provider = 'discord' AND provider_account_id = $1",
    )
    .bind(&user.id)
    .fetch_optional(&mut *tx)
    .await?;

    let user_id = match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE accounts SET provider_name = $1, provider_image = $2, updated_at = now()
                 WHERE provider = 'discord' AND provider_account_id = $3",
            )
            .bind(display_name)
            .bind(&avatar_url)
            .bind(&user.id)
            .execute(&mut *tx)
            .await?;
            id
        }
        None => {
            let id: String = sqlx::query_scalar(
                "INSERT INTO users (display_name, timezone) VALUES ($1, $2) RETURNING id",
            )
            .bind(display_name)
            .bind(timezone)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO accounts (user_id, provider, provider_account_id, account_type,
                                       provider_name, provider_image)
                 VALUES ($1, 'discord', $2, 'oauth', $3, $4)",
            )
            .bind(&id)
            .bind(&user.id)
            .bind(display_name)
            .bind(&avatar_url)
            .execute(&mut *tx)
            .await?;
            id
        }
    };

    tx.commit().await?;
    Ok(user_id)
}

// ---------------------------------------------------------------------------
// POST /api/auth/refresh
// ---------------------------------------------------------------------------

pub async fn refresh(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let cfg = &state.auth_config;
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let unauthorized = |msg: &str| {
        let mut resp = (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response();
        clear_all_cookies(&mut resp, cfg);
        no_store(&mut resp);
        resp
    };

    let Some(raw) = common::auth::cookie_value(cookie_header, &cfg.refresh_cookie_name) else {
        return unauthorized("No refresh token");
    };
    let Some((sid, secret)) = split_cookie(&raw) else {
        return unauthorized("Malformed refresh token");
    };

    let now = unix_now_secs();
    let presented_hash = state.session_keys.hash_secret(secret);

    // The replacement is minted *before* the script runs so its hash can go
    // into the AAD of the sealed copy, keeping rotation to one round trip.
    let next = sessions::generate_token(&state.session_keys, sid);
    let sealed = state
        .session_keys
        .seal_secret(sid, &next.hash, &next.secret);

    let mut redis = state.redis.clone();
    let reply: Result<Vec<redis::Value>, _> = redis::Script::new(sessions::ROTATE_SCRIPT)
        .key(refresh_session_key(sid))
        .key(rotation_recovery_key(sid))
        .arg(&presented_hash)
        .arg(now)
        .arg(sessions::grace_secs())
        .arg(&next.hash)
        .arg(&sealed)
        .invoke_async(&mut redis)
        .await;

    let parts = match reply {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Refresh rotation failed: {e}");
            // Cookies are left alone: this is our outage, not a bad credential,
            // and the client should retry rather than be logged out.
            let mut resp = (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "Session store unavailable"})),
            )
                .into_response();
            no_store(&mut resp);
            return resp;
        }
    };

    let Some(reply) = RotationReply::from_parts(parts) else {
        tracing::error!("Unrecognised rotation reply");
        return unauthorized("Invalid session");
    };

    match reply.outcome {
        RotationOutcome::Rotated => issue_refresh_response(
            &state,
            &reply.user_id,
            sid,
            &next.cookie_value,
            reply.exp,
            now,
        ),
        RotationOutcome::Grace => {
            // A concurrent refresh already rotated, or our own earlier response
            // was lost. Recover the winner's secret so this client ends up
            // holding the live token instead of a doomed one.
            let recovered = reply.sealed_secret.as_deref().and_then(|sealed| {
                state
                    .session_keys
                    .open_secret(sid, &reply.current_hash, sealed)
            });
            match recovered {
                Some(secret) => issue_refresh_response(
                    &state,
                    &reply.user_id,
                    sid,
                    &format!("{sid}.{secret}"),
                    reply.exp,
                    now,
                ),
                None => {
                    // Degraded but safe: the browser keeps whatever refresh
                    // cookie it has (usually the winner's, set moments ago).
                    tracing::debug!(sid, "Grace refresh without a recoverable secret");
                    issue_access_only_response(&state, &reply.user_id, sid, now)
                }
            }
        }
        RotationOutcome::ReuseDetected => {
            tracing::warn!(sid, "Refresh token reuse detected; session revoked");
            unauthorized("Session revoked")
        }
        RotationOutcome::Expired | RotationOutcome::NotFound => unauthorized("Session expired"),
        RotationOutcome::UnknownHash => {
            // Not treated as reuse: see `RotationOutcome::UnknownHash`.
            tracing::debug!(sid, "Refresh presented an unrecognised secret");
            unauthorized("Invalid refresh token")
        }
    }
}

fn issue_refresh_response(
    state: &AppState,
    user_id: &str,
    sid: &str,
    refresh_cookie: &str,
    session_exp: i64,
    now: i64,
) -> Response {
    let Ok((access_token, _)) = state.jwt_signer.sign(user_id, sid, now) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "signing failed").into_response();
    };
    let mut resp = StatusCode::NO_CONTENT.into_response();
    append_cookie(
        &mut resp,
        cookies::set(
            &state.auth_config,
            CookieKind::Access,
            &access_token,
            cookies::access_max_age(),
        ),
    );
    // Tracks the session's remaining life rather than resetting to 30 days, so
    // the cookie cannot outlive the record it refers to.
    append_cookie(
        &mut resp,
        cookies::set(
            &state.auth_config,
            CookieKind::Refresh,
            refresh_cookie,
            session_exp - now,
        ),
    );
    no_store(&mut resp);
    resp
}

fn issue_access_only_response(state: &AppState, user_id: &str, sid: &str, now: i64) -> Response {
    let Ok((access_token, _)) = state.jwt_signer.sign(user_id, sid, now) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "signing failed").into_response();
    };
    let mut resp = StatusCode::NO_CONTENT.into_response();
    append_cookie(
        &mut resp,
        cookies::set(
            &state.auth_config,
            CookieKind::Access,
            &access_token,
            cookies::access_max_age(),
        ),
    );
    no_store(&mut resp);
    resp
}

// ---------------------------------------------------------------------------
// POST /api/auth/logout
// ---------------------------------------------------------------------------

/// Revoke the session and clear cookies.
///
/// Deliberately *not* behind `require_auth`: an idle tab's access token has
/// normally expired, and `JwtVerifier` rejects it, so requiring one would
/// strand a live refresh session in Redis while the user believes they are out.
///
/// That leaves two independent proofs of holding the session, and revocation
/// requires one of them:
///
/// 1. A currently valid access token — signature-checked, and its `sid` claim
///    is then authority enough to delete outright.
/// 2. The refresh cookie, whose secret is HMAC-checked against the stored
///    generations *inside* [`sessions::REVOKE_SCRIPT`], so the check and the
///    delete cannot be split by a concurrent rotation.
///
/// The `sid` alone is explicitly **not** a proof: it rides in the access JWT
/// and is not secret, so honouring it would let anyone who learns a `sid` force
/// that user offline — the same DoS that [`sessions::RotationOutcome::UnknownHash`]
/// refuses to allow on the refresh path.
///
/// Every non-error path answers 204 and clears cookies, whether or not anything
/// was deleted: a caller must not be able to probe which sessions exist, and a
/// browser should drop its cookies regardless.
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let cfg = &state.auth_config;
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let sid_from_jwt = access_token_from_cookies(cookie_header, cfg)
        .and_then(|t| state.jwt_verifier.verify(&t).ok().map(|c| c.sid));

    let refresh = common::auth::cookie_value(cookie_header, &cfg.refresh_cookie_name);
    let refresh_parts = refresh.as_deref().and_then(split_cookie);

    let mut redis = state.redis.clone();
    let revoked: Result<(), redis::RedisError> = match (&sid_from_jwt, refresh_parts) {
        (Some(sid), _) => {
            redis::pipe()
                .del(refresh_session_key(sid))
                .del(rotation_recovery_key(sid))
                .query_async(&mut redis)
                .await
        }
        (None, Some((sid, secret))) => {
            let presented_hash = state.session_keys.hash_secret(secret);
            let status: Result<i64, _> = redis::Script::new(sessions::REVOKE_SCRIPT)
                .key(refresh_session_key(sid))
                .key(rotation_recovery_key(sid))
                .arg(&presented_hash)
                .invoke_async(&mut redis)
                .await;
            match status {
                // `debug`, not `warn`: `sid` is public, so anyone could
                // generate this line at will.
                Ok(status) => {
                    if !matches!(
                        sessions::revoke_status_to_outcome(status),
                        Some(sessions::RevokeOutcome::Revoked)
                    ) {
                        tracing::debug!(sid, status, "Logout presented no valid refresh secret");
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        (None, None) => Ok(()),
    };

    if let Err(e) = revoked {
        // Reporting success here would be a lie with consequences: the
        // refresh token stays live for up to 30 days. Keep the auth cookies
        // so the client can retry; the OAuth cookie is short-lived and
        // irrelevant, so it goes either way.
        tracing::error!("Logout failed to revoke session: {e}");
        let mut resp = (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Session store unavailable"})),
        )
            .into_response();
        append_cookie(&mut resp, cookies::clear(cfg, CookieKind::OAuth));
        no_store(&mut resp);
        return resp;
    }

    let mut resp = StatusCode::NO_CONTENT.into_response();
    clear_all_cookies(&mut resp, cfg);
    no_store(&mut resp);
    resp
}

// ---------------------------------------------------------------------------
// GET /api/auth/session
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub authenticated: bool,
    pub expires_at: i64,
}

/// Report whether the access token is currently valid, and until when.
///
/// Exists for the WebSocket path: a browser cannot read the status or body of a
/// failed upgrade, so the SPA checks freshness over HTTP first and refreshes
/// before opening the socket. Local verification only — no DB, no Redis.
pub async fn session(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let claims = common::auth::verify_request_token(state.as_ref(), &headers);
    let mut resp = match claims {
        Some(c) => Json(SessionInfo {
            authenticated: true,
            expires_at: c.exp,
        })
        .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Not authenticated"})),
        )
            .into_response(),
    };
    no_store(&mut resp);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> url::Url {
        url::Url::parse("https://hr.example.com").unwrap()
    }

    // --- PKCE -------------------------------------------------------------

    #[test]
    fn pkce_matches_the_rfc7636_test_vector() {
        // RFC 7636 Appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_challenges_are_url_safe_and_unpadded() {
        let c = pkce_challenge(&random_b64(32));
        assert!(!c.contains('='));
        assert!(!c.contains('+'));
        assert!(!c.contains('/'));
    }

    #[test]
    fn generated_verifiers_differ() {
        assert_ne!(random_b64(32), random_b64(32));
    }

    // --- return_to --------------------------------------------------------

    #[test]
    fn accepts_same_origin_relative_paths() {
        assert_eq!(validate_return_to(&base(), Some("/groups")), "/groups");
        assert_eq!(
            validate_return_to(&base(), Some("/users/abc")),
            "/users/abc"
        );
        assert_eq!(
            validate_return_to(&base(), Some("/settings?tab=pulsoid")),
            "/settings?tab=pulsoid"
        );
    }

    #[test]
    fn defaults_when_absent() {
        assert_eq!(validate_return_to(&base(), None), DEFAULT_RETURN_TO);
        assert_eq!(validate_return_to(&base(), Some("")), DEFAULT_RETURN_TO);
    }

    #[test]
    fn rejects_absolute_and_cross_origin_targets() {
        for candidate in [
            "https://evil.example/",
            "http://hr.example.com/x",
            "https://hr.example.com.evil.test/",
            "//evil.example",
            "//evil.example/path",
            "javascript:alert(1)",
            "data:text/html,x",
        ] {
            assert_eq!(
                validate_return_to(&base(), Some(candidate)),
                DEFAULT_RETURN_TO,
                "{candidate} must not be honoured"
            );
        }
    }

    #[test]
    fn rejects_backslashes_and_control_characters() {
        // Browsers normalise these inconsistently; a parser disagreement is
        // exactly what an open redirect needs.
        for candidate in [
            r"\\evil.example",
            r"/\evil.example",
            r"\/\/evil.example",
            "/path\nX",
            "/path\rX",
            "/path\tX",
            "/path\0X",
        ] {
            assert_eq!(
                validate_return_to(&base(), Some(candidate)),
                DEFAULT_RETURN_TO,
                "{candidate:?} must not be honoured"
            );
        }
    }

    #[test]
    fn rejects_paths_not_starting_with_a_slash() {
        for candidate in ["groups", "./groups", "../etc", "?x=1"] {
            assert_eq!(
                validate_return_to(&base(), Some(candidate)),
                DEFAULT_RETURN_TO,
                "{candidate} must not be honoured"
            );
        }
    }

    #[test]
    fn normalises_traversal_within_our_own_origin() {
        // Resolved against the base, so the result is still same-origin and is
        // returned as a plain path rather than the caller's raw text.
        let got = validate_return_to(&base(), Some("/a/../b"));
        assert!(got.starts_with('/'), "{got}");
        assert!(!got.contains(".."), "{got}");
    }

    #[test]
    fn works_for_a_loopback_development_origin() {
        let dev = url::Url::parse("http://localhost:3000").unwrap();
        assert_eq!(validate_return_to(&dev, Some("/me")), "/me");
        assert_eq!(
            validate_return_to(&dev, Some("http://localhost:3001/me")),
            DEFAULT_RETURN_TO
        );
    }

    // --- ticket -----------------------------------------------------------

    #[test]
    fn oauth_ticket_round_trips() {
        let t = DiscordOAuthTicket {
            code_verifier: "v".into(),
            nonce_hash: "h".into(),
            return_to: "/me".into(),
            timezone: Some("Asia/Tokyo".into()),
            created_at: 1,
        };
        let encoded = serde_json::to_string(&t).unwrap();
        assert_eq!(
            serde_json::from_str::<DiscordOAuthTicket>(&encoded).unwrap(),
            t
        );
    }

    #[test]
    fn oauth_ticket_tolerates_a_missing_timezone() {
        let t = DiscordOAuthTicket {
            code_verifier: "v".into(),
            nonce_hash: "h".into(),
            return_to: "/me".into(),
            timezone: None,
            created_at: 1,
        };
        let encoded = serde_json::to_string(&t).unwrap();
        assert!(serde_json::from_str::<DiscordOAuthTicket>(&encoded).is_ok());
    }
}
