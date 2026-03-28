use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// --- DB rows ---

#[derive(Debug, FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct PulsoidToken {
    pub id: String,
    pub user_id: String,
    pub label: Option<String>,
    pub access_token: String,
    pub is_active: bool,
    pub last_connected_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, FromRow, Serialize)]
#[allow(dead_code)]
pub struct HeartRateRecord {
    pub id: i64,
    pub user_id: String,
    pub pulsoid_token_id: String,
    pub recorded_at: i64,
    pub bpm: i32,
    pub received_at: i64,
}

// --- Request DTOs ---

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    pub label: Option<String>,
    pub access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTokenRequest {
    pub label: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct HeartRateQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<i64>,
}

// --- Response DTOs ---

#[derive(Debug, FromRow, Serialize)]
pub struct UserListItem {
    pub id: String,
    pub name: String,
    pub latest_bpm: Option<i32>,
    pub token_count: i64,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub id: String,
    pub user_id: String,
    pub label: Option<String>,
    pub is_active: bool,
    pub last_connected_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<PulsoidToken> for TokenResponse {
    fn from(t: PulsoidToken) -> Self {
        Self {
            id: t.id,
            user_id: t.user_id,
            label: t.label,
            is_active: t.is_active,
            last_connected_at: t.last_connected_at,
            last_error: t.last_error,
            created_at: t.created_at,
            updated_at: t.updated_at,
        }
    }
}

#[derive(Debug, FromRow, Serialize)]
pub struct HeartRateResponse {
    pub bpm: i32,
    pub timestamp: i64,
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
