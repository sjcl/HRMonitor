//! pulsoid-refresher: proactive Pulsoid OAuth token refresh service.
//!
//! Scans `pulsoid_connections` every `SCAN_INTERVAL_SECS` seconds and refreshes
//! any row whose `token_expires_at` is within `REFRESH_SAFETY_MARGIN_SECS` of
//! expiring. Within each scan pass, up to `REFRESH_CONCURRENCY` refreshes run
//! in parallel via `FuturesUnordered`. Per-user exclusivity is guaranteed by
//! a Postgres advisory lock inside each `refresh_if_expiring` call.
//!
//! **Scan-level serialization is still required.** The main loop awaits each
//! scan pass to completion before sleeping and starting the next one. Never
//! wrap `scan_and_refresh_once` in `tokio::spawn` or `tokio::interval` — if
//! a tick fires before the previous scan finishes, two passes could feed the
//! same user into `FuturesUnordered` concurrently, wasting an advisory lock
//! round-trip and a DB connection for the contended duplicate.

mod refresh;
mod scanner;

use std::time::{Duration, Instant};

use common::pulsoid_oauth::PulsoidOAuthConfig;
use common::signal::shutdown_signal;
use common::token_encryption::TokenEncryption;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::watch;

/// Scan cadence. Must stay well below `REFRESH_SAFETY_MARGIN_SECS` so that a
/// row that barely missed one scan cycle still gets picked up with plenty of
/// headroom on the next one.
const SCAN_INTERVAL_SECS: u64 = 60;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pulsoid_refresher=info".parse().unwrap()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor".into());
    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());

    let refresh_concurrency: usize = match std::env::var("REFRESH_CONCURRENCY") {
        Ok(v) => v
            .parse()
            .expect("REFRESH_CONCURRENCY must be a positive integer"),
        Err(_) => 10,
    };
    assert!(
        refresh_concurrency >= 1,
        "REFRESH_CONCURRENCY must be >= 1, got {refresh_concurrency}"
    );

    // Pool sizing: each in-flight refresh holds Tx A (advisory lock) on one
    // connection + Tx B or Tx C on a separate connection = 2 connections per
    // refresh at peak. The +4 covers the scanner's `fetch_all` SELECT,
    // `write_error_state` (which acquires a fresh connection while Tx A is
    // still held), and headroom.
    //
    // This is a per-process limit. When running multiple refresher instances,
    // operators must ensure that the sum of all pool sizes stays within the
    // DB's `max_connections`.
    //
    // `acquire_timeout(15s)`: with concurrent refreshes, transient pool
    // contention is expected under load (e.g. all slots doing HTTP calls
    // simultaneously). 15s is generous enough to ride out bursts without
    // false-alarming on a healthy system.
    let pool_size = (2 * refresh_concurrency + 4) as u32;
    let db = PgPoolOptions::new()
        .max_connections(pool_size)
        .acquire_timeout(Duration::from_secs(15))
        .connect(&database_url)
        .await
        .expect("Failed to connect to Postgres");

    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");
    tracing::info!("Connected to NATS at {nats_url}");

    let encryption = TokenEncryption::from_env();
    // Refresh-only config: does NOT read `PULSOID_REDIRECT_URI`. The refresh
    // endpoint does not accept a redirect_uri and this service never calls
    // `authorization_url` or `exchange_code`.
    let oauth = PulsoidOAuthConfig::from_env_for_refresh();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // Detached task: sets the shutdown flag when SIGTERM/SIGINT arrives.
    // Cleaned up automatically when the tokio runtime drops at end of main.
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("Received shutdown signal");
        let _ = shutdown_tx.send(true);
    });

    tracing::info!(
        scan_interval_secs = SCAN_INTERVAL_SECS,
        safety_margin_secs = refresh::REFRESH_SAFETY_MARGIN_SECS,
        refresh_concurrency,
        pool_size,
        "pulsoid-refresher starting scan loop"
    );

    let scan_interval = Duration::from_secs(SCAN_INTERVAL_SECS);
    loop {
        // IMPORTANT: await the full scan before deciding how long to wait.
        // Never wrap `scan_and_refresh_once` in `tokio::spawn` /
        // `tokio::interval` — if a tick fires before the previous scan
        // finishes, two passes could feed the same user into the concurrent
        // work queue simultaneously. Serial await-then-(maybe-)sleep makes
        // scan-level overlap structurally impossible. (Intra-scan parallelism
        // is intentional and handled by `FuturesUnordered` inside the scanner.)
        //
        // `SCAN_INTERVAL_SECS` is a *target cadence*, not an added delay.
        // After a fast pass we sleep the remainder of the window; after a
        // pass that ran close to (or over) the window we start the next
        // scan immediately. This closes a blind spot where a long pass
        // combined with a fixed 60s post-sleep could leave rows whose
        // `token_expires_at` fell just outside the initial
        // `<= now() + 300s` cutoff unrefreshed until after they had
        // already expired.
        let loop_start = Instant::now();
        let interrupted = scanner::scan_and_refresh_once(
            &db,
            &nats,
            &encryption,
            &oauth,
            &shutdown_rx,
            refresh_concurrency,
        )
        .await;
        if interrupted {
            break;
        }
        let remaining = scan_interval.saturating_sub(loop_start.elapsed());
        let mut shutdown_wait = shutdown_rx.clone();
        tokio::select! {
            biased;
            res = shutdown_wait.wait_for(|&v| v) => {
                if res.is_err() {
                    // sender dropped = watcher task died (panic or unexpected exit).
                    // Treat as internal fault, not graceful shutdown.
                    tracing::error!("Shutdown watcher task failed (sender dropped); forcing exit");
                    std::process::exit(1);
                }
                break;
            }
            _ = tokio::time::sleep(remaining) => {}
        }
    }

    nats.flush().await.ok();
    tracing::info!("pulsoid-refresher shut down gracefully");
}

