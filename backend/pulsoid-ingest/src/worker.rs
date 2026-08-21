use futures_util::{Sink, SinkExt, Stream, StreamExt};
use sqlx::PgPool;
use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::{Bytes, Message, Utf8Bytes};

use common::messages::{HeartRateReceived, subjects};
use common::pulsoid_state::ConnectionState;
use common::redis_keys::{latest_bpm_key, latest_bpm_ttl_secs, serialize_latest_bpm};
use common::time::unix_now_secs;
use common::token_encryption::TokenEncryption;
use redis::AsyncCommands;

use crate::models::{PulsoidConnectionRow, PulsoidMessage, SOURCE_OAUTH};

const PULSOID_WS_URL: &str = "wss://dev.pulsoid.net/api/v1/data/real_time";
const HR_RECEIVED_PUBLISH_TIMEOUT: Duration = Duration::from_millis(100);
/// Worker-side expiry floor: if `token_expires_at` is within this many
/// seconds of `now()` the worker will NOT attempt a WS connect and will
/// instead back off until pulsoid-refresher bumps `revision`. This
/// is deliberately smaller than the refresher's own
/// `REFRESH_SAFETY_MARGIN_SECS` (300s) so that in steady state the
/// refresher always has a window to swap in a fresh token before the
/// worker gives up on the current one.
const REFRESH_SAFETY_MARGIN_SECS: i64 = 60;

