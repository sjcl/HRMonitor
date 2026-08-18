//! Redis-backed refresh sessions.
//!
//! A refresh token is `{sid}.{secret}`. `sid` names the Redis record and is not
//! secret — it also travels inside the access JWT. `secret` is 32 CSPRNG bytes
//! and is never stored: Redis holds only its HMAC, so a dump of the session
//! store does not yield usable tokens.
//!
//! Rotation happens on every refresh, with a short grace window for the token
//! that was *just* superseded. That window exists for two legitimate races:
//!
//! 1. Two tabs refreshing simultaneously.
//! 2. A rotation that succeeded server-side but whose response never reached the
//!    browser (dropped connection, closed tab).
//!
//! Case 2 is why the winner's new secret is also stored, sealed with AES-256-GCM,
//! under a key that expires with the grace window: without it the loser can
//! never learn the current token and would be forced to re-login at the next
//! refresh, up to 30 minutes later. The AAD binds the ciphertext to
//! `(version, sid, new_current_hash)` — all of which are known *before* the Lua
//! script runs, which is what keeps rotation to a single atomic round trip.
//! (`rotation` cannot be used in the AAD: its new value is only determined
//! inside the script, so sealing would need a prior read and lose atomicity.)

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use common::redis_keys::{REFRESH_GRACE_SECS, REFRESH_SESSION_TTL_SECS};

type HmacSha256 = Hmac<Sha256>;

/// AAD version prefix. Bump if the sealed-secret format ever changes, so old
/// ciphertexts fail to open rather than being misinterpreted.
const SEAL_AAD_VERSION: &str = "hrmonitor.refresh.v1";

const SECRET_LEN: usize = 32;

/// Stored session record. One JSON string per key, matching the convention
/// already used for `latest_bpm`, and simple to read-modify-write from Lua.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshSession {
    pub user_id: String,
    /// hex HMAC-SHA256 of the currently valid secret.
    pub current_hash: String,
    /// The hash this one replaced, still acceptable until `previous_expires_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_expires_at: Option<i64>,
    pub iat: i64,
    /// Hard cap on the whole login. Rotation never moves this.
    pub exp: i64,
    pub rotation: u32,
}

/// What a presented refresh token means for a given session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationOutcome {
    /// The current token: rotate and issue a new one.
    Rotated,
    /// The just-superseded token, inside the grace window. Legitimate
    /// concurrency — reissue an access token, do not revoke anything.
    Grace,
    /// The superseded token *after* the grace window: genuine replay. Revoke
    /// the session.
    ReuseDetected,
    /// Past `exp`.
    Expired,
    /// No such session (never existed, or logged out).
    NotFound,
    /// A secret matching neither generation.
    ///
    /// Deliberately **not** treated as reuse. `sid` is not secret, so revoking
    /// on any bad secret would let anyone who learns a `sid` force that user to
    /// log out. The cost is that a token more than one generation old replays
    /// as a plain 401 without revoking the family; that trade favours
    /// availability, and the grace-window case above covers the honest races.
    UnknownHash,
}

/// Reference implementation of the rotation decision.
///
/// The Lua script in [`ROTATE_SCRIPT`] is a transliteration of this function;
/// keeping the logic here in Rust is what makes every branch unit-testable
/// without a Redis instance. The integration tests then check that the script
/// and this function agree.
///
/// The grace deadline is stored on the session as an absolute timestamp, so the
/// window length is not a parameter here — it is applied once, at rotation.
pub fn decide_rotation(
    session: &RefreshSession,
    presented_hash: &str,
    now: i64,
) -> RotationOutcome {
    // Matches the JWT `exp` rule: the expiry instant itself is already expired.
    if now >= session.exp {
        return RotationOutcome::Expired;
    }
    if constant_time_eq(&session.current_hash, presented_hash) {
        return RotationOutcome::Rotated;
    }
    if let Some(prev) = &session.previous_hash
        && constant_time_eq(prev, presented_hash)
    {
        return match session.previous_expires_at {
            Some(until) if now <= until => RotationOutcome::Grace,
            _ => RotationOutcome::ReuseDetected,
        };
    }
    RotationOutcome::UnknownHash
}

