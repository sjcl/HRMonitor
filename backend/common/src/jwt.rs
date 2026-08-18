//! Ed25519 (EdDSA) access tokens.
//!
//! api-backend signs a short-lived JWS after a successful Discord login or a
//! refresh-token rotation; api-backend and ws-gateway then verify it **locally**
//! on every request — no database round trip, no Redis round trip. That is the
//! whole point of the design: authentication is a signature check, and only
//! *authorisation* (group membership, heart-rate visibility) still reads the DB.
//!
//! Public keys are distributed as a standard JWK Set (RFC 7517) through the
//! `JWT_PUBLIC_KEYS` environment variable; the private seed lives only in
//! api-backend's `JWT_PRIVATE_KEY`. `kid` is used *solely* as a lookup key into
//! an in-memory map built at startup — never as a file path, URL, or DB query.
//!
//! `jsonwebtoken`'s `Validation` does not expose an injectable clock and does
//! not check `iat` at all, so every time-related rule is enforced here by hand
//! against a caller-supplied `now`. That also keeps the tests deterministic.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

/// Access-token lifetime. Also the upper bound enforced during verification, so
/// a token minted with a longer `exp` (by a compromised or misconfigured
/// issuer) is rejected rather than honoured.
pub const ACCESS_TOKEN_TTL_SECS: i64 = 30 * 60;

/// Tolerance for clock skew between the issuing and verifying hosts. Applied to
/// `exp` (accept slightly-expired) and to `iat` (accept slightly-future).
pub const CLOCK_SKEW_SECS: i64 = 30;

// Cheap structural limits, checked before any parsing or crypto, so a garbage
// payload cannot make us allocate or hash megabytes.
const MAX_TOKEN_LEN: usize = 4096;
const MAX_SEGMENT_LEN: usize = 2048;
const MAX_KID_LEN: usize = 64;

/// Claims carried by an access token.
///
/// Deliberately minimal: no email, no Discord tokens, no permissions, no
/// heart-rate data. Anything that can change (group membership, visibility)
/// must be read from the database at authorisation time, never trusted from
/// a token that stays valid for up to 30 minutes.
///
/// `deny_unknown_fields` blocks claim smuggling — a token carrying extra claims
/// is rejected outright rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub iss: String,
    pub aud: String,
    /// `users.id`
    pub sub: String,
    /// Redis refresh-session id, so logout can revoke without re-parsing cookies.
    pub sid: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

/// Header parsed with a strict allow-list.
///
/// Using `deny_unknown_fields` here is what rejects `crit` and `b64` (RFC 8725
/// §3.1/§3.4) along with `jku`, `jwk`, `x5u` and friends — all in one place,
/// rather than enumerating dangerous parameters one by one and hoping the list
/// stays complete. `jsonwebtoken`'s own `Header` type accepts all of them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictHeader {
    alg: String,
    kid: String,
    #[serde(default)]
    typ: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    /// Structurally invalid: wrong segment count, bad base64, bad JSON, or a
    /// segment/kid over the size limit.
    Malformed,
    /// `alg` was not exactly `EdDSA`.
    WrongAlg,
    /// `typ` was present but not `JWT`.
    BadTyp,
    /// `kid` is not in the configured key set.
    UnknownKid,
    BadSignature,
    Expired,
    /// `iat` is further in the future than the permitted clock skew.
    IatTooFarFuture,
    /// `iat` is after `exp`.
    IatAfterExp,
    /// `exp - iat` exceeds the maximum access-token lifetime.
    LifetimeTooLong,
    IssuerMismatch,
    AudienceMismatch,
    /// `sub`, `sid` or `jti` was empty.
    EmptyClaim,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            JwtError::Malformed => "malformed token",
            JwtError::WrongAlg => "unsupported alg",
            JwtError::BadTyp => "unsupported typ",
            JwtError::UnknownKid => "unknown kid",
            JwtError::BadSignature => "bad signature",
            JwtError::Expired => "token expired",
            JwtError::IatTooFarFuture => "iat too far in the future",
            JwtError::IatAfterExp => "iat after exp",
            JwtError::LifetimeTooLong => "token lifetime too long",
            JwtError::IssuerMismatch => "issuer mismatch",
            JwtError::AudienceMismatch => "audience mismatch",
            JwtError::EmptyClaim => "empty required claim",
        };
        f.write_str(s)
    }
}

impl std::error::Error for JwtError {}

// ---------------------------------------------------------------------------
// JWK Set parsing
// ---------------------------------------------------------------------------

