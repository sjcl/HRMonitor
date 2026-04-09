use axum::Extension;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::time::{Duration, interval};

use crate::AppState;
use crate::auth::{AuthenticatedUser, ViewableUserId, ensure_can_view_user};
use crate::error::AppError;
use crate::handlers::groups::ensure_active_member;
use common::messages::HeartRateReceived;

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
    State(state): State<Arc<AppState>>,
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
    State(state): State<Arc<AppState>>,
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
    State(state): State<Arc<AppState>>,
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
    state: Arc<AppState>,
    target_user_id: String,
    reauth: Option<AuthenticatedUser>, // None = self, skip reauth
) {
    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = state.hr_broadcast.subscribe();

    // Send initial snapshot
    let snapshot = read_snapshot(&state, std::slice::from_ref(&target_user_id)).await;
    let msg = WsServerMessage::Snapshot { data: snapshot };
    if let Ok(json) = serde_json::to_string(&msg)
        && sender.send(Message::Text(json.into())).await.is_err()
    {
        return;
    }

    let mut reauth_interval = interval(Duration::from_secs(30));
    reauth_interval.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // ignore all client messages
                }
            }
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if update.user_id == target_user_id {
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
                    break; // permission revoked
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
    state: Arc<AppState>,
    auth_user: AuthenticatedUser,
    group_id: String,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = state.hr_broadcast.subscribe();

    // Fetch initial member list (sharing=true OR self, status=active)
    let mut members: HashSet<String> =
        match fetch_sharing_members(&state.db, &group_id, &auth_user.id).await {
            Ok(m) => m,
            Err(_) => return,
        };

    // Send initial snapshot
    let user_ids: Vec<String> = members.iter().cloned().collect();
    let snapshot = read_snapshot(&state, &user_ids).await;
    let msg = WsServerMessage::Snapshot { data: snapshot };
    if let Ok(json) = serde_json::to_string(&msg)
        && sender.send(Message::Text(json.into())).await.is_err()
    {
        return;
    }

    let mut reauth_interval = interval(Duration::from_secs(30));
    reauth_interval.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // ignore all client messages
                }
            }
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if members.contains(&update.user_id) {
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
                // Re-check membership and sharing
                let new_members: HashSet<String> = match fetch_sharing_members(&state.db, &group_id, &auth_user.id).await {
                    Ok(m) => m,
                    Err(_) => break, // group deleted or self removed
                };

                // Check self is still a member
                if !new_members.contains(&auth_user.id) {
                    // Self was removed or left — but fetch_sharing_members always
                    // includes self if active, so absence means we left/group deleted.
                    break;
                }

                // Detect removed members → send snapshot with null
                let removed: Vec<String> = members.difference(&new_members).cloned().collect();
                if !removed.is_empty() {
                    let mut removal_data: HashMap<String, Option<HeartRateReceived>> =
                        HashMap::with_capacity(removed.len());
                    for uid in &removed {
                        removal_data.insert(uid.clone(), None::<HeartRateReceived>);
                    }
                    let msg = WsServerMessage::Snapshot { data: removal_data };
                    if let Ok(json) = serde_json::to_string(&msg)
                        && sender.send(Message::Text(json.into())).await.is_err()
                    {
                        break;
                    }
                }

                // Detect added members → send snapshot with their data
                let added: Vec<String> = new_members.difference(&members).cloned().collect();
                if !added.is_empty() {
                    let snapshot = read_snapshot(&state, &added).await;
                    let msg = WsServerMessage::Snapshot { data: snapshot };
                    if let Ok(json) = serde_json::to_string(&msg)
                        && sender.send(Message::Text(json.into())).await.is_err()
                    {
                        break;
                    }
                }

                members = new_members;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fetch active group members who have sharing enabled, plus the auth user
/// regardless of their sharing flag. Returns an error if the auth user is not
/// an active member (i.e. left or group deleted).
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
           AND (u.heart_rate_visibility != 'private' OR gm.user_id = $2)",
    )
    .bind(group_id)
    .bind(auth_user_id)
    .fetch_all(db)
    .await?;

    let set: HashSet<String> = rows.into_iter().map(|(uid,)| uid).collect();

    // If auth user is not in the result, they are no longer an active member
    if !set.contains(auth_user_id) {
        return Err(AppError::NotFound("Group not found".into()));
    }

    Ok(set)
}

async fn read_snapshot(
    state: &AppState,
    user_ids: &[String],
) -> HashMap<String, Option<HeartRateReceived>> {
    let mut results: HashMap<String, Option<HeartRateReceived>> =
        HashMap::with_capacity(user_ids.len());
    let mut missing_keys = HashMap::new();

    {
        let mut redis = state.redis.lock().await;
        for user_id in user_ids {
            let key = format!("latest_bpm:{user_id}");
            let value: Option<String> = redis.get(&key).await.unwrap_or(None);
            let parsed = value.and_then(|v| serde_json::from_str::<HeartRateReceived>(&v).ok());

            match parsed {
                Some(value) => {
                    results.insert(user_id.clone(), Some(value));
                }
                None => {
                    missing_keys.insert(user_id.clone(), key);
                    results.insert(user_id.clone(), None);
                }
            }
        }
    }

    let mut cache_refills = Vec::new();
    if !missing_keys.is_empty() {
        let rows = sqlx::query_as::<_, (String, i32, i64)>(
            "SELECT DISTINCT ON (user_id) user_id, bpm, \
             EXTRACT(EPOCH FROM recorded_at)::BIGINT AS recorded_at \
             FROM heart_rate_records \
             WHERE user_id = ANY($1) \
             ORDER BY user_id, recorded_at DESC",
        )
        .bind(&missing_keys.keys().cloned().collect::<Vec<_>>())
        .fetch_all(&state.db)
        .await
        .inspect_err(|e| tracing::warn!("batch latest HR query failed: {e}"))
        .unwrap_or_default();

        for (user_id, bpm, recorded_at) in rows {
            if let Some(key) = missing_keys.remove(&user_id) {
                let update = HeartRateReceived {
                    user_id: user_id.clone(),
                    bpm,
                    recorded_at,
                    received_at: recorded_at,
                };
                results.insert(user_id, Some(update.clone()));
                cache_refills.push((key, update));
            }
        }
    }

    if !cache_refills.is_empty() {
        let mut redis = state.redis.lock().await;
        for (key, update) in cache_refills {
            if let Ok(json) = serde_json::to_string(&update) {
                let _: Result<Option<String>, _> = redis::cmd("SET")
                    .arg(&key)
                    .arg(&json)
                    .query_async(&mut *redis)
                    .await;
            }
        }
    }

    results
}
