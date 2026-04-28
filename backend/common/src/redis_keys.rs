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

/// Compute the Redis TTL for a `latest_bpm` write anchored at `recorded_at`.
///
/// Returns `None` when the measurement is at least `LATEST_BPM_TTL_SECS` old —
/// callers must skip the Redis write (and any companion live broadcast) so we
/// never resurrect a value that should already be considered stale. Future
/// timestamps (clock skew) collapse to the full TTL.
pub fn latest_bpm_ttl_secs(now_secs: i64, recorded_at_secs: i64) -> Option<u64> {
    if recorded_at_secs >= now_secs {
        return Some(LATEST_BPM_TTL_SECS);
    }
    let age = (now_secs - recorded_at_secs) as u64;
    if age >= LATEST_BPM_TTL_SECS {
        None
    } else {
        Some(LATEST_BPM_TTL_SECS - age)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_measurement_gets_full_ttl() {
        let now = 1_700_000_000;
        assert_eq!(latest_bpm_ttl_secs(now, now), Some(LATEST_BPM_TTL_SECS));
    }

    #[test]
    fn one_hour_old_measurement_loses_one_hour() {
        let now = 1_700_000_000;
        let recorded = now - 3_600;
        assert_eq!(
            latest_bpm_ttl_secs(now, recorded),
            Some(LATEST_BPM_TTL_SECS - 3_600)
        );
    }

    #[test]
    fn measurement_exactly_at_ttl_boundary_is_stale() {
        let now = 1_700_000_000;
        let recorded = now - LATEST_BPM_TTL_SECS as i64;
        assert_eq!(latest_bpm_ttl_secs(now, recorded), None);
    }

    #[test]
    fn future_recorded_at_collapses_to_full_ttl() {
        let now = 1_700_000_000;
        let recorded = now + 60;
        assert_eq!(latest_bpm_ttl_secs(now, recorded), Some(LATEST_BPM_TTL_SECS));
    }
}