/// A single JWK, parsed with a strict allow-list.
///
/// `deny_unknown_fields` plus the explicit `d` field means a JWK Set that
/// accidentally contains a *private* key is rejected loudly at startup. That
/// matters because `JWT_PUBLIC_KEYS` is handed to ws-gateway too, and
/// `jsonwebtoken`'s own untagged JWK enum would silently ignore a stray `d`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JwkDoc {
    kty: String,
    crv: String,
    alg: String,
    #[serde(rename = "use")]
    use_: String,
    kid: String,
    x: String,
    #[serde(default)]
    d: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JwkSetDoc {
    keys: Vec<JwkDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySetError {
    /// The value was not a JSON JWK Set, or a member had unexpected fields.
    Parse(String),
    Empty,
    EmptyKid,
    DuplicateKid(String),
    /// A JWK carried a private component.
    PrivateKeyMaterial(String),
    /// `kty`/`crv`/`alg`/`use` was not the expected constant.
    UnsupportedKey {
        kid: String,
        field: &'static str,
    },
    /// `x` was not 32 bytes of base64url.
    BadPublicKey(String),
}

impl std::fmt::Display for KeySetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySetError::Parse(e) => write!(f, "JWK Set is not valid: {e}"),
            KeySetError::Empty => f.write_str("JWK Set contains no keys"),
            KeySetError::EmptyKid => f.write_str("JWK Set contains a key with an empty kid"),
            KeySetError::DuplicateKid(k) => write!(f, "JWK Set contains duplicate kid {k:?}"),
            KeySetError::PrivateKeyMaterial(k) => write!(
                f,
                "JWK {k:?} contains private key material (`d`); \
                 JWT_PUBLIC_KEYS must only ever hold public keys"
            ),
            KeySetError::UnsupportedKey { kid, field } => {
                write!(f, "JWK {kid:?} has an unsupported `{field}`")
            }
            KeySetError::BadPublicKey(k) => {
                write!(
                    f,
                    "JWK {k:?} has an invalid `x` (expected 32 base64url bytes)"
                )
            }
        }
    }
}

impl std::error::Error for KeySetError {}

/// Parse a JWK Set into `kid -> (decoding key, raw 32-byte public key)`.
///
/// Every constraint is required rather than defaulted: a key that does not say
/// `OKP`/`Ed25519`/`EdDSA`/`sig` is a configuration mistake, and guessing at
/// intent here would be exactly the wrong instinct in an auth path.
fn parse_jwk_set(json: &str) -> Result<HashMap<String, ([u8; 32], DecodingKey)>, KeySetError> {
    let doc: JwkSetDoc =
        serde_json::from_str(json).map_err(|e| KeySetError::Parse(e.to_string()))?;

    if doc.keys.is_empty() {
        return Err(KeySetError::Empty);
    }

    let mut out = HashMap::with_capacity(doc.keys.len());
    for jwk in doc.keys {
        if jwk.kid.is_empty() {
            return Err(KeySetError::EmptyKid);
        }
        if jwk.d.is_some() {
            return Err(KeySetError::PrivateKeyMaterial(jwk.kid));
        }
        for (actual, expected, field) in [
            (jwk.kty.as_str(), "OKP", "kty"),
            (jwk.crv.as_str(), "Ed25519", "crv"),
            (jwk.alg.as_str(), "EdDSA", "alg"),
            (jwk.use_.as_str(), "sig", "use"),
        ] {
            if actual != expected {
                return Err(KeySetError::UnsupportedKey {
                    kid: jwk.kid,
                    field,
                });
            }
        }

        let raw = URL_SAFE_NO_PAD
            .decode(&jwk.x)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .ok_or_else(|| KeySetError::BadPublicKey(jwk.kid.clone()))?;

        let key = DecodingKey::from_ed_components(&jwk.x)
            .map_err(|_| KeySetError::BadPublicKey(jwk.kid.clone()))?;

        if out.insert(jwk.kid.clone(), (raw, key)).is_some() {
            return Err(KeySetError::DuplicateKid(jwk.kid));
        }
    }
    Ok(out)
}

