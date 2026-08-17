//! Discord Authorization Code flow with PKCE.
//!
//! Shaped after [`crate::pulsoid_oauth`], with two deliberate differences:
//!
//! * PKCE (S256) is mandatory here — this flow authenticates a person, not an
//!   integration, so a leaked authorization code must be useless on its own.
//! * The scope is `identify` only. We never request `email`, and we never
//!   persist Discord's access or refresh tokens: after the single `identify`
//!   call at login there is nothing left to ask Discord for.

use reqwest::Client;
use serde::Deserialize;

use crate::oauth_error::{OAuthError, TokenEndpointError, build_client};

const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";
const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const USER_URL: &str = "https://discord.com/api/users/@me";
const SCOPE: &str = "identify";

pub struct DiscordOAuthConfig {
    pub client_id: String,
    client_secret: String,
    pub redirect_uri: String,
    client: Client,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// The subset of Discord's user object we use. `identify` returns more; we
/// deserialise only what is needed so nothing extra can drift into the DB.
#[derive(Debug, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    pub username: String,
    /// Discord's newer display name. `None` for accounts that never set one.
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

impl DiscordUser {
    /// Name to seed `users.display_name` with on first login.
    pub fn display_name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }

    /// CDN URL for the user's avatar, if they have one.
    pub fn avatar_url(&self) -> Option<String> {
        let hash = self.avatar.as_ref()?;
        let ext = if hash.starts_with("a_") { "gif" } else { "png" };
        Some(format!(
            "https://cdn.discordapp.com/avatars/{}/{hash}.{ext}",
            self.id
        ))
    }
}

impl std::fmt::Debug for DiscordOAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordOAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

impl DiscordOAuthConfig {
    pub fn from_env() -> Self {
        Self {
            client_id: require_env("DISCORD_CLIENT_ID"),
            client_secret: require_env("DISCORD_CLIENT_SECRET"),
            redirect_uri: require_env("DISCORD_REDIRECT_URI"),
            client: build_client(),
        }
    }

    pub fn authorization_url(&self, state: &str, code_challenge: &str) -> String {
        format!(
            "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}\
             &scope={}&state={}&code_challenge={}&code_challenge_method=S256&prompt=none",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&self.redirect_uri),
            urlencoding::encode(SCOPE),
            urlencoding::encode(state),
            urlencoding::encode(code_challenge),
        )
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<String, OAuthError> {
        let resp = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", &self.redirect_uri),
                ("code_verifier", code_verifier),
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

        let token: TokenResponse = resp.json().await?;
        if token.access_token.is_empty() {
            return Err(OAuthError::UnexpectedResponse("empty access_token"));
        }
        // Returned by value and dropped by the caller once `fetch_user` is
        // done: this token is never written to the database.
        Ok(token.access_token)
    }

    pub async fn fetch_user(&self, access_token: &str) -> Result<DiscordUser, OAuthError> {
        let resp = self
            .client
            .get(USER_URL)
            .bearer_auth(access_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OAuthError::TokenEndpoint(TokenEndpointError::new(
                status, body,
            )));
        }

        let user: DiscordUser = resp.json().await?;
        if user.id.is_empty() {
            return Err(OAuthError::UnexpectedResponse("empty user id"));
        }
        Ok(user)
    }
}

fn require_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => panic!("{name} must be set"),
    }
}

#[cfg(test)]
mod tests {
    use super::DiscordUser;

    fn user(global_name: Option<&str>, avatar: Option<&str>) -> DiscordUser {
        DiscordUser {
            id: "123".into(),
            username: "someuser".into(),
            global_name: global_name.map(str::to_string),
            avatar: avatar.map(str::to_string),
        }
    }

    #[test]
    fn prefers_global_name_for_display() {
        assert_eq!(
            user(Some("Display Name"), None).display_name(),
            "Display Name"
        );
    }

    #[test]
    fn falls_back_to_username() {
        assert_eq!(user(None, None).display_name(), "someuser");
    }

    #[test]
    fn builds_avatar_urls() {
        assert_eq!(
            user(None, Some("abc123")).avatar_url().unwrap(),
            "https://cdn.discordapp.com/avatars/123/abc123.png"
        );
    }

    #[test]
    fn uses_gif_for_animated_avatars() {
        assert_eq!(
            user(None, Some("a_abc123")).avatar_url().unwrap(),
            "https://cdn.discordapp.com/avatars/123/a_abc123.gif"
        );
    }

    #[test]
    fn no_avatar_url_without_a_hash() {
        assert!(user(None, None).avatar_url().is_none());
    }

    #[test]
    fn ignores_unknown_fields_from_discord() {
        // Discord adds fields over time; extra members must not break login.
        let u: DiscordUser = serde_json::from_str(
            r#"{"id":"1","username":"u","discriminator":"0","flags":0,"banner":null}"#,
        )
        .unwrap();
        assert_eq!(u.id, "1");
        assert_eq!(u.display_name(), "u");
    }
}
