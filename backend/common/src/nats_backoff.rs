//! Shared exponential-backoff helper for NATS `Subscriber` reconnect loops.
//!
//! `async_nats` auto-reconnects the underlying client, but does not re-install
//! existing `Subscriber` handles. When the subscription stream ends, it must be
//! re-created. Services that drive subscribers in a
//! `while let Some(msg) = sub.next().await` loop wrap that loop in an outer
//! reconnect loop using these constants / this helper so behavior stays
//! consistent across services.

use std::time::Duration;

pub const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

pub fn advance_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(MAX_BACKOFF)
}