/// Render a raw Ed25519 public key as a JWK Set entry.
pub fn public_key_to_jwk_json(kid: &str, public_key: &[u8; 32]) -> String {
    let x = URL_SAFE_NO_PAD.encode(public_key);
    format!(r#"{{"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","kid":"{kid}","x":"{x}"}}"#)
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

pub struct JwtVerifier {
    keys: HashMap<String, ([u8; 32], DecodingKey)>,
    issuer: String,
    audience: String,
    leeway_secs: i64,
    max_lifetime_secs: i64,
    /// Signature-and-shape validation only. Every temporal rule, plus `iss` and
    /// `aud`, is checked by hand below so the clock can be injected.
    validation: Validation,
}

impl std::fmt::Debug for JwtVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtVerifier")
            .field("kids", &self.keys.keys().collect::<Vec<_>>())
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .finish()
    }
}

impl JwtVerifier {
    pub fn new(jwk_set_json: &str, issuer: &str, audience: &str) -> Result<Self, KeySetError> {
        let keys = parse_jwk_set(jwk_set_json)?;

        let mut validation = Validation::new(Algorithm::EdDSA);
        // We own every time check (see `verify_at`) so the clock is injectable
        // and `exp` uses `>=` per RFC 7519 §4.1.4.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();

        Ok(Self {
            keys,
            issuer: issuer.to_string(),
            audience: audience.to_string(),
            leeway_secs: CLOCK_SKEW_SECS,
            max_lifetime_secs: ACCESS_TOKEN_TTL_SECS,
            validation,
        })
    }

    /// Build a verifier from the environment.
    ///
    /// Panics on misconfiguration: a service that cannot verify tokens must not
    /// come up and start serving 401s that look like an application bug.
    pub fn from_env() -> Self {
        let jwks = require_env("JWT_PUBLIC_KEYS");
        let issuer = require_env("JWT_ISSUER");
        let audience = require_env("JWT_AUDIENCE");
        match Self::new(&jwks, &issuer, &audience) {
            Ok(v) => v,
            Err(e) => panic!("JWT_PUBLIC_KEYS is invalid: {e}"),
        }
    }

    /// Raw public key for `kid`, used by api-backend to assert at startup that
    /// its private key matches the advertised active JWK.
    pub fn public_key(&self, kid: &str) -> Option<&[u8; 32]> {
        self.keys.get(kid).map(|(raw, _)| raw)
    }

    pub fn verify(&self, token: &str) -> Result<Claims, JwtError> {
        self.verify_at(token, crate::time::unix_now_secs())
    }

    /// Verify `token` as of `now` (unix seconds).
    ///
    /// Order matters: cheap structural limits, then `alg` pinning, then key
    /// lookup, then the signature. Claims are only trusted once the signature
    /// has been checked.
    pub fn verify_at(&self, token: &str, now: i64) -> Result<Claims, JwtError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(JwtError::Malformed);
        }

        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(sig_b64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(JwtError::Malformed);
        };
        if header_b64.len() > MAX_SEGMENT_LEN
            || payload_b64.len() > MAX_SEGMENT_LEN
            || sig_b64.len() > MAX_SEGMENT_LEN
        {
            return Err(JwtError::Malformed);
        }

        let header_bytes = URL_SAFE_NO_PAD
            .decode(header_b64)
            .map_err(|_| JwtError::Malformed)?;
        let header: StrictHeader =
            serde_json::from_slice(&header_bytes).map_err(|_| JwtError::Malformed)?;

        // Pin `alg` before touching `kid`: this is what defeats alg-confusion
        // (`none`, HS256-signed-with-the-public-key) regardless of the key set.
        if header.alg != "EdDSA" {
            return Err(JwtError::WrongAlg);
        }
        if let Some(typ) = &header.typ
            && !typ.eq_ignore_ascii_case("jwt")
        {
            return Err(JwtError::BadTyp);
        }
        if header.kid.len() > MAX_KID_LEN {
            return Err(JwtError::Malformed);
        }

        // `kid` is only ever a HashMap lookup — never a path, URL, or query.
        let (_, key) = self.keys.get(&header.kid).ok_or(JwtError::UnknownKid)?;

        let data = jsonwebtoken::decode::<Claims>(token, key, &self.validation).map_err(|e| {
            use jsonwebtoken::errors::ErrorKind;
            match e.kind() {
                ErrorKind::InvalidSignature => JwtError::BadSignature,
                _ => JwtError::Malformed,
            }
        })?;
        let claims = data.claims;

        if claims.iss != self.issuer {
            return Err(JwtError::IssuerMismatch);
        }
        if claims.aud != self.audience {
            return Err(JwtError::AudienceMismatch);
        }
        if claims.sub.is_empty() || claims.sid.is_empty() || claims.jti.is_empty() {
            return Err(JwtError::EmptyClaim);
        }

        // RFC 7519 §4.1.4: the token must not be accepted *on or after* `exp`.
        if now >= claims.exp.saturating_add(self.leeway_secs) {
            return Err(JwtError::Expired);
        }
        if claims.iat > now.saturating_add(self.leeway_secs) {
            return Err(JwtError::IatTooFarFuture);
        }
        if claims.iat > claims.exp {
            return Err(JwtError::IatAfterExp);
        }
        if claims.exp.saturating_sub(claims.iat)
            > self.max_lifetime_secs.saturating_add(self.leeway_secs)
        {
            return Err(JwtError::LifetimeTooLong);
        }

        Ok(claims)
    }
}

