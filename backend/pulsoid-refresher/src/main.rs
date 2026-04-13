//! pulsoid-refresher: proactive Pulsoid OAuth token refresh service.
//!
//! Scans `pulsoid_connections` every `SCAN_INTERVAL_SECS` seconds and refreshes
//! any row whose `token_expires_at` is within `REFRESH_SAFETY_MARGIN_SECS` of
//! expiring. The scan loop is serial (await-then-sleep, no `tokio::spawn`) so
//! a single process can never process the same user twice concurrently.
//! Cross-process dedup is handled by a Postgres advisory lock taken inside
//! each `refresh_if_expiring` call.

mod refresh;
mod scanner;

use std::time::{Duration, Instant};

use common::pulsoid_oauth::PulsoidOAuthConfig;
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

    // Pool sizing: `refresh_if_expiring` holds Tx A (advisory lock only) on
    // one connection for the full refresh lifetime while acquiring Tx B or
    // Tx C on a *separate* connection — at most 2 concurrent connections per
    // refresh. The scanner's `fetch_all` SELECT completes and releases its
    // connection before the for-loop calls `refresh_if_expiring`, so scanner
    // and refresh never hold connections simultaneously. The 4th slot is
    // headroom for the error-path `write_error_state` tx (which acquires a
    // fresh connection while Tx A is still held) and for future refactors.
    //
    // `acquire_timeout(5s)`: the serial scan loop (`scanner.rs` for-loop +
    // `main.rs` await-then-sleep) guarantees at most one refresh in flight
    // per process, so pool contention should never occur. If that invariant
    // ever regresses, the 5 s timeout surfaces the bug promptly instead of
    // hanging on sqlx's default 30 s.
    let db = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(5))
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
        "pulsoid-refresher starting scan loop"
    );

    let scan_interval = Duration::from_secs(SCAN_INTERVAL_SECS);
    loop {
        // IMPORTANT: await the full scan before deciding how long to wait.
        // Never wrap `scan_and_refresh_once` in `tokio::spawn` /
        // `tokio::interval` — if a tick fires before the previous scan
        // finishes we could double-refresh the same user. Serial
        // await-then-(maybe-)sleep makes overlap structurally impossible.
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
        let interrupted =
            scanner::scan_and_refresh_once(&db, &nats, &encryption, &oauth, &shutdown_rx).await;
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

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            biased;
            _ = sigterm.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}
