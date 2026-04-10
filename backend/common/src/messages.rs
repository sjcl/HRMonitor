use serde::{Deserialize, Serialize};

pub mod subjects {
    pub const HR_RECEIVED: &str = "hr.received";
    pub const CONNECTION_CHANGED: &str = "pulsoid.connection.changed";
    pub const TOKEN_REFRESH_NEEDED: &str = "pulsoid.token.refresh_needed";
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartRateReceived {
    pub user_id: String,
    pub bpm: i32,
    pub recorded_at: i64,
    pub received_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionChangeCommand {
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRefreshRequest {
    pub user_id: String,
    pub config_version: i32,
}
