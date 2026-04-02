use reqwest::Client;
use serde::Deserialize;

pub struct PulsoidOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    client: Client,
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
}

#[derive(Debug)]
pub enum OAuthError {
    Request(reqwest::Error),
    TokenEndpoint { status: u16, body: String },
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthError::Request(e) => write!(f, "HTTP request failed: {e}"),
            OAuthError::TokenEndpoint { status, body } => {
                write!(f, "token endpoint returned {status}: {body}")
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
    pub fn from_env() -> Self {
        let client_id =
            std::env::var("PULSOID_CLIENT_ID").expect("PULSOID_CLIENT_ID must be set");
        let client_secret =
            std::env::var("PULSOID_CLIENT_SECRET").expect("PULSOID_CLIENT_SECRET must be set");
        let redirect_uri =
            std::env::var("PULSOID_REDIRECT_URI").expect("PULSOID_REDIRECT_URI must be set");

        Self {
            client_id,
            client_secret,
            redirect_uri,
            client: Client::new(),
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
            return Err(OAuthError::TokenEndpoint { status, body });
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
            return Err(OAuthError::TokenEndpoint { status, body });
        }

        Ok(resp.json().await?)
    }
}