/// Atomic rotation.
///
/// Returns `{status, current_hash, exp, sealed_secret, user_id}` for the two
/// success statuses and `{status}` otherwise. Everything the caller needs to
/// build the response is in that reply, so rotation stays one round trip.
///
/// Status codes rather than `{err=...}`: a Lua `err` table becomes a Redis
/// *error reply*, which the client surfaces as a connection-level failure and
/// cannot easily distinguish from a real outage. Empty strings rather than
/// `nil` for absent values, because a `nil` truncates the returned array.
///
/// 0 = rotated, 1 = grace, 2 = reuse_detected,
/// 3 = expired, 4 = not_found, 5 = unknown_hash
pub const ROTATE_SCRIPT: &str = r#"
-- KEYS[1] = auth:session:v1:{sid}
-- KEYS[2] = auth:rotation:v1:{sid}
-- ARGV    = presented_hash, now, grace_secs, new_current_hash, new_sealed_secret
local raw = redis.call('GET', KEYS[1])
if not raw then return {4} end
local sess = cjson.decode(raw)
local now = tonumber(ARGV[2])
local grace = tonumber(ARGV[3])

if now >= sess.exp then
  redis.call('DEL', KEYS[1])
  redis.call('DEL', KEYS[2])
  return {3}
end

if sess.current_hash == ARGV[1] then
  sess.previous_hash = sess.current_hash
  sess.previous_expires_at = now + grace
  sess.current_hash = ARGV[4]
  sess.rotation = sess.rotation + 1
  -- EXAT, not EX: the 30-day cap is absolute and must not creep forward on
  -- every rotation.
  redis.call('SET', KEYS[1], cjson.encode(sess), 'EXAT', sess.exp)
  -- Seal the winner's secret so a client whose response was lost can recover
  -- it during the grace window. Expires with the window.
  redis.call('SET', KEYS[2], ARGV[5], 'EX', grace)
  return {0, sess.current_hash, sess.exp, '', sess.user_id}
elseif sess.previous_hash == ARGV[1] then
  if sess.previous_expires_at and now <= sess.previous_expires_at then
    local sealed = redis.call('GET', KEYS[2])
    return {1, sess.current_hash, sess.exp, sealed or '', sess.user_id}
  end
  redis.call('DEL', KEYS[1])
  redis.call('DEL', KEYS[2])
  return {2}
else
  return {5}
end
"#;

/// What a presented refresh token means for a revocation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// The secret belonged to this session; both keys are gone.
    Revoked,
    /// No such session — already logged out, or never existed.
    NotFound,
    /// A secret matching neither generation. Nothing was touched.
    ///
    /// Same reasoning as [`RotationOutcome::UnknownHash`]: `sid` is not secret,
    /// so deleting on any bad secret would let anyone who learns a `sid` force
    /// that user to log out.
    HashMismatch,
}

/// Reference implementation of the revocation decision.
///
/// Mirrors [`REVOKE_SCRIPT`] the way [`decide_rotation`] mirrors
/// [`ROTATE_SCRIPT`], so every branch is unit-testable without Redis.
pub fn decide_revocation(session: &RefreshSession, presented_hash: &str) -> RevokeOutcome {
    let matches_previous = session
        .previous_hash
        .as_deref()
        .is_some_and(|prev| constant_time_eq(prev, presented_hash));

    if constant_time_eq(&session.current_hash, presented_hash) || matches_previous {
        RevokeOutcome::Revoked
    } else {
        RevokeOutcome::HashMismatch
    }
}

/// Atomic, authenticated revocation.
///
/// Logout must not delete a session just because the caller named it: `sid`
/// travels in the access JWT and is not secret. This checks the presented
/// secret's HMAC against the stored generations and deletes in the same script,
/// so the check cannot be won by a concurrent rotation.
///
/// `previous_hash` is accepted regardless of the grace deadline. Revocation is
/// the fail-safe direction, and refusing here would strand a tab that logged
/// out moments after a rotation. A post-grace replay would be treated as reuse
/// on the refresh path — which also revokes — so the outcomes agree.
///
/// `exp` is not consulted: deleting an already-expired session is harmless.
///
/// Lua's `==` is not constant-time, but this matches the existing comparison in
/// [`ROTATE_SCRIPT`]; timing differences on an HMAC digest comparison are not
/// exploitable across a network.
///
/// 0 = revoked, 1 = not_found, 2 = hash_mismatch
pub const REVOKE_SCRIPT: &str = r#"
-- KEYS[1] = auth:session:v1:{sid}
-- KEYS[2] = auth:rotation:v1:{sid}
-- ARGV[1] = presented_hash
local raw = redis.call('GET', KEYS[1])
if not raw then return 1 end
local sess = cjson.decode(raw)
if sess.current_hash == ARGV[1] or sess.previous_hash == ARGV[1] then
  redis.call('DEL', KEYS[1])
  redis.call('DEL', KEYS[2])
  return 0
