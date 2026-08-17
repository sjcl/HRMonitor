//! Provider-agnostic OAuth error types.
//!
//! Extracted from `pulsoid_oauth` so the Discord login flow can share them.
//! The `Debug`/`Display` impls deliberately redact the response body: token
//! endpoint errors routinely echo back parts of the request, and this type is
//! logged on failure paths.

use reqwest::Client;

pub enum OAuthError {
    Request(reqwest::Error),
    TokenEndpoint(TokenEndpointError),
    /// The provider returned 2xx but the payload was not what we require.
    UnexpectedResponse(&'static str),
}

pub struct TokenEndpointError {
    status: u16,
    body: String,
}

impl TokenEndpointError {
    pub fn new(status: u16, body: String) -> Self {
        Self { status, body }
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    /// Check whether the JSON response body contains a specific OAuth `error` code.
    pub fn has_oauth_error(&self, code: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(|s| s == code))
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthError::Request(e) => f.debug_tuple("Request").field(e).finish(),
            OAuthError::TokenEndpoint(e) => f
                .debug_struct("TokenEndpoint")
                .field("status", &e.status)
                .field("body", &"<redacted>")
                .finish(),
            OAuthError::UnexpectedResponse(what) => {
                f.debug_tuple("UnexpectedResponse").field(what).finish()
            }
        }
    }
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthError::Request(e) => write!(f, "HTTP request failed: {e}"),
            OAuthError::TokenEndpoint(e) => {
                write!(f, "token endpoint returned HTTP {}", e.status)
            }
            OAuthError::UnexpectedResponse(what) => {
                write!(f, "unexpected OAuth response: {what}")
            }
        }
    }
}

impl std::error::Error for OAuthError {}

impl From<reqwest::Error> for OAuthError {
    fn from(e: reqwest::Error) -> Self {
        OAuthError::Request(e)
    }
}

pub(crate) fn build_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}
