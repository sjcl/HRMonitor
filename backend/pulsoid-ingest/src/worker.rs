use futures_util::StreamExt;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use common::messages::{HeartRateReceived, subjects};
use common::pulsoid_state::{ConnectionState, WriteOutcome, classify_no_op};
use common::redis_keys::{latest_bpm_key, latest_bpm_ttl_secs, serialize_latest_bpm};
use common::time::unix_now_secs;
use common::token_encryption::TokenEncryption;
use redis::AsyncCommands;

use crate::models::{PulsoidConnectionRow, PulsoidMessage, SOURCE_OAUTH};

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";
/// Worker-side expiry floor: if `token_expires_at` is within this many
/// seconds of `now()` the worker will NOT attempt a WS connect and will
/// instead back off until pulsoid-refresher bumps `revision`. This
/// is deliberately smaller than the refresher's own
/// `REFRESH_SAFETY_MARGIN_SECS` (300s) so that in steady state the
/// refresher always has a window to swap in a fresh token before the
/// worker gives up on the current one.
const REFRESH_SAFETY_MARGIN_SECS: i64 = 60;

pub async fn run_worker(
    db: PgPool,
    nats: async_nats::Client,
    mut redis: redis::aio::MultiplexedConnection,
    encryption: Arc<TokenEncryption>,
    user_id: String,
    revision: i32,
) {
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
        // Fetch connection from DB
        let conn: Option<PulsoidConnectionRow> = match sqlx::query_as(
            "SELECT source, access_token, key_version,
                    EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at,
                    last_error, connection_state, revision
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

        if conn.revision != revision {
            tracing::info!(
                user_id = %user_id,
                worker_revision = revision,
                db_revision = conn.revision,
                "Stale worker detected (revision mismatch at fetch), exiting"
            );
            return;
        }

        // Decrypt access token
        let access_token = match encryption.decrypt(&conn.access_token, conn.key_version as u32) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(user_id = %user_id, "Failed to decrypt access token: {e}");
                persist_terminal_error_best_effort(
                    &db,
                    &user_id,
                    revision,
                    Some("Failed to decrypt access token"),
                )
                .await;
                return;
            }
        };

        // Check token expiry for OAuth connections. The worker is passive:
        // pulsoid-refresher proactively refreshes any OAuth token whose
        // `token_expires_at` is within its own (larger) safety margin, so
        // all we need to do here is refuse to (re)connect with a token that
        // is already too close to expiry and sleep. The refresher will bump
        // `revision` once it has swapped in a fresh token, at which
        // point the stale-version guard above tears this worker down and
        // WorkerManager spawns a new one.
        if conn.source == SOURCE_OAUTH {
            if conn.connection_state == ConnectionState::Error {
                tracing::warn!(user_id = %user_id, last_error = ?conn.last_error,
                    "Row in terminal 'error' state, worker exiting. User must re-authorize.");
                // Best-effort refresh of `last_error`/`state_updated_at`. The
                // target state is 'error' so the sticky guard is disabled; a
                // zero-row result means the row was superseded (stale
                // revision) or concurrently removed — either way we're
                // already about to `return`.
                persist_terminal_error_best_effort(
                    &db,
                    &user_id,
                    revision,
                    conn.last_error.as_deref(),
                )
                .await;
                return;
            }

            if let Some(expires_at) = conn.token_expires_at {
                let now = unix_now_secs();
                if now >= expires_at - REFRESH_SAFETY_MARGIN_SECS {
                    tracing::info!(
                        user_id = %user_id,
                        backoff_secs = backoff.as_secs(),
                        "Token expired; deferring WS connect — pulsoid-refresher will refresh on its next scan"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                    continue;
                }
            } else {
                tracing::error!(user_id = %user_id, "OAuth connection missing token_expires_at");
                persist_terminal_error_best_effort(
                    &db,
                    &user_id,
                    revision,
                    Some("OAuth connection missing expiry (data inconsistency)"),
                )
                .await;
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

                let now = unix_now_secs();
                match sqlx::query(
                    "UPDATE pulsoid_connections
                     SET last_connected_at = to_timestamp($1), last_error = NULL,
                         connection_state = 'connected', state_updated_at = now()
                     WHERE user_id = $2 AND revision = $3
                       AND connection_state != 'error'",
                )
                .bind(now)
                .bind(&user_id)
                .bind(revision)
                .execute(&db)
                .await
                {
                    Ok(result) if result.rows_affected() == 0 => {
                        classify_worker_zero_row_exit(
                            &db,
                            &user_id,
                            revision,
                            "Stale worker detected (revision mismatch), exiting",
                            "Refused to mark connected: row in sticky error state, exiting",
                        )
                        .await;
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            user_id = %user_id,
                            revision,
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

                if worker_state_write_should_exit(
                    update_connection_state(
                        &db,
                        &user_id,
                        revision,
                        ConnectionState::Pending,
                        Some("WebSocket disconnected, reconnecting"),
                    )
                    .await,
                    &user_id,
                    revision,
                    "Stale worker detected (revision mismatch), exiting",
                    "WS disconnected and row is now in sticky error state, exiting",
                    "Failed to set pending state after disconnect",
                ) {
                    return;
                }
            }
            Err(error_msg) => {
                tracing::warn!(user_id = %user_id, "Failed to connect: {error_msg}");
                if persist_pending_or_stale(&db, &user_id, revision, &error_msg).await {
                    return;
                }
            }
        }

        tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Reconnecting after backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

/// Update `connection_state` for the worker's row.
///
/// Sticky-error guard: if the target `state` is `'error'` the write is
/// unconditional (within the usual `revision` check). If the target is
/// `'pending'` or `'connected'` the WHERE clause additionally requires
/// `connection_state != 'error'` — a row already in the terminal state can
/// only be resurrected by a fresh re-auth (OAuth callback or manual token
/// upload), never by worker-side state updates.
///
/// When the write lands zero rows we disambiguate via [`classify_no_op`] so
/// callers can distinguish "stale / superseded" from "refused to resurrect
/// sticky error state". For `state = 'error'` calls the sticky branch is
/// logically unreachable (the guard is disabled), but the helper still runs
/// the classification to return a single, uniform `WriteOutcome`.
async fn update_connection_state(
    db: &PgPool,
    user_id: &str,
    revision: i32,
    state: ConnectionState,
    error: Option<&str>,
) -> Result<WriteOutcome, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE pulsoid_connections
         SET connection_state = $1, state_updated_at = now(), last_error = $2
         WHERE user_id = $3 AND revision = $4
           AND ($1 = 'error' OR connection_state != 'error')",
    )
    .bind(state)
    .bind(error)
    .bind(user_id)
    .bind(revision)
    .execute(db)
    .await?;

    if result.rows_affected() > 0 {
        Ok(WriteOutcome::Applied)
    } else {
        classify_no_op(db, user_id, revision).await
    }
}

async fn persist_terminal_error_best_effort(
    db: &PgPool,
    user_id: &str,
    revision: i32,
    error: Option<&str>,
) {
    if let Err(update_err) =
        update_connection_state(db, user_id, revision, ConnectionState::Error, error).await
    {
        tracing::warn!(
            user_id = %user_id,
            revision,
            "Failed to persist terminal error state: {update_err}"
        );
    }
}

async fn classify_worker_zero_row_exit(
    db: &PgPool,
    user_id: &str,
    revision: i32,
    stale_msg: &'static str,
    sticky_msg: &'static str,
) {
    match classify_no_op(db, user_id, revision).await {
        Ok(WriteOutcome::Applied) | Ok(WriteOutcome::StaleOrMissing) => {
            tracing::info!(user_id = %user_id, revision, "{stale_msg}");
        }
        Ok(WriteOutcome::StickyError) => {
            tracing::warn!(user_id = %user_id, revision, "{sticky_msg}");
        }
        Err(e) => {
            tracing::warn!(
                user_id = %user_id,
                revision,
                "Failed to classify zero-row update: {e}; exiting"
            );
        }
    }
}

fn worker_state_write_should_exit(
    outcome: Result<WriteOutcome, sqlx::Error>,
    user_id: &str,
    revision: i32,
    stale_msg: &'static str,
    sticky_msg: &'static str,
    error_msg: &'static str,
) -> bool {
    match outcome {
        Ok(WriteOutcome::Applied) => false,
        Ok(WriteOutcome::StaleOrMissing) => {
            tracing::info!(user_id = %user_id, revision, "{stale_msg}");
            true
        }
        Ok(WriteOutcome::StickyError) => {
            tracing::warn!(user_id = %user_id, revision, "{sticky_msg}");
            true
        }
        Err(e) => {
            tracing::warn!(user_id = %user_id, revision, "{error_msg}: {e}");
            false
        }
    }
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

    let now = unix_now_secs();
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

    // Anchor staleness on `recorded_at`, not `now`. A frame whose measurement is
    // already older than `LATEST_BPM_TTL_SECS` must not become "latest" — the DB
    // insert above preserves it for history, but we deliberately skip both the
    // Redis SET and the live broadcast so the snapshot/self-heal path doesn't
    // resurrect a stale reading. `latest_bpm_ttl_secs` returns `Some(full_ttl)`
    // for future timestamps (clock skew), so this `None` branch is guaranteed
    // `now >= recorded_at`.
    let ttl = match latest_bpm_ttl_secs(now, recorded_at) {
        Some(t) => t,
        None => {
            let age_secs = now - recorded_at;
            tracing::info!(
                user_id = %user_id,
                recorded_at,
                now,
                age_secs,
                "skipping latest_bpm SET and hr.received publish for stale measurement"
            );
            return Ok(());
        }
    };

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
    if let Err(e) = redis.set_ex::<_, _, ()>(&key, &value, ttl).await {
        return Err(format!(
            "latest_bpm Redis write failed after DB insert: {e}"
        ));
    }

    let payload = match serde_json::to_vec(&update) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(user_id = %user_id, "Failed to serialize hr.received: {e}");
            return Ok(());
        }
    };

    // Best-effort publish. `hr.received` is a live-notification hint only:
    // history is already in the DB and the latest value is already in Redis.
    // Dropping this frame is fine; the next Pulsoid frame refreshes live state.
    if let Err(e) = nats.publish(subjects::HR_RECEIVED, payload.into()).await {
        tracing::warn!(
            user_id = %user_id,
            "Dropped hr.received publish (best-effort, next frame will refresh live state): {e}"
        );
    }

    Ok(())
}

