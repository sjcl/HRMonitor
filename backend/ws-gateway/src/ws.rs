use axum::Extension;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, close_code};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use redis::{AsyncCommands, RedisError};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::time::{Duration, Interval, interval};

use crate::WsState;
use common::access::ViewableUserId;

use common::access::{ensure_active_member, ensure_can_view_user};
use common::auth::AuthenticatedUser;
use common::error::AppError;
use common::messages::HeartRateReceived;
use common::redis_keys::latest_bpm_key;
use common::visibility::values::PRIVATE;

/// Close frame sent by every WS handler when the shutdown token fires.
/// Shared so production code and tests stay locked on the same 1001 + reason.
pub(crate) fn shutdown_close_frame() -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::AWAY,
        reason: Utf8Bytes::from_static("server shutting down"),
    }))
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsServerMessage {
    Snapshot {
        data: HashMap<String, Option<HeartRateReceived>>,
    },
    Update {
        data: HeartRateReceived,
    },
}

// ---------------------------------------------------------------------------
// /api/ws/me — own heart rate (no reauth needed)
// ---------------------------------------------------------------------------

pub async fn my_heart_rate_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WsState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    let user_id = auth_user.id.clone();
    ws.on_upgrade(move |socket| handle_single_user_ws(socket, state, user_id, None))
}

// ---------------------------------------------------------------------------
// /api/ws/users/{id} — specific user's heart rate (reauth every 30s)
// ---------------------------------------------------------------------------

