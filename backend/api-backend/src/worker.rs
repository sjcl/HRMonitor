use futures_util::StreamExt;
use redis::AsyncCommands;
use sqlx::PgPool;
use std::time::Duration;
use tokio::sync::broadcast::Sender;
use tokio_tungstenite::connect_async;

use crate::broadcast::LatestHeartRateUpdate;
use crate::models::{PulsoidConnectionRow, PulsoidMessage, SOURCE_OAUTH};
use crate::pulsoid_oauth::PulsoidOAuthConfig;
use crate::token_encryption::TokenEncryption;

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";
const REFRESH_SAFETY_MARGIN_SECS: i64 = 60;

pub async fn run_worker(
    db: PgPool,
    redis: redis::aio::MultiplexedConnection,
    hr_tx: Sender<LatestHeartRateUpdate>,
    user_id: String,
) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
        // Fetch connection from DB
        let conn: Option<PulsoidConnectionRow> = match sqlx::query_as(
            "SELECT id, user_id, source, access_token, refresh_token, key_version,
                    EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at,
                    EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
                    last_error
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

        // Load encryption from env each loop iteration would be wasteful;
        // instead we construct once. Since we're in a spawned task, we read from env.
        let encryption = TokenEncryption::from_env();
        let oauth_config = PulsoidOAuthConfig::from_env();

        // Decrypt access token
        let access_token = match encryption.decrypt(&conn.access_token, conn.key_version as u32) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(user_id = %user_id, "Failed to decrypt access token: {e}");
                update_last_error(&db, &user_id, "Failed to decrypt access token").await;
                return;
            }
        };

        // Check token expiry and refresh if needed (OAuth only; manual tokens have no expiry)
        let access_token = if conn.source == SOURCE_OAUTH {
            let expires_at = match conn.token_expires_at {
                Some(ts) => ts,
                None => {
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
            };
            if is_token_expired(expires_at) {
                match try_refresh(&db, &encryption, &oauth_config, &conn, &user_id).await {
                    Ok(new_token) => new_token,
                    Err(e) => {
                        tracing::warn!(user_id = %user_id, "Token refresh failed: {e}");
                        update_last_error(&db, &user_id, &format!("Token refresh failed: {e}"))
                            .await;
                        tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Retrying after backoff");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(max_backoff);
                        continue;
                    }
                }
            } else {
                access_token
            }
        } else {
            // Manual token: no expiry, no refresh
            access_token
        };

        let url = format!("{PULSOID_WS_URL}?access_token={access_token}");
        tracing::info!(user_id = %user_id, "Connecting to Pulsoid WebSocket");

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                backoff = Duration::from_secs(1);

                let now = chrono_now();
                let _ = sqlx::query(
                    "UPDATE pulsoid_connections SET last_connected_at = to_timestamp($1), last_error = NULL WHERE user_id = $2",
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
                                handle_message(&db, &redis, &hr_tx, &user_id, &text).await
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

fn is_token_expired(token_expires_at: i64) -> bool {
    let now = chrono_now();
    now >= token_expires_at - REFRESH_SAFETY_MARGIN_SECS
}

async fn try_refresh(
    db: &PgPool,
    encryption: &TokenEncryption,
    oauth_config: &PulsoidOAuthConfig,
    conn: &PulsoidConnectionRow,
    user_id: &str,
) -> Result<String, String> {
    let refresh_token_bytes = conn.refresh_token.as_ref().ok_or_else(|| {
        tracing::error!(user_id = %user_id, "OAuth connection missing refresh_token (data inconsistency)");
        "OAuth connection has no refresh_token".to_string()
    })?;

    let refresh_token_plain = encryption
        .decrypt(refresh_token_bytes, conn.key_version as u32)
        .map_err(|e| format!("Failed to decrypt refresh token: {e}"))?;

    let token_resp = oauth_config
        .refresh_token(&refresh_token_plain)
        .await
        .map_err(|e| format!("{e}"))?;

    let new_access = &token_resp.access_token;

    // Encrypt new tokens
    let (enc_access, key_version) = encryption.encrypt(new_access);

    // If refresh_token came back, use it; otherwise keep the old one
    let enc_refresh: Option<Vec<u8>> = if let Some(ref new_rt) = token_resp.refresh_token {
        Some(encryption.encrypt(new_rt).0)
    } else {
        conn.refresh_token.clone()
    };

    // Update DB
    sqlx::query(
        "UPDATE pulsoid_connections
         SET access_token = $1, refresh_token = $2, key_version = $3,
             token_expires_at = now() + make_interval(secs => $4), last_error = NULL
         WHERE user_id = $5 AND source = 'oauth'",
    )
    .bind(&enc_access)
    .bind(&enc_refresh)
    .bind(key_version as i32)
    .bind(token_resp.expires_in as f64)
    .bind(user_id)
    .execute(db)
    .await
    .map_err(|e| format!("Failed to save refreshed tokens: {e}"))?;

    tracing::info!(user_id = %user_id, "Token refreshed successfully");
    Ok(new_access.clone())
}

async fn update_last_error(db: &PgPool, user_id: &str, error: &str) {
    let _ = sqlx::query("UPDATE pulsoid_connections SET last_error = $1 WHERE user_id = $2")
        .bind(error)
        .bind(user_id)
        .execute(db)
        .await;
}

async fn handle_message(
    db: &PgPool,
    redis: &redis::aio::MultiplexedConnection,
    hr_tx: &Sender<LatestHeartRateUpdate>,
    user_id: &str,
    text: &str,
) -> Result<(), String> {
    let msg: PulsoidMessage =
        serde_json::from_str(text).map_err(|e| format!("Parse error: {e}"))?;

    let bpm = msg.data.heart_rate;
    if !(20..=250).contains(&bpm) {
        return Err(format!("BPM {bpm} out of range (20-250)"));
    }

    let now = chrono_now();
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

    let update = LatestHeartRateUpdate {
        user_id: user_id.to_string(),
        bpm,
        recorded_at,
        received_at: now,
    };

    // Write to Redis
    let redis_value = serde_json::to_string(&update).unwrap();
    let key = format!("latest_bpm:{user_id}");
    let mut redis_conn = redis.clone();
    if let Err(e) = redis_conn.set::<_, _, ()>(&key, &redis_value).await {
        tracing::warn!(user_id = %user_id, "Failed to write to Redis: {e}");
    }

    // Broadcast to WebSocket subscribers (ignore error if no receivers)
    let _ = hr_tx.send(update);

    Ok(())
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