end
return 2
"#;

pub fn revoke_status_to_outcome(status: i64) -> Option<RevokeOutcome> {
    Some(match status {
        0 => RevokeOutcome::Revoked,
        1 => RevokeOutcome::NotFound,
        2 => RevokeOutcome::HashMismatch,
        _ => return None,
    })
}

/// Decoded reply from [`ROTATE_SCRIPT`].
#[derive(Debug, Clone)]
pub struct RotationReply {
    pub outcome: RotationOutcome,
    pub current_hash: String,
    pub exp: i64,
    /// Present only on `Grace`, and only while the recovery key is alive.
    pub sealed_secret: Option<String>,
    pub user_id: String,
}

impl RotationReply {
    /// Decode the raw Lua array.
    pub fn from_parts(parts: Vec<redis::Value>) -> Option<Self> {
        let status = match parts.first()? {
            redis::Value::Int(i) => *i,
            _ => return None,
        };
        let outcome = status_to_outcome(status)?;

        let text = |i: usize| -> String {
            parts
                .get(i)
                .and_then(|v| match v {
                    redis::Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
                    redis::Value::SimpleString(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let int = |i: usize| -> i64 {
            match parts.get(i) {
                Some(redis::Value::Int(v)) => *v,
                _ => 0,
            }
        };

        let sealed = text(3);
        Some(Self {
            outcome,
            current_hash: text(1),
            exp: int(2),
            sealed_secret: (!sealed.is_empty()).then_some(sealed),
            user_id: text(4),
        })
    }
}

pub fn status_to_outcome(status: i64) -> Option<RotationOutcome> {
    Some(match status {
        0 => RotationOutcome::Rotated,
        1 => RotationOutcome::Grace,
        2 => RotationOutcome::ReuseDetected,
        3 => RotationOutcome::Expired,
        4 => RotationOutcome::NotFound,
        5 => RotationOutcome::UnknownHash,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Token material
// ---------------------------------------------------------------------------

/// Keys for hashing and sealing refresh secrets.
///
/// Two separate keys on purpose: an HMAC key and an AEAD key should never be
/// the same bytes, and separating them also means `REFRESH_SEAL_KEY` can be
/// rotated (briefly disabling lost-response recovery) without invalidating
/// every session the way rotating `REFRESH_TOKEN_HMAC_KEY` would.
pub struct SessionKeys {
    hmac_key: [u8; 32],
    seal_key: [u8; 32],
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKeys").finish_non_exhaustive()
    }
}

impl SessionKeys {
    pub fn new(hmac_key: [u8; 32], seal_key: [u8; 32]) -> Self {
        Self { hmac_key, seal_key }
    }

    pub fn from_env() -> Self {
        Self::new(
            decode_key("REFRESH_TOKEN_HMAC_KEY"),
            decode_key("REFRESH_SEAL_KEY"),
        )
    }

    /// hex HMAC-SHA256 of a raw secret.
    pub fn hash_secret(&self, secret: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC-SHA256 accepts any key length");
        mac.update(secret.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Seal a secret so it can be recovered during the grace window.
    ///
    /// The AAD ties the ciphertext to this session and this generation, so a
    /// ciphertext lifted from another session (or an earlier rotation of the
    /// same one) fails to open rather than yielding the wrong token.
    pub fn seal_secret(&self, sid: &str, current_hash: &str, secret: &str) -> String {
        use aes_gcm::Aes256Gcm;
        use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};

        let cipher = Aes256Gcm::new((&self.seal_key).into());
        let mut nonce_bytes = [0u8; 12];
        rand::fill(&mut nonce_bytes);
        let nonce: &Nonce<Aes256Gcm> = (&nonce_bytes).into();

        let aad = seal_aad(sid, current_hash);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: secret.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .expect("AES-GCM encryption is infallible for valid keys");

        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        URL_SAFE_NO_PAD.encode(out)
    }

    /// Open a sealed secret and re-check it against the stored hash.
    ///
    /// The hash check is redundant with AEAD authentication but cheap, and it
    /// keeps a mis-sealed value from ever being handed back to a browser as a
    /// live credential.
    pub fn open_secret(&self, sid: &str, current_hash: &str, sealed: &str) -> Option<String> {
        use aes_gcm::Aes256Gcm;
        use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};

        let raw = URL_SAFE_NO_PAD.decode(sealed).ok()?;
        if raw.len() < 12 {
            return None;
        }
        let (nonce_bytes, ciphertext) = raw.split_at(12);

        let cipher = Aes256Gcm::new((&self.seal_key).into());
        let aad = seal_aad(sid, current_hash);
        let plaintext = cipher
            .decrypt(
                &Nonce::<Aes256Gcm>::try_from(nonce_bytes).ok()?,
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .ok()?;

        let secret = String::from_utf8(plaintext).ok()?;
        if constant_time_eq(&self.hash_secret(&secret), current_hash) {
            Some(secret)
        } else {
            None
        }
    }
}

fn seal_aad(sid: &str, current_hash: &str) -> String {
    format!("{SEAL_AAD_VERSION}|{sid}|{current_hash}")
}

fn decode_key(name: &str) -> [u8; 32] {
    let raw = std::env::var(name).unwrap_or_default();
    base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .unwrap_or_else(|| panic!("{name} must be base64 of exactly 32 bytes"))
}

/// A freshly minted refresh token and everything needed to store it.
pub struct NewToken {
    /// Cookie value: `{sid}.{secret}`.
    pub cookie_value: String,
    pub secret: String,
    pub hash: String,
}

pub fn generate_sid() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_token(keys: &SessionKeys, sid: &str) -> NewToken {
    let mut bytes = [0u8; SECRET_LEN];
    rand::fill(&mut bytes);
    let secret = URL_SAFE_NO_PAD.encode(bytes);
    let hash = keys.hash_secret(&secret);
    NewToken {
        cookie_value: format!("{sid}.{secret}"),
        secret,
        hash,
    }
}

/// Split a refresh cookie into `(sid, secret)`.
///
/// `sid` is base64url so it never contains `.`; splitting on the first
/// separator is unambiguous.
pub fn split_cookie(value: &str) -> Option<(&str, &str)> {
    let (sid, secret) = value.split_once('.')?;
    if sid.is_empty() || secret.is_empty() {
        None
    } else {
        Some((sid, secret))
    }
}

pub fn new_session(user_id: &str, current_hash: String, now: i64) -> RefreshSession {
    RefreshSession {
        user_id: user_id.to_string(),
        current_hash,
        previous_hash: None,
        previous_expires_at: None,
        iat: now,
        exp: now + REFRESH_SESSION_TTL_SECS,
        rotation: 0,
    }
}

pub fn grace_secs() -> i64 {
    REFRESH_GRACE_SECS
}

/// Compare two hex digests without leaking their contents through timing.
pub fn hashes_match(a: &str, b: &str) -> bool {
    constant_time_eq(a, b)
}

/// Short-circuit-free comparison for hex digests.
///
/// Only the length is compared in the ordinary way: digest lengths are fixed and
/// public, so leaking a mismatch there says nothing the caller did not know.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const GRACE: i64 = 10;

    fn keys() -> SessionKeys {
        SessionKeys::new([1u8; 32], [2u8; 32])
    }

    fn session() -> RefreshSession {
        RefreshSession {
            user_id: "user-1".into(),
            current_hash: "cur".into(),
            previous_hash: None,
            previous_expires_at: None,
            iat: NOW,
            exp: NOW + REFRESH_SESSION_TTL_SECS,
            rotation: 0,
        }
    }

    fn rotated_session() -> RefreshSession {
        RefreshSession {
            current_hash: "new".into(),
            previous_hash: Some("cur".into()),
            previous_expires_at: Some(NOW + GRACE),
            rotation: 1,
            ..session()
        }
    }

    // --- decide_rotation --------------------------------------------------

    #[test]
    fn current_token_rotates() {
        assert_eq!(
            decide_rotation(&session(), "cur", NOW),
            RotationOutcome::Rotated
        );
    }

    #[test]
    fn superseded_token_inside_grace_is_concurrency_not_theft() {
        assert_eq!(
            decide_rotation(&rotated_session(), "cur", NOW + GRACE - 1),
            RotationOutcome::Grace
        );
    }

    #[test]
    fn grace_window_includes_its_final_second() {
        assert_eq!(
            decide_rotation(&rotated_session(), "cur", NOW + GRACE),
            RotationOutcome::Grace
        );
    }

    #[test]
    fn superseded_token_after_grace_is_reuse() {
        assert_eq!(
            decide_rotation(&rotated_session(), "cur", NOW + GRACE + 1),
            RotationOutcome::ReuseDetected
        );
    }

    #[test]
    fn new_token_still_rotates_after_a_rotation() {
        assert_eq!(
            decide_rotation(&rotated_session(), "new", NOW + 1),
            RotationOutcome::Rotated
        );
    }

    #[test]
    fn unknown_secret_does_not_revoke_the_session() {
        // `sid` is not secret; revoking here would be a logout-anyone DoS.
        assert_eq!(
            decide_rotation(&rotated_session(), "garbage", NOW),
            RotationOutcome::UnknownHash
        );
    }

    #[test]
    fn expired_session_is_expired_even_with_the_right_secret() {
        let s = session();
        assert_eq!(decide_rotation(&s, "cur", s.exp), RotationOutcome::Expired);
        assert_eq!(
            decide_rotation(&s, "cur", s.exp + 1),
            RotationOutcome::Expired
        );
    }

    #[test]
    fn session_is_still_valid_one_second_before_exp() {
        let s = session();
        assert_eq!(
            decide_rotation(&s, "cur", s.exp - 1),
            RotationOutcome::Rotated
        );
    }

    #[test]
    fn missing_previous_expiry_is_treated_as_reuse() {
        let s = RefreshSession {
            previous_hash: Some("cur".into()),
            previous_expires_at: None,
            ..rotated_session()
        };
        assert_eq!(
            decide_rotation(&s, "cur", NOW),
            RotationOutcome::ReuseDetected
        );
    }

    // --- decide_revocation ------------------------------------------------

    #[test]
    fn current_secret_revokes() {
        assert_eq!(decide_revocation(&session(), "cur"), RevokeOutcome::Revoked);
    }

    #[test]
    fn superseded_secret_revokes_inside_and_outside_the_grace_window() {
        // Revocation is the fail-safe direction, so unlike rotation the grace
        // deadline does not gate it.
        let inside = rotated_session();
        assert_eq!(decide_revocation(&inside, "cur"), RevokeOutcome::Revoked);

        let elapsed = RefreshSession {
            previous_expires_at: Some(NOW - 1),
            ..rotated_session()
        };
        assert_eq!(decide_revocation(&elapsed, "cur"), RevokeOutcome::Revoked);
    }

    #[test]
    fn new_secret_still_revokes_after_a_rotation() {
        assert_eq!(
            decide_revocation(&rotated_session(), "new"),
            RevokeOutcome::Revoked
        );
    }

    #[test]
    fn unknown_secret_does_not_revoke_on_logout() {
        // The whole point: `sid` is not secret, so naming a session must not be
        // enough to end it.
        assert_eq!(
            decide_revocation(&session(), "garbage"),
            RevokeOutcome::HashMismatch
        );
        assert_eq!(
            decide_revocation(&rotated_session(), "garbage"),
            RevokeOutcome::HashMismatch
        );
    }

    #[test]
    fn a_session_without_a_previous_generation_only_matches_current() {
        let s = session();
        assert!(s.previous_hash.is_none());
        assert_eq!(decide_revocation(&s, ""), RevokeOutcome::HashMismatch);
    }

    // --- token material ---------------------------------------------------

    #[test]
    fn generated_tokens_are_unique_and_hash_consistently() {
        let k = keys();
        let sid = generate_sid();
        let a = generate_token(&k, &sid);
        let b = generate_token(&k, &sid);
        assert_ne!(a.secret, b.secret);
        assert_ne!(a.hash, b.hash);
        assert_eq!(k.hash_secret(&a.secret), a.hash);
        assert!(a.cookie_value.starts_with(&format!("{sid}.")));
    }

    #[test]
    fn hashing_depends_on_the_hmac_key() {
        let a = SessionKeys::new([1u8; 32], [2u8; 32]);
        let b = SessionKeys::new([9u8; 32], [2u8; 32]);
        assert_ne!(a.hash_secret("s"), b.hash_secret("s"));
    }

    #[test]
    fn splits_cookie_values() {
        assert_eq!(split_cookie("abc.def"), Some(("abc", "def")));
        assert_eq!(split_cookie("nodot"), None);
        assert_eq!(split_cookie(".def"), None);
        assert_eq!(split_cookie("abc."), None);
        assert_eq!(split_cookie(""), None);
    }

    #[test]
    fn new_sessions_expire_thirty_days_out() {
        let s = new_session("user-1", "h".into(), NOW);
        assert_eq!(s.exp, NOW + REFRESH_SESSION_TTL_SECS);
        assert_eq!(s.rotation, 0);
        assert!(s.previous_hash.is_none());
    }

    #[test]
    fn session_json_round_trips() {
        let s = rotated_session();
        let encoded = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<RefreshSession>(&encoded).unwrap(), s);
    }

    // --- sealing ----------------------------------------------------------

    #[test]
    fn seals_and_opens_a_secret() {
        let k = keys();
        let sealed = k.seal_secret("sid-1", &k.hash_secret("s3cret"), "s3cret");
        assert_eq!(
            k.open_secret("sid-1", &k.hash_secret("s3cret"), &sealed)
                .as_deref(),
            Some("s3cret")
        );
    }

    #[test]
    fn sealing_is_randomised() {
        let k = keys();
        let h = k.hash_secret("s3cret");
        assert_ne!(
            k.seal_secret("sid-1", &h, "s3cret"),
            k.seal_secret("sid-1", &h, "s3cret")
        );
    }

    #[test]
    fn a_ciphertext_cannot_be_moved_to_another_session() {
        let k = keys();
        let h = k.hash_secret("s3cret");
        let sealed = k.seal_secret("sid-1", &h, "s3cret");
        assert!(k.open_secret("sid-2", &h, &sealed).is_none());
    }

    #[test]
    fn a_ciphertext_cannot_be_replayed_across_generations() {
        let k = keys();
        let sealed = k.seal_secret("sid-1", &k.hash_secret("old"), "old");
        // Same session, but the stored hash has since moved on.
        assert!(
            k.open_secret("sid-1", &k.hash_secret("new"), &sealed)
                .is_none()
        );
    }

    #[test]
    fn opening_fails_under_a_different_seal_key() {
        let a = SessionKeys::new([1u8; 32], [2u8; 32]);
        let b = SessionKeys::new([1u8; 32], [8u8; 32]);
        let h = a.hash_secret("s3cret");
        let sealed = a.seal_secret("sid-1", &h, "s3cret");
        assert!(b.open_secret("sid-1", &h, &sealed).is_none());
    }

    #[test]
    fn opening_rejects_corrupt_input() {
        let k = keys();
        let h = k.hash_secret("s3cret");
        assert!(k.open_secret("sid-1", &h, "not-base64!!").is_none());
        assert!(k.open_secret("sid-1", &h, "").is_none());
        assert!(
            k.open_secret("sid-1", &h, &URL_SAFE_NO_PAD.encode([0u8; 8]))
                .is_none()
        );

        let mut sealed = URL_SAFE_NO_PAD
            .decode(k.seal_secret("sid-1", &h, "s3cret"))
            .unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(
            k.open_secret("sid-1", &h, &URL_SAFE_NO_PAD.encode(sealed))
                .is_none()
        );
    }

    // --- helpers ----------------------------------------------------------

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn status_codes_map_to_outcomes() {
        assert_eq!(status_to_outcome(0), Some(RotationOutcome::Rotated));
        assert_eq!(status_to_outcome(1), Some(RotationOutcome::Grace));
        assert_eq!(status_to_outcome(2), Some(RotationOutcome::ReuseDetected));
        assert_eq!(status_to_outcome(3), Some(RotationOutcome::Expired));
        assert_eq!(status_to_outcome(4), Some(RotationOutcome::NotFound));
        assert_eq!(status_to_outcome(5), Some(RotationOutcome::UnknownHash));
        assert_eq!(status_to_outcome(99), None);
    }

    #[test]
    fn revoke_status_codes_map_to_outcomes() {
        assert_eq!(revoke_status_to_outcome(0), Some(RevokeOutcome::Revoked));
        assert_eq!(revoke_status_to_outcome(1), Some(RevokeOutcome::NotFound));
        assert_eq!(
            revoke_status_to_outcome(2),
            Some(RevokeOutcome::HashMismatch)
        );
        assert_eq!(revoke_status_to_outcome(99), None);
    }
}