fn require_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => panic!("{name} must be set"),
    }
}

// ---------------------------------------------------------------------------
// Issuing (api-backend only)
// ---------------------------------------------------------------------------

#[cfg(feature = "jwt-issue")]
mod issuing {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use ed25519_dalek::SigningKey;
    use jsonwebtoken::{EncodingKey, Header};

    /// PKCS#8 v1 prefix for an Ed25519 private key (RFC 8410). The encoding is
    /// fixed-length, so a raw 32-byte seed becomes a DER document by
    /// concatenation — `jsonwebtoken` only accepts DER/PEM here.
    const PKCS8_ED25519_PREFIX: [u8; 16] = [
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];

    fn seed_to_pkcs8_der(seed: &[u8; 32]) -> Vec<u8> {
        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&PKCS8_ED25519_PREFIX);
        der.extend_from_slice(seed);
        der
    }

    /// Derive the public key for a raw Ed25519 seed.
    pub fn public_key_for_seed(seed: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(seed).verifying_key().to_bytes()
    }

    /// Generate a fresh Ed25519 seed.
    pub fn generate_seed() -> [u8; 32] {
        let mut seed = [0u8; 32];
        rand::fill(&mut seed);
        seed
    }

    pub struct JwtSigner {
        key: EncodingKey,
        kid: String,
        issuer: String,
        audience: String,
        ttl_secs: i64,
        public_key: [u8; 32],
    }

    impl std::fmt::Debug for JwtSigner {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            // Never render the key material.
            f.debug_struct("JwtSigner")
                .field("kid", &self.kid)
                .field("issuer", &self.issuer)
                .field("audience", &self.audience)
                .finish()
        }
    }

    impl JwtSigner {
        pub fn new(seed: &[u8; 32], kid: &str, issuer: &str, audience: &str) -> Self {
            Self {
                key: EncodingKey::from_ed_der(&seed_to_pkcs8_der(seed)),
                kid: kid.to_string(),
                issuer: issuer.to_string(),
                audience: audience.to_string(),
                ttl_secs: ACCESS_TOKEN_TTL_SECS,
                public_key: public_key_for_seed(seed),
            }
        }

        /// Build the signer from the environment and prove it agrees with the
        /// published key set.
        ///
        /// Rotating `JWT_ACTIVE_KID` without also rotating `JWT_PRIVATE_KEY`
        /// would mint tokens nobody can verify. Failing at startup turns that
        /// into an obvious deploy failure instead of a site-wide 401 storm.
        pub fn from_env(verifier: &JwtVerifier) -> Self {
            let kid = require_env("JWT_ACTIVE_KID");
            let seed_b64 = require_env("JWT_PRIVATE_KEY");
            let seed = B64
                .decode(seed_b64.trim())
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                .unwrap_or_else(|| panic!("JWT_PRIVATE_KEY must be base64 of exactly 32 bytes"));

            let signer = Self::new(
                &seed,
                &kid,
                &require_env("JWT_ISSUER"),
                &require_env("JWT_AUDIENCE"),
            );

            match verifier.public_key(&kid) {
                None => panic!("JWT_ACTIVE_KID {kid:?} has no matching JWK in JWT_PUBLIC_KEYS"),
                Some(published) if published != &signer.public_key => panic!(
                    "JWT_PRIVATE_KEY does not match the JWT_PUBLIC_KEYS entry for kid {kid:?}; \
                     rotate JWT_PRIVATE_KEY and JWT_ACTIVE_KID together"
                ),
                Some(_) => {}
            }
            signer
        }

        pub fn public_key(&self) -> &[u8; 32] {
            &self.public_key
        }

        pub fn ttl_secs(&self) -> i64 {
            self.ttl_secs
        }

        /// Mint an access token for `sub`, bound to refresh session `sid`.
        pub fn sign(&self, sub: &str, sid: &str, now: i64) -> Result<(String, Claims), JwtError> {
            let mut jti = [0u8; 16];
            rand::fill(&mut jti);

            let claims = Claims {
                iss: self.issuer.clone(),
                aud: self.audience.clone(),
                sub: sub.to_string(),
                sid: sid.to_string(),
                iat: now,
                exp: now + self.ttl_secs,
                jti: URL_SAFE_NO_PAD.encode(jti),
            };

            let mut header = Header::new(Algorithm::EdDSA);
            header.kid = Some(self.kid.clone());
            header.typ = Some("JWT".to_string());

            let token = jsonwebtoken::encode(&header, &claims, &self.key)
                .map_err(|_| JwtError::Malformed)?;
            Ok((token, claims))
        }
    }
}