/// How often the worker sends a WebSocket Ping control frame while a
/// connection is up. Pulsoid does not document Ping/Pong support, but any
/// RFC 6455 compliant endpoint must answer a Ping with a Pong. A missing
/// Pong on its own is NEVER treated as a dead connection — see
/// [`IDLE_TIMEOUT`].
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// If no Text, Binary, Ping or Pong frame arrives for this long, the
/// transport is considered silently dead and the session is torn down so the
/// existing reconnect path runs. Three times [`PING_INTERVAL`], so two
/// consecutive lost Pings are not enough to trigger a reconnect. This
/// measures *transport* silence, not heart-rate silence: a session that only
/// receives Pongs (sensor offline, socket healthy) is deliberately kept.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Upper bound on a single Ping send. On a half-open connection a write can
/// block forever once the send buffer fills; without this bound the watchdog
/// would stall inside its own keepalive.
const PING_SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run_worker(
    db: PgPool,
    nats: async_nats::Client,
    redis: redis::aio::ConnectionManager,
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
                        // 0 rows: stale `revision` (superseded) or the row was
                        // flipped to sticky 'error'. Either way this worker
                        // generation is done — no SELECT needed to tell which.
                        tracing::info!(
                            user_id = %user_id,
                            revision,
                            "Refused to mark connected (0 rows: superseded or sticky error), exiting"
                        );
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

                let (mut write, mut read) = ws_stream.split();
                // One handle clone per WebSocket session (not per frame): the
                // session loop needs to own what it writes to, while the
                // reconnect loop keeps using the originals.
                let mut handler = IngestTextHandler {
                    db: db.clone(),
                    nats: nats.clone(),
                    redis: redis.clone(),
                    user_id: user_id.clone(),
                };

                let reason = run_ws_session(&mut write, &mut read, &mut handler).await;
                match &reason {
                    DisconnectReason::ServerClose => {
                        tracing::info!(user_id = %user_id, "WebSocket closed by server");
                    }
                    DisconnectReason::StreamEnded => {
                        tracing::info!(user_id = %user_id, "WebSocket stream ended");
                    }
                    DisconnectReason::ReadError(error_msg) => {
                        tracing::warn!(user_id = %user_id, "WebSocket error: {error_msg}");
                    }
                    DisconnectReason::IdleTimeout { idle_secs } => {
                        tracing::warn!(
                            user_id = %user_id,
                            timeout_secs = IDLE_TIMEOUT.as_secs(),
                            idle_secs,
                            "No WebSocket frames received; transport looks silently dead, reconnecting"
                        );
                    }
                    DisconnectReason::PingFailed(error_msg) => {
                        tracing::warn!(
                            user_id = %user_id,
                            "Failed to send WebSocket ping: {error_msg}"
                        );
                    }
                    DisconnectReason::PingTimedOut => {
                        tracing::warn!(
                            user_id = %user_id,
                            timeout_secs = PING_SEND_TIMEOUT.as_secs(),
                            "WebSocket ping send timed out, reconnecting"
                        );
                    }
                }

                if persist_state_best_effort(
                    &db,
                    &user_id,
                    revision,
                    reason.next_state(),
                    Some(&reason.last_error()),
                )
                .await
                {
                    return;
                }
            }
            Err(error_msg) => {
                tracing::warn!(user_id = %user_id, "Failed to connect: {error_msg}");
                if persist_state_best_effort(
                    &db,
                    &user_id,
                    revision,
                    ConnectionState::Pending,
                    Some(&error_msg),
                )
                .await
                {
                    return;
                }
            }
        }

        tracing::info!(user_id = %user_id, backoff_secs = backoff.as_secs(), "Reconnecting after backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

/// Why a connected WebSocket session ended.
///
/// Every variant is recoverable: [`DisconnectReason::next_state`] is
/// `Pending` for all of them, so a transport failure never flips the row into
/// the terminal `'error'` state and never asks the user to re-authorize.
#[derive(Debug, PartialEq, Eq)]
enum DisconnectReason {
    /// The server sent a Close frame.
    ServerClose,
    /// Reading from the socket failed. Already sanitized.
    ReadError(String),
    /// The stream ended without a Close frame (EOF).
    StreamEnded,
    /// Nothing at all was received for [`IDLE_TIMEOUT`].
    IdleTimeout { idle_secs: u64 },
    /// Sending a keepalive Ping failed. Already sanitized.
    PingFailed(String),
    /// Sending a keepalive Ping did not finish within [`PING_SEND_TIMEOUT`].
    PingTimedOut,
}

impl DisconnectReason {
    /// State to persist before reconnecting. Always `Pending`: a dead
    /// transport is not a credential problem, so it must not become terminal.
    fn next_state(&self) -> ConnectionState {
        ConnectionState::Pending
    }

    /// Text for the `last_error` column. The three pre-existing paths keep
    /// their original wording; the watchdog paths are distinguishable.
    fn last_error(&self) -> String {
        match self {
            Self::ServerClose | Self::ReadError(_) | Self::StreamEnded => {
                "WebSocket disconnected, reconnecting".to_string()
            }
            Self::IdleTimeout { .. } => format!(
                "WebSocket idle timeout: no frames for {}s, reconnecting",
                IDLE_TIMEOUT.as_secs()
            ),
            Self::PingFailed(e) => format!("WebSocket ping failed, reconnecting: {e}"),
            Self::PingTimedOut => format!(
                "WebSocket ping send timed out after {}s, reconnecting",
                PING_SEND_TIMEOUT.as_secs()
            ),
        }
    }
}

/// Handles Pulsoid text frames. Abstracted so [`run_ws_session`] can be
/// tested without a database, Redis or NATS.
trait TextHandler {
    fn handle(&mut self, text: Utf8Bytes) -> impl Future<Output = ()> + Send;
}

/// Production [`TextHandler`]: forwards to [`handle_message`].
struct IngestTextHandler {
    db: PgPool,
    nats: async_nats::Client,
    redis: redis::aio::ConnectionManager,
    user_id: String,
}

impl TextHandler for IngestTextHandler {
    async fn handle(&mut self, text: Utf8Bytes) {
        if let Err(e) =
            handle_message(&self.db, &self.nats, &mut self.redis, &self.user_id, &text).await
        {
            tracing::warn!(user_id = %self.user_id, "Failed to handle message: {e}");
        }
    }
}

/// Drives a connected WebSocket until it dies, keeping a heartbeat on it.
///
/// Sends a Ping every [`PING_INTERVAL`] and gives up if *nothing* arrives for
/// [`IDLE_TIMEOUT`]. Received Pings are answered by tungstenite itself, which
/// queues the Pong and lets a later read/write/flush push it out — we never
/// send a Pong by hand; the periodic Ping doubles as that flush.
///
/// The caller owns logging and state persistence; this function only reports
/// why the session ended.
async fn run_ws_session<Si, St, E, H>(
    write: &mut Si,
    read: &mut St,
    handler: &mut H,
) -> DisconnectReason
where
    Si: Sink<Message> + Unpin,
    Si::Error: Display,
    St: Stream<Item = Result<Message, E>> + Unpin,
    E: Display,
    H: TextHandler + Send,
{
    // First tick one full interval from now, not immediately.
    let mut ping_timer = interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
    // A late tick must not be followed by a burst of catch-up Pings.
    ping_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_frame_at = Instant::now();

    loop {
        tokio::select! {
            msg = read.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    handler.handle(text).await;
                    // Stamped *after* the handler: time spent writing to the
                    // DB is not transport silence, so a slow handler must not
                    // make the next iteration look like a dead socket.
                    last_frame_at = Instant::now();
                }
                // Any data or control frame proves the transport is alive —
                // including a lone Pong while the sensor is offline.
                Some(Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_))) => {
                    last_frame_at = Instant::now();
                }
                Some(Ok(Message::Close(_))) => return DisconnectReason::ServerClose,
                Some(Ok(Message::Frame(_))) => {}
                Some(Err(e)) => {
                    return DisconnectReason::ReadError(sanitize_error(&e.to_string()));
                }
                None => return DisconnectReason::StreamEnded,
            },
            _ = ping_timer.tick() => {
                match tokio::time::timeout(
                    PING_SEND_TIMEOUT,
                    write.send(Message::Ping(Bytes::new())),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        return DisconnectReason::PingFailed(sanitize_error(&e.to_string()));
                    }
                    Err(_) => return DisconnectReason::PingTimedOut,
                }
            }
            // Rebuilt every iteration, so each received frame pushes the
            // deadline out by a full IDLE_TIMEOUT.
            _ = tokio::time::sleep_until(last_frame_at + IDLE_TIMEOUT) => {
                return DisconnectReason::IdleTimeout {
                    idle_secs: last_frame_at.elapsed().as_secs(),
                };
            }
        }
    }
}

