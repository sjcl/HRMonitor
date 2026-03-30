use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

use crate::AppState;
use crate::broadcast::LatestHeartRateUpdate;
use crate::models::WsClientMessage;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsServerMessage {
    Snapshot {
        data: Vec<Option<LatestHeartRateUpdate>>,
    },
    Update {
        data: LatestHeartRateUpdate,
    },
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
                                if let Ok(json) = serde_json::to_string(&msg)
                                    && sender.send(Message::Text(json.into())).await.is_err()
                                {
                                    break;
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
        }
    }
}

async fn read_snapshot(
    state: &AppState,
    user_ids: &[String],
) -> Vec<Option<LatestHeartRateUpdate>> {
    let mut results = Vec::with_capacity(user_ids.len());
    let mut missing = Vec::new();

    {
        let mut redis = state.redis.lock().await;
        for user_id in user_ids {
            let key = format!("latest_bpm:{user_id}");
            let value: Option<String> = redis.get(&key).await.unwrap_or(None);
            let parsed = value.and_then(|v| serde_json::from_str::<LatestHeartRateUpdate>(&v).ok());

            match parsed {
                Some(value) => results.push(Some(value)),
                None => {
                    missing.push((results.len(), user_id.clone(), key));
                    results.push(None);
                }
            }
        }
    }

    let mut cache_refills = Vec::new();
    for (index, user_id, key) in missing {
        let from_db = sqlx::query_as::<_, (i32, i64)>(
            "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as recorded_at \
             FROM heart_rate_records WHERE user_id = $1 \
             ORDER BY recorded_at DESC LIMIT 1",
        )
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|(bpm, recorded_at)| LatestHeartRateUpdate {
            user_id,
            bpm,
            recorded_at,
            received_at: recorded_at,
        });

        if let Some(update) = from_db {
            results[index] = Some(update.clone());
            cache_refills.push((key, update));
        }
    }

    if !cache_refills.is_empty() {
        let mut redis = state.redis.lock().await;
        for (key, update) in cache_refills {
            if let Ok(json) = serde_json::to_string(&update) {
                let _: Result<Option<String>, _> = redis::cmd("SET")
                    .arg(&key)
                    .arg(&json)
                    .arg("NX")
                    .query_async(&mut *redis)
                    .await;
            }
        }
    }

    results
}
