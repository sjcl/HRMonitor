use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::collections::HashMap;

const NONCE_LEN: usize = 12;

pub struct TokenEncryption {
    keys: HashMap<u32, [u8; 32]>,
    current_version: u32,
}

impl TokenEncryption {
    /// Load encryption key(s) from Docker secret or environment variable.
    /// Priority: /run/secrets/pulsoid_token_key > TOKEN_ENCRYPTION_KEY
    pub fn from_env() -> Self {
        let key_b64 = std::fs::read_to_string("/run/secrets/pulsoid_token_key")
            .map(|s| s.trim().to_string())
            .or_else(|_| std::env::var("TOKEN_ENCRYPTION_KEY"))
            .expect("TOKEN_ENCRYPTION_KEY must be set (or provide /run/secrets/pulsoid_token_key)");

        let key_bytes = BASE64
            .decode(&key_b64)
            .expect("TOKEN_ENCRYPTION_KEY must be valid base64");

        assert_eq!(
            key_bytes.len(),
            32,
            "TOKEN_ENCRYPTION_KEY must be exactly 32 bytes (256 bits)"
        );

        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);

        let mut keys = HashMap::new();
        keys.insert(1, key);

        Self {
            keys,
            current_version: 1,
        }
    }

    pub fn current_version(&self) -> u32 {
        self.current_version
    }

    /// Encrypt plaintext with the current key.
    /// Returns (nonce || ciphertext || tag, key_version).
    pub fn encrypt(&self, plaintext: &str) -> (Vec<u8>, u32) {
        let key = self
            .keys
            .get(&self.current_version)
            .expect("current_version key must exist");

        let cipher = Aes256Gcm::new(key.into());
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .expect("encryption must not fail");

        let mut result = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);

        (result, self.current_version)
    }

    /// Decrypt ciphertext (nonce || ciphertext || tag) with the specified key version.
    pub fn decrypt(&self, data: &[u8], key_version: u32) -> Result<String, DecryptError> {
        if data.len() < NONCE_LEN + 16 {
            return Err(DecryptError::InvalidData);
        }

        let key = self
            .keys
            .get(&key_version)
            .ok_or(DecryptError::UnknownKeyVersion(key_version))?;

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
        let cipher = Aes256Gcm::new(key.into());

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| DecryptError::DecryptionFailed)?;

        String::from_utf8(plaintext).map_err(|_| DecryptError::InvalidUtf8)
    }
}

#[derive(Debug)]
pub enum DecryptError {
    InvalidData,
    UnknownKeyVersion(u32),
    DecryptionFailed,
    InvalidUtf8,
}

impl std::fmt::Display for DecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecryptError::InvalidData => write!(f, "encrypted data too short"),
            DecryptError::UnknownKeyVersion(v) => write!(f, "unknown key version: {v}"),
            DecryptError::DecryptionFailed => write!(f, "decryption failed"),
            DecryptError::InvalidUtf8 => write!(f, "decrypted data is not valid UTF-8"),
        }
    }
}

impl std::error::Error for DecryptError {}
