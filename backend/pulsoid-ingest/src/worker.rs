use futures_util::StreamExt;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use common::messages::{HeartRateReceived, TokenRefreshRequest, subjects};
use common::redis_keys::{LATEST_BPM_TTL_SECS, latest_bpm_key, serialize_latest_bpm};
use common::token_encryption::TokenEncryption;
use redis::AsyncCommands;

use crate::models::{PulsoidConnectionRow, PulsoidMessage, SOURCE_OAUTH};

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";
const REFRESH_SAFETY_MARGIN_SECS: i64 = 60;

pub async fn run_worker(
    db: PgPool,
    nats: async_nats::Client,
    mut redis: redis::aio::MultiplexedConnection,
    encryption: Arc<TokenEncryption>,
    user_id: String,
    config_version: i32,
) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
        // Fetch connection from DB
        let conn: Option<PulsoidConnectionRow> = match sqlx::query_as(
            "SELECT source, access_token, key_version,
                    EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at,
                    last_error, refresh_blocked, config_version
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

        if conn.config_version != config_version {
            tracing::info!(
                user_id = %user_id,
                worker_version = config_version,
                db_version = conn.config_version,
                "Stale worker detected (config_version mismatch at fetch), exiting"
            );
            return;
        }

        // Decrypt access token
        let access_token = match encryption.decrypt(&conn.access_token, conn.key_version as u32) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(user_id = %user_id, "Failed to decrypt access token: {e}");
                if let Err(update_err) = update_connection_state(
                    &db,
                    &user_id,
                    config_version,
                    "error",
                    Some("Failed to decrypt access token"),
                )
                .await
                {
                    tracing::warn!(
                        user_id = %user_id,
                        config_version,
                        "Failed to persist terminal error state: {update_err}"
                    );
                }
                return;
            }
        };

        // Check token expiry for OAuth connections — request refresh via NATS
        if conn.source == SOURCE_OAUTH {
            if conn.refresh_blocked {
                tracing::warn!(user_id = %user_id, last_error = ?conn.last_error,
                    "Refresh blocked due to terminal failure, worker exiting. User must re-authorize.");
                if let Err(update_err) = update_connection_state(
                    &db,
                    &user_id,
                    config_version,
                    "error",
                    conn.last_error.as_deref(),
                )
                .await
                {
                    tracing::warn!(
                        user_id = %user_id,
                        config_version,
                        "Failed to persist terminal error state: {update_err}"
                    );
                }
                return;
            }

            if let Some(expires_at) = conn.token_expires_at {
                let now = system_now();
                if now >= expires_at - REFRESH_SAFETY_MARGIN_SECS {
                    // Token expired or about to expire — request refresh from api-backend
                    let req = TokenRefreshRequest {
                        user_id: user_id.to_string(),
                        config_version,
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
                    match update_connection_state(
                        &db,
                        &user_id,
                        config_version,
                        "pending",
                        Some("Token expired, refresh requested"),
                    )
                    .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::info!(
                                user_id = %user_id,
                                config_version,
                                "Stale worker detected (config_version mismatch), exiting"
                            );
                            return;
                        }
                        Err(e) => {
                            tracing::warn!(
                                user_id = %user_id,
                                config_version,
                                "Failed to set pending state after refresh request: {e}"
                            );
                        }
                    }
                    tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Token expired, waiting for refresh");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                    continue;
                }
            } else {
                tracing::error!(user_id = %user_id, "OAuth connection missing token_expires_at");
                if let Err(update_err) = update_connection_state(
                    &db,
                    &user_id,
                    config_version,
                    "error",
                    Some("OAuth connection missing expiry (data inconsistency)"),
                )
                .await
                {
                    tracing::warn!(
                        user_id = %user_id,
                        config_version,
                        "Failed to persist terminal error state: {update_err}"
                    );
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        }

        tracing::info!(user_id = %user_id, "Connecting to Pulsoid WebSocket");

        // Build WS request with Authorization: Bearer header so the token is
        // NEVER embedded in the URL (tungstenite errors may include the URL).
        let request_result = PULSOID_WS_URL
            .into_client_request()
            .map_err(|e| sanitize_error(&format!("Invalid WS request: {e}")))
            .and_then(|mut req| {
                let value = format!("Bearer {access_token}")
                    .parse()
                    .map_err(|e| sanitize_error(&format!("Invalid Authorization header: {e}")))?;
                req.headers_mut().insert(AUTHORIZATION, value);
                Ok(req)
            });

        let connect_result = match request_result {
            Ok(req) => connect_async(req)
                .await
                .map_err(|e| sanitize_error(&format!("{e}"))),
            Err(msg) => Err(msg),
        };

        match connect_result {
            Ok((ws_stream, _)) => {
                backoff = Duration::from_secs(1);

                let now = system_now();
                match sqlx::query(
                    "UPDATE pulsoid_connections SET last_connected_at = to_timestamp($1), last_error = NULL, connection_state = 'connected', state_updated_at = now() WHERE user_id = $2 AND config_version = $3",
                )
                .bind(now)
                .bind(&user_id)
                .bind(config_version)
                .execute(&db)
                .await
                {
                    Ok(result) if result.rows_affected() == 0 => {
                        tracing::info!(
                            user_id = %user_id,
                            config_version,
                            "Stale worker detected (config_version mismatch), exiting"
                        );
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            user_id = %user_id,
                            config_version,
                            "Failed to set connected state: {e}"
                        );
                    }
                }

                tracing::info!(user_id = %user_id, "Connected to Pulsoid WebSocket");

                let (_, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if let Err(e) =
                                handle_message(&db, &nats, &mut redis, &user_id, &text).await
                            {
                                tracing::warn!(user_id = %user_id, "Failed to handle message: {e}");
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            tracing::info!(user_id = %user_id, "WebSocket closed by server");
                            break;
                        }
                        Err(e) => {
                            let error_msg = sanitize_error(&format!("{e}"));
                            tracing::warn!(user_id = %user_id, "WebSocket error: {error_msg}");
                            break;
                        }
                        _ => {}
                    }
                }

                match update_connection_state(
                    &db,
                    &user_id,
                    config_version,
                    "pending",
                    Some("WebSocket disconnected, reconnecting"),
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::info!(
                            user_id = %user_id,
                            config_version,
                            "Stale worker detected (config_version mismatch), exiting"
                        );
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(
                            user_id = %user_id,
                            config_version,
                            "Failed to set pending state after disconnect: {e}"
                        );
                    }
                }
            }
            Err(error_msg) => {
                tracing::warn!(user_id = %user_id, "Failed to connect: {error_msg}");
                if persist_pending_or_stale(&db, &user_id, config_version, &error_msg).await {
                    return;
                }
            }
        }

        tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Reconnecting after backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

