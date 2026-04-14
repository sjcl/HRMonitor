use crate::messages::HeartRateReceived;

/// latest_bpm Redis key の有効期間 (6 時間)
pub const LATEST_BPM_TTL_SECS: u64 = 6 * 60 * 60;

/// Redis key for a user's latest heart rate.
///
/// The `v2:` namespace was introduced when Redis became the authoritative
/// latest-state store with per-key TTL. The old `latest_bpm:{user_id}` keys
/// written by api-backend had no TTL, so a pre-migration deploy may leave
/// permanent zombie keys behind. Bumping the namespace ensures the new code
/// never reads them (they can be dropped manually via `redis-cli` if desired).
pub fn latest_bpm_key(user_id: &str) -> String {
    format!("latest_bpm:v2:{user_id}")
}

pub fn serialize_latest_bpm(update: &HeartRateReceived) -> String {
    serde_json::to_string(update).expect("HeartRateReceived serialization is infallible")
}
