//! Shared Redis connection construction.
//!
//! Every service holds its Redis handle for the lifetime of the process, so the
//! handle must survive a Redis restart on its own. `MultiplexedConnection` does
//! not: it has no reconnect, so once the socket dies every subsequent command
//! fails until the *process* is restarted. `/healthz` is a static `"ok"`, so
//! nothing notices. `ConnectionManager` reconnects in the background instead.
//!
//! Crucially, it does not retry the command that failed — `reconnect_if_io_error!`
//! / `reconnect_if_dropped!` schedule a reconnect and hand the error back to the
//! caller. That is what makes it safe here: the refresh rotation is a Lua script
//! whose effects must not be re-applied blindly, and every caller already has a
//! correct error path (503 with cookies intact for refresh, warn-and-skip for the
//! warm-up pipeline, `Err` propagation in the ingest worker).
//!
//! This module exists for the same reason as `nats_backoff`: the settings below
//! are a behavioural contract that must stay identical across services, so they
//! are defined exactly once.

use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};

/// Bound on a single reconnect attempt's TCP+RESP handshake. Redis is on the
/// Docker network with the services, so this matches nginx's
/// `proxy_connect_timeout 2s` for the analogous hop.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound on waiting for a reply. Every command we issue is O(1) — GET, SET,
/// MGET, DEL, and two small scripts — so exceeding this means the connection is
/// wedged, not that the work is slow.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Reconnect attempts *charged to the waiting command* (see below). One retry,
/// not the crate default of 6.
const NUMBER_OF_RETRIES: usize = 1;

const MIN_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_millis(500);

/// The connection settings shared by every service.
///
/// Both timeouts default to `None` (wait forever) in the crate, which is exactly
/// the failure mode we are removing, so both are always set.
///
/// `number_of_retries` is deliberately far below the crate default of 6. The
/// reconnect retry loop runs *inside* the shared connection future that
/// `send_packed_command` awaits, so its whole budget is added to the latency of
/// whichever command triggered it. At the default, a Redis that blackholes
/// packets would stall a single command for ~20s — unacceptable for
/// `/api/auth/refresh`, which sits on the browser's critical path.
///
/// Retrying harder in here buys nothing anyway, because every caller already
/// retries at a better layer: the SPA re-attempts a 503 refresh
/// (`frontend/src/lib/http.ts`), the WS self-heal ticks every 10s, and the ingest
/// worker gets the next Pulsoid frame about a second later. A cached connect
/// failure is itself an I/O error, so the *next* command starts a fresh reconnect
/// cycle — the manager never lands in a terminal broken state as long as traffic
/// keeps arriving. That property is why `/healthz` deliberately does not check
/// Redis.
pub fn manager_config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new()
        .set_connection_timeout(Some(CONNECTION_TIMEOUT))
        .set_response_timeout(Some(RESPONSE_TIMEOUT))
        .set_number_of_retries(NUMBER_OF_RETRIES)
        .set_min_delay(MIN_RETRY_DELAY)
        .set_max_delay(MAX_RETRY_DELAY)
}

/// Builds a `ConnectionManager` without dialing out.
///
/// The first command establishes the connection. This means a service starts
/// even when Redis is down — matching `docker-compose`'s `service_started`
/// dependency — and the only remaining error is a malformed URL, i.e. a
/// misconfiguration. Fail-fast now applies to configuration rather than to the
/// availability of a dependency that recovers by itself.
///
/// Each call opens its own connection, and clones of the returned manager share
/// it. Call this again when a caller needs a *separate* connection; cloning is
/// not a substitute.
pub fn connect_lazy(url: &str) -> redis::RedisResult<ConnectionManager> {
    let client = redis::Client::open(url)?;
    let manager = ConnectionManager::new_lazy_with_config(client, manager_config())?;
    // The URL is deliberately not logged: `redis://:password@host` is a valid
    // REDIS_URL, and the target is already visible in the deployment config.
    tracing::info!(
        "Redis connection manager ready (lazy: connects on first command, \
         reconnects in the background on I/O errors)"
    );
    Ok(manager)
}