pub async fn user_heart_rate_ws(
    ws: WebSocketUpgrade,
    ViewableUserId(target_id): ViewableUserId,
    State(state): State<Arc<WsState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, AppError> {
    let reauth = if auth_user.id == target_id {
        None
    } else {
        Some(auth_user)
    };
    Ok(ws.on_upgrade(move |socket| handle_single_user_ws(socket, state, target_id, reauth)))
}

// ---------------------------------------------------------------------------
// /api/ws/groups/{id} — group heart rates (reauth every 30s)
// ---------------------------------------------------------------------------

pub async fn group_heart_rate_ws(
    ws: WebSocketUpgrade,
    Path(group_id): Path<String>,
    State(state): State<Arc<WsState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, AppError> {
    ensure_active_member(&state.db, &group_id, &auth_user.id).await?;
    Ok(ws.on_upgrade(move |socket| handle_group_ws(socket, state, auth_user, group_id)))
}

// ---------------------------------------------------------------------------
// Internal: single-user WebSocket loop (used by /me and /users/{id})
// ---------------------------------------------------------------------------

async fn handle_single_user_ws(
    socket: WebSocket,
    state: Arc<WsState>,
    target_user_id: String,
    reauth: Option<AuthenticatedUser>, // None = self, skip reauth
) {
    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = state.hr_broadcast.subscribe();

    let snap = match read_snapshot(&state, std::slice::from_ref(&target_user_id)).await {
        Ok(snap) => snap,
        Err(e) => {
            warn_snapshot_error("single_user initial", &e);
            return;
        }
    };
    let mut last_sent: Option<HeartRateReceived> =
        snap.get(&target_user_id).and_then(|hr| hr.clone());
    let initial = WsServerMessage::Snapshot { data: snap };
    if !send_ws_message(&mut sender, &initial).await {
        return;
    }

    let (mut reauth_interval, mut self_heal_interval, mut ping_interval) =
        make_ws_intervals().await;

    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                let _ = sender.send(shutdown_close_frame()).await;
                break;
            }
            should_disconnect = poll_receiver_or_ping(
                &mut receiver, &mut sender, &mut ping_interval,
            ) => {
                if should_disconnect { break; }
            }
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if update.user_id == target_user_id {
                            last_sent = Some(update.clone());
                            let msg = WsServerMessage::Update { data: update };
                            if !send_ws_message(&mut sender, &msg).await { break; }
                        }
                    }
                    Err(e) => if broadcast_error_should_break(e) { break; }
                }
            }
            _ = reauth_interval.tick() => {
                if let Some(ref auth_user) = reauth
                    && ensure_can_view_user(&state.db, auth_user, &target_user_id)
                        .await
                        .is_err()
                {
                    break;
                }
            }
            _ = self_heal_interval.tick() => {
                let snap = match read_snapshot(&state, std::slice::from_ref(&target_user_id)).await {
                    Ok(snap) => snap,
                    Err(e) => {
                        warn_snapshot_error("single_user self_heal", &e);
                        continue;
                    }
                };
                match snap.get(&target_user_id) {
                    Some(Some(hr)) => {
                        if last_sent.as_ref() != Some(hr) {
                            last_sent = Some(hr.clone());
                            let msg = WsServerMessage::Update { data: hr.clone() };
                            if !send_ws_message(&mut sender, &msg).await { break; }
                        }
                    }
                    Some(None) | None => {
                        if last_sent.is_some() {
                            last_sent = None;
                            let mut data: HashMap<String, Option<HeartRateReceived>> =
                                HashMap::with_capacity(1);
                            data.insert(target_user_id.clone(), None);
                            let msg = WsServerMessage::Snapshot { data };
                            if !send_ws_message(&mut sender, &msg).await { break; }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal: group WebSocket loop
// ---------------------------------------------------------------------------

async fn handle_group_ws(
    socket: WebSocket,
    state: Arc<WsState>,
    auth_user: AuthenticatedUser,
    group_id: String,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = state.hr_broadcast.subscribe();

    let mut members: HashSet<String> =
        match fetch_sharing_members(&state.db, &group_id, &auth_user.id).await {
            Ok(m) => m,
            Err(_) => return,
        };

    let mut last_sent: HashMap<String, HeartRateReceived> = HashMap::new();

    let user_ids: Vec<String> = members.iter().cloned().collect();
    let snap = match read_snapshot(&state, &user_ids).await {
        Ok(snap) => snap,
        Err(e) => {
            warn_snapshot_error("group initial", &e);
            return;
        }
    };
    for (uid, entry) in &snap {
        if let Some(hr) = entry {
            last_sent.insert(uid.clone(), hr.clone());
        }
    }
    let initial = WsServerMessage::Snapshot { data: snap };
    if !send_ws_message(&mut sender, &initial).await {
        return;
    }

    let (mut reauth_interval, mut self_heal_interval, mut ping_interval) =
        make_ws_intervals().await;

    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                let _ = sender.send(shutdown_close_frame()).await;
                break;
            }
            should_disconnect = poll_receiver_or_ping(
                &mut receiver, &mut sender, &mut ping_interval,
            ) => {
                if should_disconnect { break; }
            }
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if members.contains(&update.user_id) {
                            last_sent.insert(update.user_id.clone(), update.clone());
                            let msg = WsServerMessage::Update { data: update };
                            if !send_ws_message(&mut sender, &msg).await { break; }
                        }
                    }
                    Err(e) => if broadcast_error_should_break(e) { break; }
                }
            }
            _ = reauth_interval.tick() => {
                let new_members: HashSet<String> = match fetch_sharing_members(&state.db, &group_id, &auth_user.id).await {
                    Ok(m) => m,
                    Err(_) => break,
                };

                if !new_members.contains(&auth_user.id) {
                    break;
                }

                let removed: Vec<String> = members.difference(&new_members).cloned().collect();
                if !removed.is_empty() {
                    let mut removal_data: HashMap<String, Option<HeartRateReceived>> =
                        HashMap::with_capacity(removed.len());
                    for uid in &removed {
                        last_sent.remove(uid);
                        removal_data.insert(uid.clone(), None::<HeartRateReceived>);
                    }
                    let msg = WsServerMessage::Snapshot { data: removal_data };
                    if !send_ws_message(&mut sender, &msg).await { break; }
                }

                let added: Vec<String> = new_members.difference(&members).cloned().collect();
                if !added.is_empty() {
                    match read_snapshot(&state, &added).await {
                        Ok(snap) => {
                            for (uid, entry) in &snap {
                                if let Some(hr) = entry {
                                    last_sent.insert(uid.clone(), hr.clone());
                                }
                            }
                            let msg = WsServerMessage::Snapshot { data: snap };
                            if !send_ws_message(&mut sender, &msg).await { break; }
                        }
                        Err(e) => warn_snapshot_error("group added_members", &e),
                    }
                }

                members = new_members;
            }
            _ = self_heal_interval.tick() => {
                let user_ids: Vec<String> = members.iter().cloned().collect();
                let snap = match read_snapshot(&state, &user_ids).await {
                    Ok(snap) => snap,
                    Err(e) => {
                        warn_snapshot_error("group self_heal", &e);
                        continue;
                    }
                };
                let mut diffs: HashMap<String, Option<HeartRateReceived>> = HashMap::new();
                for (uid, entry) in snap {
                    match entry {
                        Some(hr) => {
                            if last_sent.get(&uid) != Some(&hr) {
                                last_sent.insert(uid.clone(), hr.clone());
                                diffs.insert(uid, Some(hr));
                            }
                        }
                        None => {
                            if last_sent.remove(&uid).is_some() {
                                diffs.insert(uid, None);
                            }
                        }
                    }
                }
                if !diffs.is_empty() {
                    let msg = WsServerMessage::Snapshot { data: diffs };
                    if !send_ws_message(&mut sender, &msg).await { break; }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn make_ws_intervals() -> (Interval, Interval, Interval) {
    let mut reauth = interval(Duration::from_secs(30));
    reauth.tick().await;
    let mut self_heal = interval(Duration::from_secs(10));
    self_heal.tick().await;
    let mut ping = interval(Duration::from_secs(30));
    ping.tick().await;
    (reauth, self_heal, ping)
}

/// Serialize `msg` and send it. Returns `false` iff the socket send failed —
/// caller should `break` the loop. Returns `true` on success and also on
/// `serde_json::to_string` failure, preserving the original
/// `if let Ok(json) = … && sender.send(…).is_err() { break }` semantics that
/// silently skip unsendable messages rather than tearing down the connection.
async fn send_ws_message(
    sender: &mut SplitSink<WebSocket, Message>,
    msg: &WsServerMessage,
) -> bool {
    let Ok(json) = serde_json::to_string(msg) else {
        return true;
    };
    sender.send(Message::Text(json.into())).await.is_ok()
}

async fn poll_receiver_or_ping(
    receiver: &mut SplitStream<WebSocket>,
    sender: &mut SplitSink<WebSocket, Message>,
    ping_interval: &mut Interval,
) -> bool {
    tokio::select! {
        msg = receiver.next() => {
            matches!(msg, Some(Ok(Message::Close(_))) | None)
        }
        _ = ping_interval.tick() => {
            sender.send(Message::Ping(Default::default())).await.is_err()
        }
    }
}

fn broadcast_error_should_break(err: tokio::sync::broadcast::error::RecvError) -> bool {
    match err {
        tokio::sync::broadcast::error::RecvError::Lagged(n) => {
            tracing::warn!("WebSocket broadcast lagged by {n} messages");
            false
        }
        tokio::sync::broadcast::error::RecvError::Closed => true,
    }
}

async fn fetch_sharing_members(
    db: &sqlx::PgPool,
    group_id: &str,
    auth_user_id: &str,
) -> Result<HashSet<String>, AppError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT gm.user_id FROM group_members gm \
         JOIN users u ON u.id = gm.user_id \
         WHERE gm.group_id = $1 AND gm.status = 'active' \
           AND (gm.sharing = true OR gm.user_id = $2) \
           AND (u.heart_rate_visibility != $3 OR gm.user_id = $2)",
    )
    .bind(group_id)
    .bind(auth_user_id)
    .bind(PRIVATE)
    .fetch_all(db)
    .await?;

    let set: HashSet<String> = rows.into_iter().map(|(uid,)| uid).collect();

    if !set.contains(auth_user_id) {
        return Err(AppError::NotFound("Group not found".into()));
    }

    Ok(set)
}

async fn read_snapshot(
    state: &WsState,
    user_ids: &[String],
) -> redis::RedisResult<HashMap<String, Option<HeartRateReceived>>> {
    let mut results: HashMap<String, Option<HeartRateReceived>> =
        HashMap::with_capacity(user_ids.len());

    if user_ids.is_empty() {
        return Ok(results);
    }

    let keys: Vec<String> = user_ids.iter().map(|id| latest_bpm_key(id)).collect();
    let mut redis = state.redis.clone();

    let values = redis.mget::<_, Vec<Option<String>>>(&keys).await?;
    for (user_id, value) in user_ids.iter().zip(values) {
        let entry = match value {
            Some(s) => match serde_json::from_str::<HeartRateReceived>(&s) {
                Ok(hr) => Some(hr),
                Err(e) => {
                    tracing::warn!(
                        user_id = %user_id,
                        error = %e,
                        "failed to parse latest_bpm payload; treating as miss"
                    );
                    None
                }
            },
            None => None,
        };
        results.insert(user_id.clone(), entry);
    }

    Ok(results)
}

fn warn_snapshot_error(context: &str, error: &RedisError) {
    tracing::warn!(
        context,
        error = %error,
        "redis snapshot read failed; preserving last sent values"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_close_frame_is_1001_going_away() {
        match shutdown_close_frame() {
            Message::Close(Some(cf)) => {
                assert_eq!(cf.code, 1001);
                assert_eq!(&*cf.reason, "server shutting down");
            }
            other => panic!("expected Close(Some(_)), got {other:?}"),
        }
    }
}
