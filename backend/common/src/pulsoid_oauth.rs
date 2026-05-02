use reqwest::Client;
use serde::Deserialize;

pub struct PulsoidOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Redirect URI used in the authorization-code flow. The refresh flow does
    /// not need this value, so `from_env_for_refresh` leaves it empty. Any
    /// caller that invokes `authorization_url` or `exchange_code` must
    /// therefore have constructed this config via `from_env_full`.
    pub redirect_uri: String,
    client: Client,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
}

pub enum OAuthError {
    Request(reqwest::Error),
    TokenEndpoint(TokenEndpointError),
}

pub struct TokenEndpointError {
    status: u16,
    body: String,
}

impl TokenEndpointError {
    pub(crate) fn new(status: u16, body: String) -> Self {
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
        }
    }
}

impl std::error::Error for OAuthError {}

impl From<reqwest::Error> for OAuthError {
    fn from(e: reqwest::Error) -> Self {
        OAuthError::Request(e)
    }
}

impl PulsoidOAuthConfig {
    /// Full config for the authorization-code flow (api-backend).
    /// `PULSOID_REDIRECT_URI` is required and validated at startup.
    pub fn from_env_full() -> Self {
        let client_id = std::env::var("PULSOID_CLIENT_ID").expect("PULSOID_CLIENT_ID must be set");
        let client_secret =
            std::env::var("PULSOID_CLIENT_SECRET").expect("PULSOID_CLIENT_SECRET must be set");
        let redirect_uri =
            std::env::var("PULSOID_REDIRECT_URI").expect("PULSOID_REDIRECT_URI must be set");

        Self {
            client_id,
            client_secret,
            redirect_uri,
            client: build_client(),
        }
    }

    /// Refresh-only config (pulsoid-refresher). Does not read or require
    /// `PULSOID_REDIRECT_URI` since the refresh endpoint does not accept it.
    /// Callers of this variant must not call `authorization_url` or
    /// `exchange_code`; `redirect_uri` is left empty.
    pub fn from_env_for_refresh() -> Self {
        let client_id = std::env::var("PULSOID_CLIENT_ID").expect("PULSOID_CLIENT_ID must be set");
        let client_secret =
            std::env::var("PULSOID_CLIENT_SECRET").expect("PULSOID_CLIENT_SECRET must be set");

        Self {
            client_id,
            client_secret,
            redirect_uri: String::new(),
            client: build_client(),
        }
    }

    pub fn authorization_url(&self, state: &str) -> String {
        format!(
            "https://pulsoid.net/oauth2/authorize?response_type=code&client_id={}&redirect_uri={}&scope=data:heart_rate:read&state={}",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&self.redirect_uri),
            urlencoding::encode(state),
        )
    }

    pub async fn exchange_code(&self, code: &str) -> Result<TokenResponse, OAuthError> {
        let resp = self
            .client
            .post("https://pulsoid.net/oauth2/token")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", &self.redirect_uri),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OAuthError::TokenEndpoint(TokenEndpointError::new(
                status, body,
            )));
        }

        Ok(resp.json().await?)
    }

    pub async fn refresh_token(&self, refresh_token: &str) -> Result<TokenResponse, OAuthError> {
        let resp = self
            .client
            .post("https://pulsoid.net/oauth2/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OAuthError::TokenEndpoint(TokenEndpointError::new(
                status, body,
            )));
        }

        Ok(resp.json().await?)
    }
}

fn build_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}
