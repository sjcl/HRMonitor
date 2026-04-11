//! Wall-clock helpers.
//!
//! Centralised so every backend crate shares a single "now in unix
//! seconds" implementation. Uses `std::time::SystemTime` directly
//! (no `chrono`) but handles the pre-epoch error case instead of
//! `.unwrap()` — the old inlined helpers panicked if the OS clock
//! was set before 1970 (e.g. an unsynced container clock at boot,
//! a BIOS RTC glitch).

use std::time::{SystemTime, UNIX_EPOCH};

/// Current **wall-clock** time as seconds since `UNIX_EPOCH`.
///
/// Note: this is wall-clock, not monotonic — it can jump backwards
/// on NTP adjustments and should not be used for measuring elapsed
/// durations. Use [`std::time::Instant`] for that.
///
/// Never panics. If the system clock is set before `UNIX_EPOCH` this
/// returns a negative value (the number of seconds the clock is
/// *before* 1970, negated) so that comparisons against DB-stored
/// expiries remain monotonic and existing rows simply look
/// not-expired until the clock is corrected.
pub fn unix_now_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}