async fn update_connection_state(
    db: &PgPool,
    user_id: &str,
    config_version: i32,
    state: &str,
    error: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE pulsoid_connections SET connection_state = $1, state_updated_at = now(), last_error = $2 WHERE user_id = $3 AND config_version = $4",
    )
    .bind(state)
    .bind(error)
    .bind(user_id)
    .bind(config_version)
    .execute(db)
    .await?;

    Ok(result.rows_affected() > 0)
}

async fn handle_message(
    db: &PgPool,
    nats: &async_nats::Client,
    redis: &mut redis::aio::MultiplexedConnection,
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

    // Write to Redis latest_bpm cache with TTL. This is the authoritative
    // write — api-backend's read_snapshot and WS self-heal read only from
    // here. If this fails we must skip the NATS publish below: otherwise
    // connected clients receive the live Update and then get rolled back
    // to the stale Redis value (or null) on the next self-heal, and new
    // subscribers never see this reading at all. The DB insert above has
    // already committed the historical record, so this is a partial
    // success — the next Pulsoid frame re-establishes live state once
    // Redis recovers.
    let key = latest_bpm_key(user_id);
    let value = serialize_latest_bpm(&update);
    if let Err(e) = redis
        .set_ex::<_, _, ()>(&key, &value, LATEST_BPM_TTL_SECS)
        .await
    {
        return Err(format!("latest_bpm Redis write failed after DB insert: {e}"));
    }

    // Publish to NATS for api-backend to broadcast via WebSocket (best-effort).
    let payload = serde_json::to_vec(&update).unwrap();
    let retries = [
        None,
        Some(Duration::from_millis(100)),
        Some(Duration::from_millis(500)),
    ];
    let last = retries.len() - 1;
    for (i, delay) in retries.into_iter().enumerate() {
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        match nats
            .publish(subjects::HR_RECEIVED, payload.clone().into())
            .await
        {
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

/// Update the connection to `pending` with a pre-sanitized error message.
/// Returns `true` if the worker should exit (stale config_version), `false`
/// otherwise. DB errors are logged and treated as "continue" to match the
/// existing behavior. Callers remain responsible for sleeping/backing off
/// — this helper intentionally does not touch backoff.
async fn persist_pending_or_stale(
    db: &PgPool,
    user_id: &str,
    config_version: i32,
    error_msg: &str,
) -> bool {
    match update_connection_state(db, user_id, config_version, "pending", Some(error_msg)).await {
        Ok(true) => false,
        Ok(false) => {
            tracing::info!(
                user_id = %user_id,
                config_version,
                "Stale worker detected (config_version mismatch), exiting"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                user_id = %user_id,
                config_version,
                "Failed to set pending state after connection error: {e}"
            );
            false
        }
    }
}

/// Redact any Pulsoid access tokens that may have leaked into an error
/// string before it is logged or persisted to `last_error`. Defense in depth:
/// the primary protection is that we no longer embed the token in the URL.
fn sanitize_error(error: &str) -> String {
    let mut s = error.to_string();
    redact_all(&mut s, "access_token=");
    redact_all(&mut s, "Bearer ");
    s
}

fn redact_all(s: &mut String, prefix: &str) {
    const PLACEHOLDER: &str = "[REDACTED]";
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find(prefix) {
        let value_start = search_from + rel + prefix.len();
        let value_end = s[value_start..]
            .find(|c: char| matches!(c, '&' | '"' | '\'' | ']' | ')') || c.is_whitespace())
            .map(|i| value_start + i)
            .unwrap_or(s.len());
        if value_end == value_start {
            // No value to redact; advance past the prefix to avoid looping.
            search_from = value_start;
            continue;
        }
        s.replace_range(value_start..value_end, PLACEHOLDER);
        search_from = value_start + PLACEHOLDER.len();
        if search_from >= s.len() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_error;

    #[test]
    fn redacts_access_token_query_param() {
        let input =
            "WS error: wss://dev.pulsoid.net/api/v1/data/real_time?access_token=abc123&other=1";
        let out = sanitize_error(input);
        assert!(!out.contains("abc123"), "token leaked: {out}");
        assert!(out.contains("access_token=[REDACTED]"));
        assert!(out.contains("&other=1"));
    }

    #[test]
    fn redacts_bearer_token_at_end_of_string() {
        let input = "Invalid Authorization header: Bearer abc123";
        let out = sanitize_error(input);
        assert_eq!(out, "Invalid Authorization header: Bearer [REDACTED]");
    }

    #[test]
    fn redacts_multiple_access_token_occurrences() {
        let input = "access_token=aaa something access_token=bbb end";
        let out = sanitize_error(input);
        assert!(!out.contains("aaa"));
        assert!(!out.contains("bbb"));
        assert_eq!(
            out,
            "access_token=[REDACTED] something access_token=[REDACTED] end"
        );
    }

    #[test]
    fn redacts_mixed_bearer_and_query_string() {
        let input = "url=wss://x?access_token=aaa. Bearer bbb";
        let out = sanitize_error(input);
        assert!(!out.contains("aaa"));
        assert!(!out.contains("bbb"));
        assert!(out.contains("access_token=[REDACTED]"));
        assert!(out.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn no_match_returns_unchanged() {
        let input = "generic IO error: connection refused";
        assert_eq!(sanitize_error(input), input);
    }

    #[test]
    fn empty_string_is_unchanged() {
        assert_eq!(sanitize_error(""), "");
    }

    #[test]
    fn handles_prefix_with_no_value() {
        // "Bearer " followed immediately by a delimiter / end — nothing to redact
        let input = "Bearer ";
        let out = sanitize_error(input);
        assert_eq!(out, "Bearer ");
    }
}
