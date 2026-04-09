use futures_util::StreamExt;
use sqlx::PgPool;
use std::time::Duration;
use tokio_tungstenite::connect_async;

use common::messages::{HeartRateReceived, TokenRefreshRequest, subjects};
use common::token_encryption::TokenEncryption;

use crate::models::{PulsoidConnectionRow, PulsoidMessage, SOURCE_OAUTH};

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";
const REFRESH_SAFETY_MARGIN_SECS: i64 = 60;

pub async fn run_worker(db: PgPool, nats: async_nats::Client, user_id: String) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
        // Fetch connection from DB
        let conn: Option<PulsoidConnectionRow> = match sqlx::query_as(
            "SELECT id, user_id, source, access_token, refresh_token, key_version,
                    EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at,
                    EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
                    last_error, refresh_blocked, config_version, connection_state
             FROM pulsoid_connections WHERE user_id = $1",
        )
        .bind(&user_id)
        .fetch_optional(&db)
        .await
        {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(user_id = %user_id, "DB error fetching pulsoid connection: {e}");
                tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Retrying after backoff");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        let conn = match conn {
            Some(c) => c,
            None => {
                tracing::info!(user_id = %user_id, "No pulsoid connection found, worker exiting");
                return;
            }
        };

        let encryption = TokenEncryption::from_env();

        // Decrypt access token
        let access_token = match encryption.decrypt(&conn.access_token, conn.key_version as u32) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(user_id = %user_id, "Failed to decrypt access token: {e}");
                update_connection_state(&db, &user_id, "error", Some("Failed to decrypt access token")).await;
                return;
            }
        };

        // Check token expiry for OAuth connections — request refresh via NATS
        if conn.source == SOURCE_OAUTH {
            if conn.refresh_blocked {
                tracing::warn!(user_id = %user_id, last_error = ?conn.last_error,
                    "Refresh blocked due to terminal failure, worker exiting. User must re-authorize.");
                update_connection_state(&db, &user_id, "error", conn.last_error.as_deref()).await;
                return;
            }

            if let Some(expires_at) = conn.token_expires_at {
                let now = system_now();
                if now >= expires_at - REFRESH_SAFETY_MARGIN_SECS {
                    // Token expired or about to expire — request refresh from api-backend
                    let req = TokenRefreshRequest {
                        user_id: user_id.to_string(),
                    };
                    if let Err(e) = nats
                        .publish(
                            subjects::TOKEN_REFRESH_NEEDED,
                            serde_json::to_vec(&req).unwrap().into(),
                        )
                        .await
                    {
                        tracing::warn!(user_id = %user_id, "Failed to publish refresh_needed: {e}");
                    }
                    update_last_error(&db, &user_id, "Token expired, refresh requested").await;
                    tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Token expired, waiting for refresh");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                    continue;
                }
            } else {
                tracing::error!(user_id = %user_id, "OAuth connection missing token_expires_at");
                update_last_error(
                    &db,
                    &user_id,
                    "OAuth connection missing expiry (data inconsistency)",
                )
                .await;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        }

        let url = format!("{PULSOID_WS_URL}?access_token={access_token}");
        tracing::info!(user_id = %user_id, "Connecting to Pulsoid WebSocket");

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                backoff = Duration::from_secs(1);

                let now = system_now();
                let _ = sqlx::query(
                    "UPDATE pulsoid_connections SET last_connected_at = to_timestamp($1), last_error = NULL, connection_state = 'connected', state_updated_at = now() WHERE user_id = $2",
                )
                .bind(now)
                .bind(&user_id)
                .execute(&db)
                .await;

                tracing::info!(user_id = %user_id, "Connected to Pulsoid WebSocket");

                let (_, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if let Err(e) =
                                handle_message(&db, &nats, &user_id, &text).await
                            {
                                tracing::warn!(user_id = %user_id, "Failed to handle message: {e}");
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            tracing::info!(user_id = %user_id, "WebSocket closed by server");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(user_id = %user_id, "WebSocket error: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("{e}");
                tracing::warn!(user_id = %user_id, "Failed to connect: {error_msg}");
                update_last_error(&db, &user_id, &error_msg).await;
            }
        }

        tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Reconnecting after backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

async fn update_last_error(db: &PgPool, user_id: &str, error: &str) {
    let _ = sqlx::query("UPDATE pulsoid_connections SET last_error = $1 WHERE user_id = $2")
        .bind(error)
        .bind(user_id)
        .execute(db)
        .await;
}

async fn update_connection_state(db: &PgPool, user_id: &str, state: &str, error: Option<&str>) {
    let _ = sqlx::query(
        "UPDATE pulsoid_connections SET connection_state = $1, state_updated_at = now(), last_error = $2 WHERE user_id = $3",
    )
    .bind(state)
    .bind(error)
    .bind(user_id)
    .execute(db)
    .await;
}

async fn handle_message(
    db: &PgPool,
    nats: &async_nats::Client,
    user_id: &str,
    text: &str,
) -> Result<(), String> {
    let msg: PulsoidMessage =
        serde_json::from_str(text).map_err(|e| format!("Parse error: {e}"))?;

    let bpm = msg.data.heart_rate;
    if !(20..=250).contains(&bpm) {
        return Err(format!("BPM {bpm} out of range (20-250)"));
    }

    let now = system_now();
    let recorded_at = msg
        .measured_at
        .filter(|&t| t > 0)
        .map(|t| t / 1000)
        .unwrap_or(now);

    sqlx::query(
        "INSERT INTO heart_rate_records (user_id, recorded_at, bpm, received_at) VALUES ($1, to_timestamp($2), $3, to_timestamp($4))"
    )
    .bind(user_id)
    .bind(recorded_at)
    .bind(bpm)
    .bind(now)
    .execute(db)
    .await
    .map_err(|e| format!("DB insert error: {e}"))?;

    let update = HeartRateReceived {
        user_id: user_id.to_string(),
        bpm,
        recorded_at,
        received_at: now,
    };

    // Publish to NATS for api-backend to update Redis cache + WS broadcast
    let payload = serde_json::to_vec(&update).unwrap();
    let retries = [None, Some(Duration::from_millis(100)), Some(Duration::from_millis(500))];
    let last = retries.len() - 1;
    for (i, delay) in retries.into_iter().enumerate() {
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        match nats.publish(subjects::HR_RECEIVED, payload.clone().into()).await {
            Ok(()) => break,
            Err(e) if i == last => {
                tracing::warn!(user_id = %user_id, "Failed to publish hr.received after {} attempts: {e}", last + 1);
            }
            Err(e) => {
                tracing::debug!(user_id = %user_id, attempt = i + 1, "Retrying hr.received publish: {e}");
            }
        }
    }

    Ok(())
}

fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
