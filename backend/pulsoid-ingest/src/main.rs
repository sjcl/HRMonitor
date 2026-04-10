mod models;
mod worker;
mod worker_manager;

use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;

use common::messages::{ConnectionChangeAck, ConnectionChangeCommand, subjects};
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

    // Load encryption key BEFORE opening external connections so a missing
    // or invalid key fails fast without touching the DB or NATS server.
    let encryption = Arc::new(TokenEncryption::from_env());

    // Connect to database
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    tracing::info!("Connected to database");

    // Connect to NATS
    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    tracing::info!("Connected to NATS at {nats_url}");

    // Create worker manager and start all active workers
    let worker_manager = WorkerManager::new(pool, nats.clone(), encryption);
    worker_manager.start_all_active().await;

    // Subscribe to connection.changed events
    let mut connection_sub = nats
        .subscribe(subjects::CONNECTION_CHANGED)
        .await
        .expect("Failed to subscribe to connection.changed");

    let wm_events = worker_manager.clone();
    let wm_reconcile = worker_manager.clone();
    let nats_for_reply = nats.clone();

    // Spawn connection.changed subscriber (request/reply handler)
    let events_task = tokio::spawn(async move {
        while let Some(msg) = connection_sub.next().await {
            match serde_json::from_slice::<ConnectionChangeCommand>(&msg.payload) {
                Ok(cmd) => {
                    tracing::info!(
                        user_id = %cmd.user_id,
                        config_version = ?cmd.config_version,
                        "Received connection change command"
                    );
                    let result = wm_events
                        .notify_connection_changed(&cmd.user_id, cmd.config_version)
                        .await;

                    if let Some(reply) = msg.reply {
                        let ack = match result {
                            Ok(outcome) => ConnectionChangeAck {
                                applied: true,
                                stale: outcome.stale,
                                config_version: outcome.actual_config_version,
                                error: None,
                            },
                            Err(e) => ConnectionChangeAck {
                                applied: false,
                                stale: false,
                                config_version: None,
                                error: Some(e),
                            },
                        };
                        let payload = serde_json::to_vec(&ack).unwrap().into();
                        if let Err(e) = nats_for_reply.publish(reply, payload).await {
                            tracing::warn!(
                                user_id = %cmd.user_id,
                                "Failed to send ack: {e}"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to parse connection change command: {e}");
                }
            }
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
