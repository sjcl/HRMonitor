use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// --- Constants ---

pub const SOURCE_OAUTH: &str = "oauth";
pub const SOURCE_MANUAL: &str = "manual";

// --- DB rows ---

#[derive(Debug, Clone, FromRow)]
pub struct UserRow {
    pub id: String,
    pub display_name: String,
    pub timezone: String,
    pub avatar_url: Option<String>,
    pub heart_rate_visibility: String,
    pub created_at: i64,
    pub updated_at: i64,
}

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
}

#[derive(Debug, FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub display_name: String,
    pub timezone: String,
    pub avatar_url: Option<String>,
    pub heart_rate_visibility: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<UserRow> for User {
    fn from(r: UserRow) -> Self {
        Self {
            id: r.id,
            display_name: r.display_name,
            timezone: r.timezone,
            avatar_url: r.avatar_url,
            heart_rate_visibility: r.heart_rate_visibility,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, FromRow, Serialize)]
#[allow(dead_code)]
pub struct HeartRateRecord {
    pub id: i64,
    pub user_id: String,
    pub recorded_at: i64,
    pub bpm: i32,
    pub received_at: i64,
}

// --- Request DTOs ---

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub display_name: Option<String>,
    pub timezone: Option<String>,
    pub heart_rate_visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HeartRateQuery {
    pub period: String,
}

#[derive(Debug, Deserialize)]
pub struct HeartRateByDateQuery {
    pub date: String,
}

#[derive(Debug, Deserialize)]
pub struct DailyStatsQuery {
    pub date: String,
}

// --- Response DTOs ---

#[derive(Debug, FromRow, Serialize)]
pub struct UserListItem {
    pub id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub latest_bpm: Option<i32>,
    pub has_pulsoid_token: bool,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct PulsoidTokenResponse {
    pub source: String,
    pub last_connected_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetManualTokenRequest {
    pub access_token: String,
}

#[derive(Debug, FromRow, Serialize)]
pub struct HeartRateResponse {
    pub bpm: i32,
    pub timestamp: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DailyStatsResponse {
    pub day: String,
    pub avg_bpm: f64,
    pub min_bpm: i32,
    pub max_bpm: i32,
    pub count: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct MinuteStatsResponse {
    pub timestamp: i64,
    pub avg_bpm: f64,
    pub min_bpm: i32,
    pub max_bpm: i32,
    pub sample_count: i64,
}

// --- Client WebSocket messages ---

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMessage {
    Subscribe { user_ids: Vec<String> },
    Unsubscribe { user_ids: Vec<String> },
}

// --- Pulsoid WebSocket message ---

#[derive(Debug, Deserialize)]
pub struct PulsoidMessage {
    pub measured_at: Option<i64>,
    pub data: PulsoidData,
}

#[derive(Debug, Deserialize)]
pub struct PulsoidData {
    pub heart_rate: i32,
}
