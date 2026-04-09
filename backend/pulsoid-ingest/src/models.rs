use serde::Deserialize;
use sqlx::FromRow;

pub const SOURCE_OAUTH: &str = "oauth";

#[derive(Debug, Clone, FromRow)]
pub struct PulsoidConnectionRow {
    pub id: String,
    pub user_id: String,
    pub source: String,
    pub access_token: Vec<u8>,
    pub refresh_token: Option<Vec<u8>>,
    pub key_version: i32,
    pub token_expires_at: Option<i64>,
    pub last_connected_at: Option<i64>,
    pub last_error: Option<String>,
    pub refresh_blocked: bool,
    pub config_version: i32,
    pub connection_state: String,
}

#[derive(Debug, Deserialize)]
pub struct PulsoidMessage {
    pub measured_at: Option<i64>,
    pub data: PulsoidData,
}

#[derive(Debug, Deserialize)]
pub struct PulsoidData {
    pub heart_rate: i32,
}
