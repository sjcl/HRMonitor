use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

use crate::broadcast::LatestHeartRateUpdate;
use crate::models::WsClientMessage;
use crate::AppState;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsServerMessage {
    Snapshot { data: Vec<Option<LatestHeartRateUpdate>> },
    Update { data: LatestHeartRateUpdate },
}

pub async fn heart_rate_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut subscribed: HashSet<String> = HashSet::new();
    let mut broadcast_rx = state.hr_broadcast.subscribe();

    loop {
        tokio::select! {
            // Handle messages from client
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsClientMessage>(&text) {
                            Ok(WsClientMessage::Subscribe { user_ids }) => {
                                subscribed.extend(user_ids.iter().cloned());
                                // Send snapshot from Redis
                                let snapshot = read_snapshot(&state, &user_ids).await;
                                let msg = WsServerMessage::Snapshot { data: snapshot };
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    if sender.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Ok(WsClientMessage::Unsubscribe { user_ids }) => {
                                for id in &user_ids {
                                    subscribed.remove(id);
                                }
                            }
                            Err(_) => {
                                // Ignore malformed messages
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            // Handle broadcast updates
            result = broadcast_rx.recv() => {
                match result {
                    Ok(update) => {
                        if subscribed.contains(&update.user_id) {
                            let msg = WsServerMessage::Update { data: update };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                if sender.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket broadcast lagged by {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn read_snapshot(
    state: &AppState,
    user_ids: &[String],
) -> Vec<Option<LatestHeartRateUpdate>> {
    let mut redis = state.redis.lock().await;
    let mut results = Vec::with_capacity(user_ids.len());

    for user_id in user_ids {
        let key = format!("latest_bpm:{user_id}");
        let value: Option<String> = redis.get(&key).await.unwrap_or(None);
        let parsed = value.and_then(|v| serde_json::from_str::<LatestHeartRateUpdate>(&v).ok());
        results.push(parsed);
    }

    results
}
