mod models;
mod worker;
mod worker_manager;

use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;

use common::messages::{ConnectionChangeCommand, subjects};
use common::nats_backoff::{advance_backoff, INITIAL_BACKOFF, STABILITY_THRESHOLD};
use common::token_encryption::TokenEncryption;
use worker_manager::WorkerManager;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pulsoid_ingest=info".parse().unwrap()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor".into());

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());

    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());

    // Load encryption key BEFORE opening external connections so a missing
    // or invalid key fails fast without touching the DB or NATS server.
    let encryption = Arc::new(TokenEncryption::from_env());

    // Connect to database
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    tracing::info!("Connected to database");

    // Connect to NATS
    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    tracing::info!("Connected to NATS at {nats_url}");

    // Connect to Redis for latest_bpm cache writes
    let redis_client = redis::Client::open(redis_url.clone()).expect("Invalid REDIS_URL");
    let redis_conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to connect to Redis");

    tracing::info!("Connected to Redis at {redis_url}");

    // Create worker manager and start all active workers
    let worker_manager = WorkerManager::new(pool, nats.clone(), redis_conn, encryption);
    worker_manager.start_all_active().await;

    let wm_events = worker_manager.clone();
    let wm_reconcile = worker_manager.clone();

    // Spawn connection.changed subscriber (fire-and-forget hint → reconcile_user).
    //
    // Wrapped in an outer reconnect loop so the task does not silently die if
    // the Subscriber stream ends (`async_nats` auto-reconnects the client but
    // does NOT re-install existing Subscriber handles). Backoff is only reset
    // after a subscription has stayed up for STABILITY_THRESHOLD — a flapping
    // "subscribe → immediate end" cycle still backs off exponentially. Mirrors
    // the api-backend `hr.received` subscriber.
    let nats_events = nats.clone();
    let events_task = tokio::spawn(async move {
        let mut backoff = INITIAL_BACKOFF;
        loop {
            let mut connection_sub = match nats_events
                .subscribe(subjects::CONNECTION_CHANGED)
                .await
            {
                Ok(s) => {
                    tracing::info!("Subscribed to {}", subjects::CONNECTION_CHANGED);
                    s
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to subscribe to {}: {e}; retrying in {:?}",
                        subjects::CONNECTION_CHANGED,
                        backoff
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = advance_backoff(backoff);
                    continue;
                }
            };

            // Gap-close: a NATS outage may have dropped connection.changed
            // events while the subscriber was down. Run one full reconcile()
            // so worker state catches up immediately instead of waiting for
            // the 60s periodic pass. On cold start this is redundant with
            // `start_all_active()` but cheap (one SELECT, no-op branches in
            // apply_db_state_for_user), so we keep it unconditional to avoid
            // a first-time vs. reconnect branch.
            wm_events.reconcile().await;

            let subscribed_at = std::time::Instant::now();
            while let Some(msg) = connection_sub.next().await {
                match serde_json::from_slice::<ConnectionChangeCommand>(&msg.payload) {
                    Ok(cmd) => {
                        tracing::info!(
                            user_id = %cmd.user_id,
                            "Received connection change hint"
                        );
                        wm_events.reconcile_user(&cmd.user_id).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse connection change command: {e}");
                    }
                }
            }

            if subscribed_at.elapsed() >= STABILITY_THRESHOLD {
                backoff = INITIAL_BACKOFF;
            } else {
                backoff = advance_backoff(backoff);
            }
            tracing::warn!(
                "{} subscription ended; resubscribing in {:?}",
                subjects::CONNECTION_CHANGED,
                backoff
            );
            tokio::time::sleep(backoff).await;
        }
    });

    // Spawn periodic DB reconciliation (every 60 seconds)
    let reconcile_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            wm_reconcile.reconcile().await;
        }
    });

    // Wait for either task to complete (shouldn't happen in normal operation)
    tokio::select! {
        _ = events_task => {
            tracing::error!("Connection events subscriber exited unexpectedly");
        }
        _ = reconcile_task => {
            tracing::error!("Reconciliation task exited unexpectedly");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received shutdown signal");
        }
    }
}
