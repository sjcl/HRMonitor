use futures_util::StreamExt;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use common::messages::{HeartRateReceived, TokenRefreshRequest, subjects};
use common::pulsoid_state::{WriteOutcome, classify_no_op};
use common::redis_keys::{LATEST_BPM_TTL_SECS, latest_bpm_key, serialize_latest_bpm};
use common::token_encryption::TokenEncryption;
use redis::AsyncCommands;

use crate::models::{PulsoidConnectionRow, PulsoidMessage, SOURCE_OAUTH};

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";
const REFRESH_SAFETY_MARGIN_SECS: i64 = 60;
/// Minimum interval between `TOKEN_REFRESH_NEEDED` publishes for a single
/// worker. Must be strictly greater than `max_backoff` (60s) so the cooldown
/// stays effective after backoff saturates; otherwise the worker would
/// re-publish every 60s indefinitely on non-terminal refresh failures.
const REFRESH_REQUEST_MIN_INTERVAL: Duration = Duration::from_secs(300);

/// Aborts the wrapped `JoinHandle` when dropped. Used so that the per-worker
/// NATS publish task (spawned at the top of `run_worker`) is cancelled on
/// every `run_worker` exit path — normal `return`, decrypt failure, stale
/// config_version, or external `WorkerManager::replace_worker` abort. Without
/// this, a detached spawn could outlive its parent worker and emit delayed
/// `hr.received` events after abort.
///
/// Cancellation is cooperative: `abort()` takes effect at the task's next
/// `.await` point. In practice that's sub-second (async-nats yields regularly),
/// but it is not a hard preemption guarantee.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn run_worker(
    db: PgPool,
    nats: async_nats::Client,
    mut redis: redis::aio::MultiplexedConnection,
    encryption: Arc<TokenEncryption>,
    user_id: String,
    config_version: i32,
) {
    // Decouple NATS `hr.received` publishing from the Pulsoid WS read loop.
    // Even without retries, `publish().await` can stall briefly while
    // async-nats is reconnecting or its write buffer is saturated; running
    // that inline inside `handle_message` would stop draining the WS socket
    // long enough for tungstenite to lose pongs and tear down the upstream
    // connection every time NATS hiccuped.
    //
    // `watch` gives us "capacity-1, newest-wins" semantics: if multiple
    // frames arrive while the publish task is blocked on a slow publish,
    // only the most recent is ever delivered. This matches `hr.received`'s
    // latest-live-state contract (api-backend just rebroadcasts; Redis
    // already has the value). A bounded `mpsc` would be wrong here —
    // `try_send` rejects the *newest* sender on `Full`, which would drain
    // stale frames FIFO on recovery.
    let (publish_tx, mut publish_rx) =
        tokio::sync::watch::channel::<Option<HeartRateReceived>>(None);
    let _publish_guard = {
        let nats = nats.clone();
        let user_id = user_id.clone();
        AbortOnDrop(tokio::spawn(async move {
            // Do NOT call `borrow_and_update()` here to "mark initial None as
            // seen": a fresh `watch::Receiver` already treats the initial value
            // as seen, and priming would race with an early producer `send`
            // and discard the first real frame.
            loop {
                if publish_rx.changed().await.is_err() {
                    // All senders dropped — worker exiting.
                    break;
                }
                // Clone out of the borrow immediately; holding it across
                // `.await` would deadlock concurrent `send` calls.
                let update = publish_rx.borrow_and_update().clone();
                let Some(update) = update else { continue };

                let payload = match serde_json::to_vec(&update) {
                    Ok(p) => p,
                    Err(e) => {
                        // `HeartRateReceived` can't actually fail to encode,
                        // but panicking here would break the AbortOnDrop
                        // invariant (the task would be gone before the
                        // producer noticed), so warn and continue instead.
                        tracing::warn!(
                            user_id = %user_id,
                            "Failed to serialize hr.received: {e}"
                        );
                        continue;
                    }
                };

                // Best-effort publish. `hr.received` is a live-notification
                // hint only: history is already in the DB and the latest value
                // is already in Redis, so dropping a frame is fine — the next
                // Pulsoid frame (1–2 s away) will re-deliver fresh state, and
                // api-backend's Redis self-heal covers WS clients in the
                // meantime. Retrying would only trade freshness for a stale
                // rebroadcast, which is the wrong tradeoff for live push.
                if let Err(e) = nats
                    .publish(subjects::HR_RECEIVED, payload.into())
                    .await
                {
                    tracing::warn!(
                        user_id = %user_id,
                        "Dropped hr.received publish (best-effort, next frame will refresh live state): {e}"
                    );
                }
            }
        }))
    };

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);
    // Cooldown state for `TOKEN_REFRESH_NEEDED` publishes. Monotonic clock so
    // wall-clock jumps can't bypass the cooldown. Local to this worker: a
    // successful refresh bumps `config_version` and tears down the worker,
    // and a terminal failure exits via `connection_state = 'error'`, so a
    // fresh worker always starts from `None` and may publish immediately.
    let mut last_refresh_request_at: Option<tokio::time::Instant> = None;

    loop {
        // Fetch connection from DB
        let conn: Option<PulsoidConnectionRow> = match sqlx::query_as(
            "SELECT source, access_token, key_version,
                    EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at,
                    last_error, connection_state, config_version
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
            if conn.connection_state == "error" {
                tracing::warn!(user_id = %user_id, last_error = ?conn.last_error,
                    "Row in terminal 'error' state, worker exiting. User must re-authorize.");
                // Best-effort refresh of `last_error`/`state_updated_at`. The
                // target state is 'error' so the sticky guard is disabled; a
                // zero-row result means the row was superseded (stale
                // config_version) or concurrently removed — either way we're
                // already about to `return`.
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
                    // Token expired or about to expire — request refresh from api-backend.
                    // Throttle re-publishes: on non-terminal refresh failures api-backend
                    // does not bump `config_version`, so without a cooldown the worker
                    // would re-publish every backoff cycle (pinned at 60s) forever.
                    let should_publish = match last_refresh_request_at {
                        None => true,
                        Some(at) => at.elapsed() >= REFRESH_REQUEST_MIN_INTERVAL,
                    };

                    if should_publish {
                        let req = TokenRefreshRequest {
                            user_id: user_id.to_string(),
                            config_version,
                        };
                        // Arm cooldown on entering the enqueue path — before we even try
                        // to serialize. Covers the serialize-failure case (publish never
                        // happened) and the publish-failure case with a single assignment,
                        // independent of which sub-step failed. Trade-off: if NATS recovers
                        // a few seconds after a publish failure, we still wait up to the
                        // full interval before retrying. This is intentional — we favor
                        // spam suppression over fast recovery from broker outages.
                        last_refresh_request_at = Some(tokio::time::Instant::now());

                        // Serialize fallibly: in practice `TokenRefreshRequest`
                        // (String + i32) can't fail to encode, but the fix's theme is
                        // "make the worker robust against transient failures", so a
                        // `.unwrap()` here would be out of place.
                        let published: bool = match serde_json::to_vec(&req) {
                            Ok(payload) => match nats
                                .publish(subjects::TOKEN_REFRESH_NEEDED, payload.into())
                                .await
                            {
                                Ok(()) => true,
                                Err(e) => {
                                    tracing::warn!(user_id = %user_id, "Failed to publish refresh_needed: {e}");
                                    false
                                }
                            },
                            Err(e) => {
                                tracing::warn!(user_id = %user_id, "Failed to serialize refresh request: {e}");
                                false
                            }
                        };

                        if published {
                            // Only on publish success do we claim "refresh requested" in
                            // the DB. This overwrites api-backend's prior last_error, but
                            // that's fine — we're starting a fresh refresh attempt.
                            match update_connection_state(
                                &db,
                                &user_id,
                                config_version,
                                "pending",
                                Some("Token expired, refresh requested"),
                            )
                            .await
                            {
                                Ok(WriteOutcome::Applied) => {}
                                Ok(WriteOutcome::StaleOrMissing) => {
                                    tracing::info!(
                                        user_id = %user_id,
                                        config_version,
                                        "Stale worker detected (config_version mismatch), exiting"
                                    );
                                    return;
                                }
                                Ok(WriteOutcome::StickyError) => {
                                    tracing::warn!(
                                        user_id = %user_id,
                                        config_version,
                                        "Refresh requested but row is in sticky error state, exiting"
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
                        }
                        // else: neither serialize nor publish succeeded. Deliberately do
                        // NOT touch the DB — we never sent the request, and writing
                        // "Token expired, refresh requested" would clobber api-backend's
                        // diagnostic last_error from a prior failed refresh.
                    } else {
                        // Cooldown active: don't re-publish and don't touch the DB. The
                        // worker still loops and re-fetches so it notices a successful
                        // refresh (config_version bump → stale-worker exit above).
                        tracing::debug!(
                            user_id = %user_id,
                            "Refresh request in cooldown, not re-publishing"
                        );
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
                    "UPDATE pulsoid_connections
                     SET last_connected_at = to_timestamp($1), last_error = NULL,
                         connection_state = 'connected', state_updated_at = now()
                     WHERE user_id = $2 AND config_version = $3
                       AND connection_state != 'error'",
                )
                .bind(now)
                .bind(&user_id)
                .bind(config_version)
                .execute(&db)
                .await
                {
                    Ok(result) if result.rows_affected() == 0 => {
                        // Disambiguate: stale config_version vs. sticky error
                        // state flipped by api-backend during WS connect.
                        match classify_no_op(&db, &user_id, config_version).await {
                            Ok(WriteOutcome::StaleOrMissing) | Ok(WriteOutcome::Applied) => {
                                tracing::info!(
                                    user_id = %user_id,
                                    config_version,
                                    "Stale worker detected (config_version mismatch), exiting"
                                );
                            }
                            Ok(WriteOutcome::StickyError) => {
                                tracing::warn!(
                                    user_id = %user_id,
                                    config_version,
                                    "Refused to mark connected: row in sticky error state, exiting"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    user_id = %user_id,
                                    config_version,
                                    "Failed to classify zero-row update: {e}; exiting"
                                );
                            }
                        }
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
                                handle_message(&db, &publish_tx, &mut redis, &user_id, &text).await
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
                    Ok(WriteOutcome::Applied) => {}
                    Ok(WriteOutcome::StaleOrMissing) => {
                        tracing::info!(
                            user_id = %user_id,
                            config_version,
                            "Stale worker detected (config_version mismatch), exiting"
                        );
                        return;
                    }
                    Ok(WriteOutcome::StickyError) => {
                        tracing::warn!(
                            user_id = %user_id,
                            config_version,
                            "WS disconnected and row is now in sticky error state, exiting"
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

/// Update `connection_state` for the worker's row.
///
/// Sticky-error guard: if the target `state` is `'error'` the write is
/// unconditional (within the usual `config_version` check). If the target is
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
    config_version: i32,
    state: &str,
    error: Option<&str>,
) -> Result<WriteOutcome, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE pulsoid_connections
         SET connection_state = $1, state_updated_at = now(), last_error = $2
         WHERE user_id = $3 AND config_version = $4
           AND ($1 = 'error' OR connection_state != 'error')",
    )
    .bind(state)
    .bind(error)
    .bind(user_id)
    .bind(config_version)
    .execute(db)
    .await?;

    if result.rows_affected() > 0 {
        Ok(WriteOutcome::Applied)
    } else {
        classify_no_op(db, user_id, config_version).await
    }
}

async fn handle_message(
    db: &PgPool,
    publish_tx: &tokio::sync::watch::Sender<Option<HeartRateReceived>>,
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

    // Hand off to the per-worker publish task. This is non-blocking: `watch`
    // overwrites any unread value, so intermediate frames collapse to the
    // latest during a slow NATS period. NATS I/O happens off the WS read
    // path so a stalled publish can never block tungstenite pongs. The only
    // `Err` shape is "all receivers dropped", which in the happy path is
    // unreachable — the receiver is owned by `run_worker`'s stack behind an
    // AbortOnDrop guard. Logged defensively so an unexpected early exit of
    // the publish task would not be silent.
    if let Err(e) = publish_tx.send(Some(update)) {
        tracing::warn!(user_id = %user_id, "hr.received publish task is gone: {e}");
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
/// Returns `true` if the worker should exit (stale config_version **or** the
/// row has since been flipped to sticky `'error'` by api-backend), `false`
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
        Ok(WriteOutcome::Applied) => false,
        Ok(WriteOutcome::StaleOrMissing) => {
            tracing::info!(
                user_id = %user_id,
                config_version,
                "Stale worker detected (config_version mismatch), exiting"
            );
            true
        }
        Ok(WriteOutcome::StickyError) => {
            tracing::warn!(
                user_id = %user_id,
                config_version,
                "Connection error persist refused: row is in sticky error state, exiting"
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
