use futures_util::StreamExt;
use sqlx::PgPool;
use std::time::Duration;
use tokio_tungstenite::connect_async;

use crate::models::{PulsoidMessage, PulsoidToken};

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";

pub async fn run_worker(db: PgPool, token: PulsoidToken) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
        let url = format!("{PULSOID_WS_URL}?access_token={}", token.access_token);
        tracing::info!(token_id = %token.id, "Connecting to Pulsoid WebSocket");

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                backoff = Duration::from_secs(1);

                let now = chrono_now();
                let _ = sqlx::query("UPDATE pulsoid_tokens SET last_connected_at = to_timestamp($1), last_error = NULL, updated_at = to_timestamp($2) WHERE id = $3")
                    .bind(now)
                    .bind(now)
                    .bind(&token.id)
                    .execute(&db)
                    .await;

                tracing::info!(token_id = %token.id, "Connected to Pulsoid WebSocket");

                let (_, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if let Err(e) = handle_message(&db, &token, &text).await {
                                tracing::warn!(token_id = %token.id, "Failed to handle message: {e}");
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            tracing::info!(token_id = %token.id, "WebSocket closed by server");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(token_id = %token.id, "WebSocket error: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                let error_msg = format!("{e}");
                tracing::warn!(token_id = %token.id, "Failed to connect: {error_msg}");

                let now = chrono_now();
                let _ = sqlx::query("UPDATE pulsoid_tokens SET last_error = $1, updated_at = to_timestamp($2) WHERE id = $3")
                    .bind(&error_msg)
                    .bind(now)
                    .bind(&token.id)
                    .execute(&db)
                    .await;
            }
        }

        tracing::info!(token_id = %token.id, backoff_secs = backoff.as_secs(), "Reconnecting after backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

async fn handle_message(
    db: &PgPool,
    token: &PulsoidToken,
    text: &str,
) -> Result<(), String> {
    let msg: PulsoidMessage = serde_json::from_str(text).map_err(|e| format!("Parse error: {e}"))?;

    let bpm = msg.data.heart_rate;
    if !(20..=250).contains(&bpm) {
        return Err(format!("BPM {bpm} out of range (20-250)"));
    }

    let now = chrono_now();
    let recorded_at = msg.measured_at
        .filter(|&t| t > 0)
        .map(|t| t / 1000)
        .unwrap_or(now);

    sqlx::query(
        "INSERT INTO heart_rate_records (user_id, pulsoid_token_id, recorded_at, bpm, received_at) VALUES ($1, $2, to_timestamp($3), $4, to_timestamp($5))"
    )
    .bind(&token.user_id)
    .bind(&token.id)
    .bind(recorded_at)
    .bind(bpm)
    .bind(now)
    .execute(db)
    .await
    .map_err(|e| format!("DB insert error: {e}"))?;

    Ok(())
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
