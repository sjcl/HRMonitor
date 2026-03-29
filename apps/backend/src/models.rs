use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// --- DB rows ---

#[derive(Debug, Clone, FromRow)]
pub struct UserRow {
    pub id: String,
    pub name: String,
    pub timezone: String,
    pub pulsoid_access_token: Option<String>,
    pub pulsoid_last_connected_at: Option<i64>,
    pub pulsoid_last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub timezone: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<UserRow> for User {
    fn from(r: UserRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            timezone: r.timezone,
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
pub struct CreateUserRequest {
    pub name: String,
    pub timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetPulsoidTokenRequest {
    pub access_token: String,
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
    pub from: String,
    pub to: String,
}

// --- Response DTOs ---

#[derive(Debug, FromRow, Serialize)]
pub struct UserListItem {
    pub id: String,
    pub name: String,
    pub latest_bpm: Option<i32>,
    pub has_pulsoid_token: bool,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct PulsoidTokenResponse {
    pub last_connected_at: Option<i64>,
    pub last_error: Option<String>,
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
