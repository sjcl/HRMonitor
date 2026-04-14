//! Shared exponential-backoff helper for NATS `Subscriber` reconnect loops.
//!
//! `async_nats` auto-reconnects the underlying client, but does not re-install
//! existing `Subscriber` handles — when NATS flaps, the subscription stream
//! ends and must be re-created. Services that drive subscribers in a
//! `while let Some(msg) = sub.next().await` loop wrap that loop in an outer
//! reconnect loop using these constants / this helper so behavior stays
//! consistent across services.

use std::time::Duration;

pub const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// A subscription that stays up at least this long is considered "healthy";
/// the backoff is reset to `INITIAL_BACKOFF` on the next reconnect. A flapping
/// "subscribe → immediate end" cycle therefore keeps backing off exponentially
/// instead of resetting on every short-lived attempt.
pub const STABILITY_THRESHOLD: Duration = Duration::from_secs(60);

pub fn advance_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(MAX_BACKOFF)
}