/// Update the connection to `pending` with a pre-sanitized error message.
/// Returns `true` if the worker should exit (stale revision **or** the
/// row has since been flipped to sticky `'error'` by api-backend), `false`
/// otherwise. DB errors are logged and treated as "continue" to match the
/// existing behavior. Callers remain responsible for sleeping/backing off
/// — this helper intentionally does not touch backoff.
async fn persist_pending_or_stale(
    db: &PgPool,
    user_id: &str,
    revision: i32,
    error_msg: &str,
) -> bool {
    worker_state_write_should_exit(
        update_connection_state(
            db,
            user_id,
            revision,
            ConnectionState::Pending,
            Some(error_msg),
        )
        .await,
        user_id,
        revision,
        "Stale worker detected (revision mismatch), exiting",
        "Connection error persist refused: row is in sticky error state, exiting",
        "Failed to set pending state after connection error",
    )
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
    let prefix_lower = prefix.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].to_ascii_lowercase().find(&prefix_lower) {
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
    fn redacts_bearer_case_insensitive() {
        let input = "header: bearer abc123 and BEARER def456";
        let out = sanitize_error(input);
        assert!(!out.contains("abc123"), "lowercase bearer leaked: {out}");
        assert!(!out.contains("def456"), "uppercase BEARER leaked: {out}");
    }

    #[test]
    fn handles_prefix_with_no_value() {
        // "Bearer " followed immediately by a delimiter / end — nothing to redact
        let input = "Bearer ";
        let out = sanitize_error(input);
        assert_eq!(out, "Bearer ");
    }
}
