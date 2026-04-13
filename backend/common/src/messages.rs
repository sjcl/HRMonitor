use serde::{Deserialize, Serialize};

pub mod subjects {
    pub const HR_RECEIVED: &str = "hr.received";
    pub const CONNECTION_CHANGED: &str = "pulsoid.connection.changed";
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

impl ConnectionChangeCommand {
    pub fn payload_for(user_id: &str) -> Vec<u8> {
        serde_json::to_vec(&Self {
            user_id: user_id.to_owned(),
        })
        .expect("ConnectionChangeCommand JSON serialization should be infallible")
    }
}
