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

// --- Group DTOs ---

#[derive(Debug, Deserialize)]
pub struct CreateGroupRequest {
    pub name: Option<String>,
    pub invite_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateGroupRequest {
    pub name: Option<String>,
    pub invite_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMembershipRequest {
    pub sharing: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub expires_in_hours: Option<i64>,
    pub max_uses: Option<i32>,
    pub target_user_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GroupMemberPreview {
    pub user_id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GroupListItem {
    pub id: String,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub member_count: i64,
    pub my_sharing: bool,
    pub my_role: String,
    pub invite_policy: String,
    pub member_previews: Vec<GroupMemberPreview>,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct GroupDetail {
    pub id: String,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub invite_policy: String,
    pub my_sharing: bool,
    pub my_role: String,
    pub members: Vec<GroupMemberInfo>,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct GroupMemberInfo {
    pub user_id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role: String,
    pub sharing: bool,
}

#[derive(Debug, Serialize)]
pub struct InviteListItem {
    pub id: String,
    pub created_by_name: String,
    pub expires_at: i64,
    pub max_uses: Option<i32>,
    pub use_count: i32,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct InviteInfo {
    pub group_name: Option<String>,
    pub group_display_name: Option<String>,
    pub group_id: String,
    pub inviter_name: String,
    pub expires_at: i64,
    pub valid: bool,
    pub reason: Option<String>,
    pub already_member: bool,
}

#[derive(Debug, Serialize)]
pub struct CreateInviteResponse {
    pub id: String,
    pub token: String,
    pub expires_at: i64,
}

#[derive(Debug, Serialize)]
pub struct AcceptInviteResponse {
    pub group_id: String,
}

// --- Group heart rate DTOs ---

#[derive(Debug, FromRow, Serialize)]
pub struct GroupHeartRateResponse {
    pub user_id: String,
    pub bpm: i32,
    pub timestamp: i64,
}

#[derive(Debug, FromRow, Serialize)]
pub struct GroupMinuteStatsResponse {
    pub user_id: String,
    pub timestamp: i64,
    pub avg_bpm: f64,
    pub min_bpm: i32,
    pub max_bpm: i32,
    pub sample_count: i64,
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