#[cfg(feature = "jwt-issue")]
pub use issuing::{JwtSigner, generate_seed, public_key_for_seed};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "jwt-issue"))]
mod tests {
    use super::*;

    const ISS: &str = "https://hr.example.com";
    const AUD: &str = "hrmonitor-web";
    const KID: &str = "k1";
    const NOW: i64 = 1_800_000_000;

    /// Fixed seed so failures are reproducible.
    const SEED: [u8; 32] = [7u8; 32];

    fn signer() -> JwtSigner {
        JwtSigner::new(&SEED, KID, ISS, AUD)
    }

    fn jwk_set() -> String {
        let pk = public_key_for_seed(&SEED);
        format!(r#"{{"keys":[{}]}}"#, public_key_to_jwk_json(KID, &pk))
    }

    fn verifier() -> JwtVerifier {
        JwtVerifier::new(&jwk_set(), ISS, AUD).unwrap()
    }

    fn b64url(v: &serde_json::Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
    }

    /// Re-sign an arbitrary header/payload pair with the real key, so tests can
    /// probe validation rules rather than just tripping the signature check.
    fn forge(header: serde_json::Value, payload: serde_json::Value) -> String {
        use ed25519_dalek::{Signer, SigningKey};
        let signing_input = format!("{}.{}", b64url(&header), b64url(&payload));
        let sig = SigningKey::from_bytes(&SEED).sign(signing_input.as_bytes());
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }

    fn good_payload() -> serde_json::Value {
        serde_json::json!({
            "iss": ISS, "aud": AUD, "sub": "user-1", "sid": "sess-1",
            "iat": NOW, "exp": NOW + ACCESS_TOKEN_TTL_SECS, "jti": "j1",
        })
    }

    fn good_header() -> serde_json::Value {
        serde_json::json!({ "typ": "JWT", "alg": "EdDSA", "kid": KID })
    }

    // --- happy path -------------------------------------------------------

    #[test]
    fn round_trip() {
        let (token, issued) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let claims = verifier().verify_at(&token, NOW).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.sid, "sess-1");
        assert_eq!(claims.iss, ISS);
        assert_eq!(claims.aud, AUD);
        assert_eq!(claims.exp, NOW + ACCESS_TOKEN_TTL_SECS);
        assert_eq!(claims.jti, issued.jti);
        assert!(!claims.jti.is_empty());
    }

    #[test]
    fn each_token_gets_a_unique_jti() {
        let s = signer();
        let (_, a) = s.sign("user-1", "sess-1", NOW).unwrap();
        let (_, b) = s.sign("user-1", "sess-1", NOW).unwrap();
        assert_ne!(a.jti, b.jti);
    }

    // --- signature / key --------------------------------------------------

    /// Known-answer test for the seed -> public-key derivation (RFC 8032 §7.1,
    /// TEST 1). `round_trip` only proves that our derivation agrees with
    /// whatever `jsonwebtoken` signs with; this pins it to the standard, so
    /// swapping either crate cannot silently move the derivation.
    ///
    /// seed   9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
    /// public d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
    #[test]
    fn public_key_matches_rfc8032_vector() {
        const RFC8032_SEED: [u8; 32] = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        const RFC8032_PUBLIC: [u8; 32] = [
            0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64,
            0x07, 0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68,
            0xf7, 0x07, 0x51, 0x1a,
        ];
        assert_eq!(public_key_for_seed(&RFC8032_SEED), RFC8032_PUBLIC);
    }