/// Best-effort write of `connection_state` for the worker's row.
///
/// Sticky-error guard: the `($1 = 'error' OR connection_state != 'error')`
/// clause means a row already in the terminal `'error'` state is resurrected
/// only by a fresh re-auth (OAuth callback / manual token upload), never by a
/// worker state write. For `state = 'error'` calls the guard is disabled.
///
/// Returns `true` if the worker should exit. A zero-row result means the row
/// was superseded (stale `revision`), removed, or sits in sticky `'error'` —
/// in all three cases this worker generation is finished, so we deliberately
/// do NOT run a follow-up SELECT to tell them apart: the worker exits either
/// way, and the loop-head re-fetch already re-derives the precise reason on
/// the next iteration for any path that loops instead of returning. A DB
/// error returns `false` — the worker keeps going and the loop-head retries.
async fn persist_state_best_effort(
    db: &PgPool,
    user_id: &str,
    revision: i32,
    state: ConnectionState,
    error: Option<&str>,
) -> bool {
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
    .await;

    match result {
        Ok(r) if r.rows_affected() == 0 => {
            tracing::info!(
                user_id = %user_id,
                revision,
                state = %state,
                "Connection state write affected 0 rows (superseded or sticky error), exiting"
            );
            true
        }
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(
                user_id = %user_id,
                revision,
                state = %state,
                "Failed to persist connection state: {e}"
            );
            false
        }
    }
}

/// Best-effort write of the terminal `'error'` state. Thin wrapper over
/// [`persist_state_best_effort`]: every caller is already on its way out
/// (`return` / `continue`), so the should-exit signal is irrelevant here.
async fn persist_terminal_error_best_effort(
    db: &PgPool,
    user_id: &str,
    revision: i32,
    error: Option<&str>,
) {
    let _ = persist_state_best_effort(db, user_id, revision, ConnectionState::Error, error).await;
}

async fn handle_message(
    db: &PgPool,
    nats: &async_nats::Client,
    redis: &mut redis::aio::ConnectionManager,
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
    match tokio::time::timeout(
        HR_RECEIVED_PUBLISH_TIMEOUT,
        nats.publish(subjects::HR_RECEIVED, payload.into()),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(
                user_id = %user_id,
                "Dropped hr.received publish (best-effort, next frame will refresh live state): {e}"
            );
        }
        Err(_) => {
            tracing::warn!(
                user_id = %user_id,
                timeout_ms = HR_RECEIVED_PUBLISH_TIMEOUT.as_millis(),
                "Dropped hr.received publish after timeout (best-effort, next frame will refresh live state)"
            );
        }
    }

    Ok(())
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

