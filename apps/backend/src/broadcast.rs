use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestHeartRateUpdate {
    pub user_id: String,
    pub bpm: i32,
    pub recorded_at: i64,
    pub received_at: i64,
}
