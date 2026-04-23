use axum::Extension;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, close_code};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::time::{Duration, interval};

use common::access::ViewableUserId;
use crate::WsState;

use common::access::{ensure_active_member, ensure_can_view_user};
use common::auth::AuthenticatedUser;
use common::error::AppError;
use common::messages::HeartRateReceived;
use common::visibility::values::PRIVATE;
use common::redis_keys::latest_bpm_key;

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

    // Send initial snapshot
    let snap = read_snapshot(&state, std::slice::from_ref(&target_user_id)).await;
    log_snapshot_errors("single_user initial", &snap);
    let mut last_sent: Option<HeartRateReceived> = match snap.get(&target_user_id) {
        Some(SnapshotEntry::Hit(hr)) => Some(hr.clone()),
        _ => None,
    };
    let msg = WsServerMessage::Snapshot {
        data: to_ws_snapshot(snap),
    };
    if let Ok(json) = serde_json::to_string(&msg)
        && sender.send(Message::Text(json.into())).await.is_err()
    {
        return;
    }

    let mut reauth_interval = interval(Duration::from_secs(30));
    reauth_interval.tick().await;

    let mut self_heal_interval = interval(Duration::from_secs(10));
    self_heal_interval.tick().await;

    let mut ping_interval = interval(Duration::from_secs(30));
    ping_interval.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                let _ = sender.send(shutdown_close_frame()).await;
                break;
            }
            should_disconnect = async {
                tokio::select! {
                    msg = receiver.next() => {
                        matches!(msg, Some(Ok(Message::Close(_))) | None)
                    }
                    _ = ping_interval.tick() => {
                        sender.send(Message::Ping(Default::default())).await.is_err()
                    }
                }
            } => {
                if should_disconnect {
                    break;
                }
            }
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if update.user_id == target_user_id {
                            last_sent = Some(update.clone());
                            let msg = WsServerMessage::Update { data: update };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && sender.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket broadcast lagged by {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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
                let snap = read_snapshot(&state, std::slice::from_ref(&target_user_id)).await;
                log_snapshot_errors("single_user self_heal", &snap);
                match snap.get(&target_user_id) {
                    Some(SnapshotEntry::Hit(hr)) => {
                        if last_sent.as_ref() != Some(hr) {
                            last_sent = Some(hr.clone());
                            let msg = WsServerMessage::Update { data: hr.clone() };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && sender.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                    }
                    Some(SnapshotEntry::Miss) | None => {
                        if last_sent.is_some() {
                            last_sent = None;
                            let mut data: HashMap<String, Option<HeartRateReceived>> =
                                HashMap::with_capacity(1);
                            data.insert(target_user_id.clone(), None);
                            let msg = WsServerMessage::Snapshot { data };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && sender.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                    }
                    Some(SnapshotEntry::Error) => {}
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
    let snap = read_snapshot(&state, &user_ids).await;
    log_snapshot_errors("group initial", &snap);
    for (uid, entry) in &snap {
        if let SnapshotEntry::Hit(hr) = entry {
            last_sent.insert(uid.clone(), hr.clone());
        }
    }
    let msg = WsServerMessage::Snapshot {
        data: to_ws_snapshot(snap),
    };
    if let Ok(json) = serde_json::to_string(&msg)
        && sender.send(Message::Text(json.into())).await.is_err()
    {
        return;
    }

    let mut reauth_interval = interval(Duration::from_secs(30));
    reauth_interval.tick().await;

    let mut self_heal_interval = interval(Duration::from_secs(10));
    self_heal_interval.tick().await;

    let mut ping_interval = interval(Duration::from_secs(30));
    ping_interval.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                let _ = sender.send(shutdown_close_frame()).await;
                break;
            }
            should_disconnect = async {
                tokio::select! {
                    msg = receiver.next() => {
                        matches!(msg, Some(Ok(Message::Close(_))) | None)
                    }
                    _ = ping_interval.tick() => {
                        sender.send(Message::Ping(Default::default())).await.is_err()
                    }
                }
            } => {
                if should_disconnect {
                    break;
                }
            }
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if members.contains(&update.user_id) {
                            last_sent.insert(update.user_id.clone(), update.clone());
                            let msg = WsServerMessage::Update { data: update };
                            if let Ok(json) = serde_json::to_string(&msg)
                                && sender.send(Message::Text(json.into())).await.is_err()
                            {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket broadcast lagged by {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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
                    if let Ok(json) = serde_json::to_string(&msg)
                        && sender.send(Message::Text(json.into())).await.is_err()
                    {
                        break;
                    }
                }

                let added: Vec<String> = new_members.difference(&members).cloned().collect();
                if !added.is_empty() {
                    let snap = read_snapshot(&state, &added).await;
                    log_snapshot_errors("group added_members", &snap);
                    for (uid, entry) in &snap {
                        if let SnapshotEntry::Hit(hr) = entry {
                            last_sent.insert(uid.clone(), hr.clone());
                        }
                    }
                    let msg = WsServerMessage::Snapshot {
                        data: to_ws_snapshot(snap),
                    };
                    if let Ok(json) = serde_json::to_string(&msg)
                        && sender.send(Message::Text(json.into())).await.is_err()
                    {
                        break;
                    }
                }

                members = new_members;
            }
            _ = self_heal_interval.tick() => {
                let user_ids: Vec<String> = members.iter().cloned().collect();
                let snap = read_snapshot(&state, &user_ids).await;
                log_snapshot_errors("group self_heal", &snap);
                let mut diffs: HashMap<String, Option<HeartRateReceived>> = HashMap::new();
                for (uid, entry) in snap {
                    match entry {
                        SnapshotEntry::Hit(hr) => {
                            if last_sent.get(&uid) != Some(&hr) {
                                last_sent.insert(uid.clone(), hr.clone());
                                diffs.insert(uid, Some(hr));
                            }
                        }
                        SnapshotEntry::Miss => {
                            if last_sent.remove(&uid).is_some() {
                                diffs.insert(uid, None);
                            }
                        }
                        SnapshotEntry::Error => {}
                    }
                }
                if !diffs.is_empty() {
                    let msg = WsServerMessage::Snapshot { data: diffs };
                    if let Ok(json) = serde_json::to_string(&msg)
                        && sender.send(Message::Text(json.into())).await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn fetch_sharing_members(
    db: &sqlx::PgPool,
    group_id: &str,
    auth_user_id: &str,
) -> Result<HashSet<String>, AppError> {
    let query = format!(
        "SELECT gm.user_id FROM group_members gm \
         JOIN users u ON u.id = gm.user_id \
         WHERE gm.group_id = $1 AND gm.status = 'active' \
           AND (gm.sharing = true OR gm.user_id = $2) \
           AND (u.heart_rate_visibility != '{}' OR gm.user_id = $2)",
        PRIVATE
    );
    let rows: Vec<(String,)> = sqlx::query_as(&query)
    .bind(group_id)
    .bind(auth_user_id)
    .fetch_all(db)
    .await?;

    let set: HashSet<String> = rows.into_iter().map(|(uid,)| uid).collect();

    if !set.contains(auth_user_id) {
        return Err(AppError::NotFound("Group not found".into()));
    }

    Ok(set)
}

enum SnapshotEntry {
    Hit(HeartRateReceived),
    Miss,
    Error,
}

async fn read_snapshot(
    state: &WsState,
    user_ids: &[String],
) -> HashMap<String, SnapshotEntry> {
    let mut results: HashMap<String, SnapshotEntry> = HashMap::with_capacity(user_ids.len());

    if user_ids.is_empty() {
        return results;
    }

    let keys: Vec<String> = user_ids.iter().map(|id| latest_bpm_key(id)).collect();
    let mut redis = state.redis.clone();

    match redis.mget::<_, Vec<Option<String>>>(&keys).await {
        Ok(values) => {
            for (user_id, value) in user_ids.iter().zip(values) {
                let entry = match value {
                    Some(s) => match serde_json::from_str::<HeartRateReceived>(&s) {
                        Ok(hr) => SnapshotEntry::Hit(hr),
                        Err(e) => {
                            tracing::warn!(
                                user_id = %user_id,
                                error = %e,
                                "failed to parse latest_bpm payload; treating as miss"
                            );
                            SnapshotEntry::Miss
                        }
                    },
                    None => SnapshotEntry::Miss,
                };
                results.insert(user_id.clone(), entry);
            }
        }
        Err(_) => {
            for user_id in user_ids {
                results.insert(user_id.clone(), SnapshotEntry::Error);
            }
        }
    }

    results
}

fn log_snapshot_errors(context: &str, entries: &HashMap<String, SnapshotEntry>) {
    let error_count = entries
        .values()
        .filter(|e| matches!(e, SnapshotEntry::Error))
        .count();
    if error_count > 0 {
        tracing::warn!(
            context,
            error_count,
            total = entries.len(),
            "redis snapshot read had errors; preserving last sent values"
        );
    }
}

fn to_ws_snapshot(
    entries: HashMap<String, SnapshotEntry>,
) -> HashMap<String, Option<HeartRateReceived>> {
    entries
        .into_iter()
        .map(|(k, v)| {
            let slot = match v {
                SnapshotEntry::Hit(hr) => Some(hr),
                SnapshotEntry::Miss | SnapshotEntry::Error => None,
            };
            (k, slot)
        })
        .collect()
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