    #[test]
    fn rejects_tampered_signature() {
        // Flip a bit in the decoded signature and re-encode, so the segment is
        // still a well-formed 64-byte signature that simply does not verify.
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let mut sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        assert_eq!(sig.len(), 64);
        sig[0] ^= 0x01;
        let tampered = format!("{}.{}.{}", parts[0], parts[1], URL_SAFE_NO_PAD.encode(&sig));
        assert_eq!(
            verifier().verify_at(&tampered, NOW),
            Err(JwtError::BadSignature)
        );
    }

    #[test]
    fn rejects_signature_of_the_wrong_length() {
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let short = URL_SAFE_NO_PAD.encode([0u8; 32]);
        let tampered = format!("{}.{}.{}", parts[0], parts[1], short);
        assert!(verifier().verify_at(&tampered, NOW).is_err());
    }

    #[test]
    fn rejects_tampered_payload() {
        // Signed for user-1, payload rewritten to user-2.
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let mut payload = good_payload();
        payload["sub"] = serde_json::json!("user-2");
        let forged = format!("{}.{}.{}", parts[0], b64url(&payload), parts[2]);
        assert_eq!(
            verifier().verify_at(&forged, NOW),
            Err(JwtError::BadSignature)
        );
    }

    #[test]
    fn rejects_unknown_kid() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "EdDSA", "kid": "nope" }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::UnknownKid));
    }

    #[test]
    fn rejects_token_signed_by_a_different_key() {
        let other = JwtSigner::new(&[9u8; 32], KID, ISS, AUD);
        let (token, _) = other.sign("user-1", "sess-1", NOW).unwrap();
        assert_eq!(
            verifier().verify_at(&token, NOW),
            Err(JwtError::BadSignature)
        );
    }

    // --- alg / typ / header -----------------------------------------------

    #[test]
    fn rejects_alg_none() {
        let header = serde_json::json!({ "typ": "JWT", "alg": "none", "kid": KID });
        let token = format!("{}.{}.", b64url(&header), b64url(&good_payload()));
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::WrongAlg));
    }

    #[test]
    fn rejects_alg_hs256() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "HS256", "kid": KID }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::WrongAlg));
    }

    #[test]
    fn rejects_empty_alg() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "", "kid": KID }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::WrongAlg));
    }

    #[test]
    fn rejects_crit_header() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "EdDSA", "kid": KID, "crit": ["exp"] }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::Malformed));
    }

    #[test]
    fn rejects_b64_header() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "EdDSA", "kid": KID, "b64": false }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::Malformed));
    }

    #[test]
    fn rejects_embedded_jwk_header() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "EdDSA", "kid": KID, "jwk": {} }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::Malformed));
    }

    #[test]
    fn rejects_missing_kid() {
        let token = forge(
            serde_json::json!({ "typ": "JWT", "alg": "EdDSA" }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::Malformed));
    }

    #[test]
    fn rejects_bad_typ() {
        let token = forge(
            serde_json::json!({ "typ": "at+jwt", "alg": "EdDSA", "kid": KID }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::BadTyp));
    }

    #[test]
    fn accepts_absent_typ() {
        let token = forge(
            serde_json::json!({ "alg": "EdDSA", "kid": KID }),
            good_payload(),
        );
        assert!(verifier().verify_at(&token, NOW).is_ok());
    }

    // --- time -------------------------------------------------------------

    #[test]
    fn accepts_just_before_expiry_plus_leeway() {
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let last_ok = NOW + ACCESS_TOKEN_TTL_SECS + CLOCK_SKEW_SECS - 1;
        assert!(verifier().verify_at(&token, last_ok).is_ok());
    }

    #[test]
    fn rejects_exactly_at_expiry_plus_leeway() {
        // RFC 7519 §4.1.4 — `exp` is exclusive, so the boundary itself fails.
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let boundary = NOW + ACCESS_TOKEN_TTL_SECS + CLOCK_SKEW_SECS;
        assert_eq!(
            verifier().verify_at(&token, boundary),
            Err(JwtError::Expired)
        );
    }

    #[test]
    fn rejects_long_expired_token() {
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        assert_eq!(
            verifier().verify_at(&token, NOW + 86_400),
            Err(JwtError::Expired)
        );
    }

    #[test]
    fn accepts_iat_within_leeway() {
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        assert!(verifier().verify_at(&token, NOW - CLOCK_SKEW_SECS).is_ok());
    }

    #[test]
    fn rejects_iat_too_far_in_future() {
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        assert_eq!(
            verifier().verify_at(&token, NOW - CLOCK_SKEW_SECS - 1),
            Err(JwtError::IatTooFarFuture)
        );
    }

    #[test]
    fn rejects_iat_after_exp() {
        let mut payload = good_payload();
        payload["iat"] = serde_json::json!(NOW + 10);
        payload["exp"] = serde_json::json!(NOW + 5);
        let token = forge(good_header(), payload);
        assert_eq!(
            verifier().verify_at(&token, NOW),
            Err(JwtError::IatAfterExp)
        );
    }

    #[test]
    fn rejects_lifetime_beyond_maximum() {
        // A "valid" token minted with a 24h lifetime must still be refused.
        let mut payload = good_payload();
        payload["exp"] = serde_json::json!(NOW + 86_400);
        let token = forge(good_header(), payload);
        assert_eq!(
            verifier().verify_at(&token, NOW),
            Err(JwtError::LifetimeTooLong)
        );
    }

    #[test]
    fn accepts_lifetime_at_maximum_plus_leeway() {
        let mut payload = good_payload();
        payload["exp"] = serde_json::json!(NOW + ACCESS_TOKEN_TTL_SECS + CLOCK_SKEW_SECS);
        let token = forge(good_header(), payload);
        assert!(verifier().verify_at(&token, NOW).is_ok());
    }

    // --- claims -----------------------------------------------------------

    #[test]
    fn rejects_issuer_mismatch() {
        let mut payload = good_payload();
        payload["iss"] = serde_json::json!("https://evil.example");
        let token = forge(good_header(), payload);
        assert_eq!(
            verifier().verify_at(&token, NOW),
            Err(JwtError::IssuerMismatch)
        );
    }

    #[test]
    fn rejects_audience_mismatch() {
        let mut payload = good_payload();
        payload["aud"] = serde_json::json!("some-other-app");
        let token = forge(good_header(), payload);
        assert_eq!(
            verifier().verify_at(&token, NOW),
            Err(JwtError::AudienceMismatch)
        );
    }

    #[test]
    fn rejects_empty_sub_sid_or_jti() {
        for field in ["sub", "sid", "jti"] {
            let mut payload = good_payload();
            payload[field] = serde_json::json!("");
            let token = forge(good_header(), payload);
            assert_eq!(
                verifier().verify_at(&token, NOW),
                Err(JwtError::EmptyClaim),
                "empty {field} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_missing_claim() {
        for field in ["iss", "aud", "sub", "sid", "iat", "exp", "jti"] {
            let mut payload = good_payload();
            payload.as_object_mut().unwrap().remove(field);
            let token = forge(good_header(), payload);
            assert_eq!(
                verifier().verify_at(&token, NOW),
                Err(JwtError::Malformed),
                "missing {field} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_smuggled_extra_claim() {
        let mut payload = good_payload();
        payload["role"] = serde_json::json!("admin");
        let token = forge(good_header(), payload);
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::Malformed));
    }

    // --- structure --------------------------------------------------------

    #[test]
    fn rejects_wrong_segment_count() {
        let v = verifier();
        assert_eq!(v.verify_at("a.b", NOW), Err(JwtError::Malformed));
        assert_eq!(v.verify_at("a.b.c.d", NOW), Err(JwtError::Malformed));
        assert_eq!(v.verify_at("", NOW), Err(JwtError::Malformed));
    }

    #[test]
    fn rejects_invalid_base64_and_json() {
        let v = verifier();
        assert_eq!(v.verify_at("!!!.!!!.!!!", NOW), Err(JwtError::Malformed));
        let not_json = URL_SAFE_NO_PAD.encode("nonsense");
        assert_eq!(
            v.verify_at(&format!("{not_json}.{not_json}.{not_json}"), NOW),
            Err(JwtError::Malformed)
        );
    }

    #[test]
    fn rejects_oversized_token() {
        let (token, _) = signer().sign("user-1", "sess-1", NOW).unwrap();
        let padded = format!("{token}{}", "A".repeat(MAX_TOKEN_LEN));
        assert_eq!(verifier().verify_at(&padded, NOW), Err(JwtError::Malformed));
    }

    #[test]
    fn rejects_oversized_kid() {
        let token = forge(
            serde_json::json!({ "alg": "EdDSA", "kid": "k".repeat(MAX_KID_LEN + 1) }),
            good_payload(),
        );
        assert_eq!(verifier().verify_at(&token, NOW), Err(JwtError::Malformed));
    }

    // --- JWK Set ----------------------------------------------------------

    #[test]
    fn parses_a_valid_jwk_set() {
        let v = verifier();
        assert_eq!(v.public_key(KID), Some(&public_key_for_seed(&SEED)));
        assert_eq!(v.public_key("other"), None);
    }

    #[test]
    fn rejects_empty_jwk_set() {
        assert_eq!(
            JwtVerifier::new(r#"{"keys":[]}"#, ISS, AUD).unwrap_err(),
            KeySetError::Empty
        );
    }

    #[test]
    fn rejects_empty_kid() {
        let pk = public_key_for_seed(&SEED);
        let jwks = format!(r#"{{"keys":[{}]}}"#, public_key_to_jwk_json("", &pk));
        assert_eq!(
            JwtVerifier::new(&jwks, ISS, AUD).unwrap_err(),
            KeySetError::EmptyKid
        );
    }

    #[test]
    fn rejects_duplicate_kid() {
        let a = public_key_to_jwk_json(KID, &public_key_for_seed(&SEED));
        let b = public_key_to_jwk_json(KID, &public_key_for_seed(&[9u8; 32]));
        let jwks = format!(r#"{{"keys":[{a},{b}]}}"#);
        assert_eq!(
            JwtVerifier::new(&jwks, ISS, AUD).unwrap_err(),
            KeySetError::DuplicateKid(KID.to_string())
        );
    }

    #[test]
    fn rejects_jwk_carrying_private_key_material() {
        let x = URL_SAFE_NO_PAD.encode(public_key_for_seed(&SEED));
        let d = URL_SAFE_NO_PAD.encode(SEED);
        let jwks = format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","kid":"{KID}","x":"{x}","d":"{d}"}}]}}"#
        );
        assert_eq!(
            JwtVerifier::new(&jwks, ISS, AUD).unwrap_err(),
            KeySetError::PrivateKeyMaterial(KID.to_string())
        );
    }

    #[test]
    fn rejects_wrong_key_parameters() {
        let x = URL_SAFE_NO_PAD.encode(public_key_for_seed(&SEED));
        for (kty, crv, alg, use_, field) in [
            ("EC", "Ed25519", "EdDSA", "sig", "kty"),
            ("OKP", "X25519", "EdDSA", "sig", "crv"),
            ("OKP", "Ed25519", "ES256", "sig", "alg"),
            ("OKP", "Ed25519", "EdDSA", "enc", "use"),
        ] {
            let jwks = format!(
                r#"{{"keys":[{{"kty":"{kty}","crv":"{crv}","alg":"{alg}","use":"{use_}","kid":"{KID}","x":"{x}"}}]}}"#
            );
            assert_eq!(
                JwtVerifier::new(&jwks, ISS, AUD).unwrap_err(),
                KeySetError::UnsupportedKey {
                    kid: KID.to_string(),
                    field
                }
            );
        }
    }

    #[test]
    fn rejects_bad_public_key_bytes() {
        let x = URL_SAFE_NO_PAD.encode([1u8; 31]);
        let jwks = format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","kid":"{KID}","x":"{x}"}}]}}"#
        );
        assert_eq!(
            JwtVerifier::new(&jwks, ISS, AUD).unwrap_err(),
            KeySetError::BadPublicKey(KID.to_string())
        );
    }

    #[test]
    fn rejects_unknown_jwk_fields() {
        let x = URL_SAFE_NO_PAD.encode(public_key_for_seed(&SEED));
        let jwks = format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","kid":"{KID}","x":"{x}","x5u":"https://evil.example/k"}}]}}"#
        );
        assert!(matches!(
            JwtVerifier::new(&jwks, ISS, AUD).unwrap_err(),
            KeySetError::Parse(_)
        ));
    }

    #[test]
    fn multiple_kids_verify_during_rotation() {
        // Old and new key both published: tokens from either must verify.
        let old_seed = [3u8; 32];
        let jwks = format!(
            r#"{{"keys":[{},{}]}}"#,
            public_key_to_jwk_json("old", &public_key_for_seed(&old_seed)),
            public_key_to_jwk_json("new", &public_key_for_seed(&SEED)),
        );
        let v = JwtVerifier::new(&jwks, ISS, AUD).unwrap();

        let (old_token, _) = JwtSigner::new(&old_seed, "old", ISS, AUD)
            .sign("user-1", "sess-1", NOW)
            .unwrap();
        let (new_token, _) = JwtSigner::new(&SEED, "new", ISS, AUD)
            .sign("user-1", "sess-1", NOW)
            .unwrap();

        assert!(v.verify_at(&old_token, NOW).is_ok());
        assert!(v.verify_at(&new_token, NOW).is_ok());
    }
}