/// Heartbeat watchdog tests.
///
/// These drive [`run_ws_session`] against a scripted stream and a recording
/// sink under `start_paused = true`, so the tokio clock auto-advances and a
/// 90-second timeout is verified in milliseconds of real time. No Pulsoid
/// API, no token, no socket.
#[cfg(test)]
mod watchdog_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::time::Sleep;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SinkMode {
        Ok,
        /// Every send fails immediately.
        Fail,
        /// Never becomes ready: models a write blocked on a full send buffer.
        Stall,
    }

    /// `Sink<Message>` that records what was sent and when (relative to its
    /// construction, i.e. to the start of the session).
    struct RecordingSink {
        start: Instant,
        sent: Vec<(Duration, Message)>,
        mode: SinkMode,
    }

    impl RecordingSink {
        fn new(mode: SinkMode) -> Self {
            Self {
                start: Instant::now(),
                sent: Vec::new(),
                mode,
            }
        }

        /// Seconds elapsed at which each Ping was sent.
        fn ping_offsets_secs(&self) -> Vec<u64> {
            self.sent
                .iter()
                .filter(|(_, m)| matches!(m, Message::Ping(_)))
                .map(|(at, _)| at.as_secs())
                .collect()
        }
    }

    impl Sink<Message> for RecordingSink {
        type Error = String;

        fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), String>> {
            match self.mode {
                SinkMode::Ok => Poll::Ready(Ok(())),
                SinkMode::Fail => Poll::Ready(Err("sink is broken".to_string())),
                SinkMode::Stall => Poll::Pending,
            }
        }

        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), String> {
            let this = self.get_mut();
            let at = this.start.elapsed();
            this.sent.push((at, item));
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), String>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), String>> {
            Poll::Ready(Ok(()))
        }
    }

    /// What the stream does once its script is exhausted.
    enum Tail {
        /// Stays pending forever: a silently dead transport.
        Silent,
        /// Yields `None`: EOF.
        End,
    }

    /// Replays `(gap, item)` pairs, where `gap` is the delay since the
    /// previous item.
    struct ScriptedStream {
        script: VecDeque<(Duration, Result<Message, String>)>,
        tail: Tail,
        sleep: Option<Pin<Box<Sleep>>>,
    }

    impl ScriptedStream {
        fn new(script: Vec<(u64, Result<Message, String>)>, tail: Tail) -> Self {
            Self {
                script: script
                    .into_iter()
                    .map(|(gap, item)| (Duration::from_secs(gap), item))
                    .collect(),
                tail,
                sleep: None,
            }
        }

        fn silent() -> Self {
            Self::new(Vec::new(), Tail::Silent)
        }
    }

    impl Stream for ScriptedStream {
        type Item = Result<Message, String>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            let gap = match this.script.front() {
                Some((gap, _)) => *gap,
                None => {
                    return match this.tail {
                        Tail::Silent => Poll::Pending,
                        Tail::End => Poll::Ready(None),
                    };
                }
            };
            let sleep = this
                .sleep
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(gap)));
            match sleep.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    this.sleep = None;
                    let (_, item) = this.script.pop_front().expect("front checked above");
                    Poll::Ready(Some(item))
                }
            }
        }
    }

    #[derive(Default)]
    struct RecordingTextHandler {
        texts: Vec<String>,
        /// Time each `handle` call blocks for, modelling a slow DB write.
        delay: Duration,
    }

    impl TextHandler for RecordingTextHandler {
        async fn handle(&mut self, text: Utf8Bytes) {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.texts.push(text.to_string());
        }
    }

    fn text(s: &str) -> Result<Message, String> {
        Ok(Message::Text(s.to_string().into()))
    }

    fn close() -> Result<Message, String> {
        Ok(Message::Close(None))
    }

    /// Runs a session and returns `(reason, sink, handler, elapsed_secs)`.
    async fn run(
        mode: SinkMode,
        stream: ScriptedStream,
    ) -> (DisconnectReason, RecordingSink, RecordingTextHandler, u64) {
        run_with_handler_delay(mode, stream, Duration::ZERO).await
    }

    async fn run_with_handler_delay(
        mode: SinkMode,
        mut stream: ScriptedStream,
        delay: Duration,
    ) -> (DisconnectReason, RecordingSink, RecordingTextHandler, u64) {
        let start = Instant::now();
        let mut sink = RecordingSink::new(mode);
        let mut handler = RecordingTextHandler {
            delay,
            ..Default::default()
        };
        let reason = run_ws_session(&mut sink, &mut stream, &mut handler).await;
        let elapsed = start.elapsed().as_secs();
        (reason, sink, handler, elapsed)
    }

    #[tokio::test(start_paused = true)]
    async fn pings_at_regular_interval() {
        // Text at 25/50/75/100s keeps the idle deadline far away (the 75s
        // frame pushes it to 165s), so the Ping cadence is unambiguous.
        let stream = ScriptedStream::new(
            vec![
                (25, text("hr")),
                (25, text("hr")),
                (25, text("hr")),
                (25, close()),
            ],
            Tail::End,
        );
        let (reason, sink, _, elapsed) = run(SinkMode::Ok, stream).await;

        assert_eq!(reason, DisconnectReason::ServerClose);
        // Nothing at t=0: the first tick is one full interval in.
        assert_eq!(sink.ping_offsets_secs(), vec![30, 60, 90]);
        assert_eq!(elapsed, 100);
    }

    #[tokio::test(start_paused = true)]
    async fn text_frames_keep_connection_alive() {
        // 60s < IDLE_TIMEOUT, for five minutes.
        let mut script: Vec<(u64, Result<Message, String>)> =
            (0..5).map(|_| (60, text("hr"))).collect();
        script.push((60, close()));
        let (reason, _, handler, elapsed) =
            run(SinkMode::Ok, ScriptedStream::new(script, Tail::End)).await;

        assert_eq!(reason, DisconnectReason::ServerClose);
        assert_eq!(handler.texts.len(), 5);
        assert_eq!(elapsed, 360);
    }

    #[tokio::test(start_paused = true)]
    async fn pong_frames_keep_connection_alive() {
        // Sensor offline: no heart-rate Text at all, only Pongs. The socket
        // is healthy, so the watchdog must not fire.
        let mut script: Vec<(u64, Result<Message, String>)> = (0..10)
            .map(|_| (30, Ok(Message::Pong(Bytes::new()))))
            .collect();
        script.push((30, close()));
        let (reason, _, handler, elapsed) =
            run(SinkMode::Ok, ScriptedStream::new(script, Tail::End)).await;

        assert_eq!(reason, DisconnectReason::ServerClose);
        assert!(handler.texts.is_empty());
        assert_eq!(elapsed, 330);
    }

    #[tokio::test(start_paused = true)]
    async fn binary_and_ping_count_as_activity() {
        let stream = ScriptedStream::new(
            vec![
                (60, Ok(Message::Binary(Bytes::new()))),
                (60, Ok(Message::Ping(Bytes::new()))),
                (60, close()),
            ],
            Tail::End,
        );
        let (reason, _, _, elapsed) = run(SinkMode::Ok, stream).await;

        assert_eq!(reason, DisconnectReason::ServerClose);
        assert_eq!(elapsed, 180);
    }

    #[tokio::test(start_paused = true)]
    async fn silence_triggers_watchdog() {
        let (reason, _, _, elapsed) = run(SinkMode::Ok, ScriptedStream::silent()).await;

        // Ping count is deliberately not asserted: at t=90s the third Ping
        // tick and the idle deadline are ready together and `select!` picks
        // randomly between them.
        match reason {
            DisconnectReason::IdleTimeout { idle_secs } => assert_eq!(idle_secs, 90),
            other => panic!("expected IdleTimeout, got {other:?}"),
        }
        assert_eq!(elapsed, 90);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_does_not_fire_just_before_timeout() {
        // A frame one second short of the deadline resets it in full.
        let stream = ScriptedStream::new(vec![(89, text("hr"))], Tail::Silent);
        let (reason, _, handler, elapsed) = run(SinkMode::Ok, stream).await;

        assert!(matches!(reason, DisconnectReason::IdleTimeout { .. }));
        assert_eq!(handler.texts, vec!["hr".to_string()]);
        assert_eq!(elapsed, 89 + 90);
    }

    #[tokio::test(start_paused = true)]
    async fn ping_send_failure_ends_session() {
        let (reason, _, _, elapsed) = run(SinkMode::Fail, ScriptedStream::silent()).await;

        assert_eq!(
            reason,
            DisconnectReason::PingFailed("sink is broken".to_string())
        );
        assert_eq!(elapsed, 30);
    }

    #[tokio::test(start_paused = true)]
    async fn ping_send_timeout_ends_session() {
        let (reason, _, _, elapsed) = run(SinkMode::Stall, ScriptedStream::silent()).await;

        assert_eq!(reason, DisconnectReason::PingTimedOut);
        // 30s to the first Ping + 10s waiting for the write to go through,
        // i.e. the watchdog does not sit in its own keepalive until 90s.
        assert_eq!(elapsed, 40);
    }

    #[tokio::test(start_paused = true)]
    async fn close_frame_ends_session() {
        let stream = ScriptedStream::new(vec![(5, close())], Tail::Silent);
        let (reason, _, _, elapsed) = run(SinkMode::Ok, stream).await;

        assert_eq!(reason, DisconnectReason::ServerClose);
        assert_eq!(elapsed, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn read_error_ends_session() {
        let stream = ScriptedStream::new(
            vec![(5, Err("connection reset by peer".to_string()))],
            Tail::Silent,
        );
        let (reason, _, _, elapsed) = run(SinkMode::Ok, stream).await;

        assert_eq!(
            reason,
            DisconnectReason::ReadError("connection reset by peer".to_string())
        );
        assert_eq!(elapsed, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn read_error_is_sanitized() {
        let stream = ScriptedStream::new(
            vec![(1, Err("handshake failed: Bearer secret123".to_string()))],
            Tail::Silent,
        );
        let (reason, _, _, _) = run(SinkMode::Ok, stream).await;

        match reason {
            DisconnectReason::ReadError(msg) => assert!(!msg.contains("secret123"), "{msg}"),
            other => panic!("expected ReadError, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stream_end_ends_session() {
        let (reason, _, _, elapsed) =
            run(SinkMode::Ok, ScriptedStream::new(Vec::new(), Tail::End)).await;

        assert_eq!(reason, DisconnectReason::StreamEnded);
        assert_eq!(elapsed, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn text_frames_are_dispatched_to_handler() {
        let stream = ScriptedStream::new(
            vec![(1, text("first")), (1, text("second")), (1, close())],
            Tail::End,
        );
        let (reason, _, handler, _) = run(SinkMode::Ok, stream).await;

        assert_eq!(reason, DisconnectReason::ServerClose);
        assert_eq!(
            handler.texts,
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ping_backlog_does_not_burst_after_a_stall() {
        // The Text branch body blocks the whole loop for 100s, so three Ping
        // ticks are missed. MissedTickBehavior::Delay must release one Ping
        // and then re-space the rest, not fire the backlog back to back.
        let stream = ScriptedStream::new(vec![(1, text("hr"))], Tail::Silent);
        let (reason, sink, _, _) =
            run_with_handler_delay(SinkMode::Ok, stream, Duration::from_secs(100)).await;

        // The slow handler itself must not be mistaken for a dead transport:
        // the idle deadline restarts when the handler returns (t=101s).
        match reason {
            DisconnectReason::IdleTimeout { idle_secs } => assert_eq!(idle_secs, 90),
            other => panic!("expected IdleTimeout, got {other:?}"),
        }
        let offsets = sink.ping_offsets_secs();
        assert_eq!(offsets[0], 101, "first Ping after the stall: {offsets:?}");
        for pair in offsets.windows(2) {
            assert!(
                pair[1] - pair[0] >= PING_INTERVAL.as_secs(),
                "pings burst after the stall: {offsets:?}"
            );
        }
    }

    #[test]
    fn all_disconnect_reasons_are_recoverable() {
        let reasons = [
            DisconnectReason::ServerClose,
            DisconnectReason::ReadError("io".to_string()),
            DisconnectReason::StreamEnded,
            DisconnectReason::IdleTimeout { idle_secs: 90 },
            DisconnectReason::PingFailed("io".to_string()),
            DisconnectReason::PingTimedOut,
        ];

        for reason in &reasons {
            // Never terminal: the row stays spawnable and no re-auth is asked
            // for, so WorkerManager reconnects on the existing backoff.
            assert_eq!(
                reason.next_state(),
                ConnectionState::Pending,
                "{reason:?} must stay recoverable"
            );
            assert!(!reason.last_error().is_empty());
        }

        // The pre-existing disconnect paths keep their original wording...
        let ordinary = DisconnectReason::ServerClose.last_error();
        assert_eq!(ordinary, "WebSocket disconnected, reconnecting");
        assert_eq!(DisconnectReason::StreamEnded.last_error(), ordinary);
        assert_eq!(
            DisconnectReason::ReadError("io".to_string()).last_error(),
            ordinary
        );

        // ...and the watchdog paths are distinguishable in `last_error`.
        let idle = DisconnectReason::IdleTimeout { idle_secs: 120 }.last_error();
        assert_ne!(idle, ordinary);
        assert!(idle.contains("idle timeout"), "{idle}");
        assert!(idle.contains("90s"), "{idle}");
        assert_ne!(DisconnectReason::PingTimedOut.last_error(), ordinary);
        assert_ne!(
            DisconnectReason::PingFailed("io".to_string()).last_error(),
            ordinary
        );
    }
}
