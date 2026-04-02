use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::broadcast::Sender;
use tokio::task::JoinHandle;

use crate::broadcast::LatestHeartRateUpdate;
use crate::worker::run_worker;

pub struct WorkerManager {
    db: PgPool,
    redis: redis::aio::MultiplexedConnection,
    hr_tx: Sender<LatestHeartRateUpdate>,
    workers: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl WorkerManager {
    pub fn new(
        db: PgPool,
        redis: redis::aio::MultiplexedConnection,
        hr_tx: Sender<LatestHeartRateUpdate>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            redis,
            hr_tx,
            workers: Mutex::new(HashMap::new()),
        })
    }

    pub async fn start_all_active(&self) {
        let user_ids: Vec<(String,)> = match sqlx::query_as(
            "SELECT user_id FROM pulsoid_connections",
        )
        .fetch_all(&self.db)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("Failed to fetch active connections: {e}");
                return;
            }
        };

        tracing::info!("Starting {} active workers", user_ids.len());

        for (user_id,) in user_ids {
            self.spawn_worker(&user_id).await;
        }
    }

    /// Notify that a user's pulsoid connection changed (created, updated, or deleted).
    /// Stops the old worker and starts a new one if a connection still exists.
    pub async fn notify_connection_changed(&self, user_id: &str) {
        // Step 1: Remove old handle under lock
        let old_handle = {
            let mut workers = self.workers.lock().await;
            workers.remove(user_id)
        };

        // Step 2: Abort + await outside lock
        if let Some(handle) = old_handle {
            handle.abort();
            let _ = handle.await;
        }

        // Step 3: Check if connection still exists (outside lock)
        let has_connection: bool = match sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pulsoid_connections WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(user_id, "Failed to check pulsoid connection, assuming exists: {e}");
                true
            }
        };

        // Step 4: Spawn new worker if needed (under lock)
        if has_connection {
            self.spawn_worker(user_id).await;
        } else {
            tracing::info!(user_id, "No pulsoid connection, worker not started");
        }
    }

    async fn spawn_worker(&self, user_id: &str) {
        let mut workers = self.workers.lock().await;

        // Stop existing worker if any
        if let Some(handle) = workers.remove(user_id) {
            handle.abort();
            let _ = handle.await;
        }

        let db = self.db.clone();
        let redis = self.redis.clone();
        let hr_tx = self.hr_tx.clone();
        let uid = user_id.to_string();
        let handle = tokio::spawn(run_worker(db, redis, hr_tx, uid));
        workers.insert(user_id.to_string(), handle);
    }

    pub async fn stop(&self, user_id: &str) {
        let old_handle = {
            let mut workers = self.workers.lock().await;
            workers.remove(user_id)
        };
        if let Some(handle) = old_handle {
            handle.abort();
            let _ = handle.await;
            tracing::info!(user_id, "Worker stopped");
        }
    }
}
