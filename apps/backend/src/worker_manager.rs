use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::broadcast::Sender;
use tokio::task::JoinHandle;

use crate::broadcast::LatestHeartRateUpdate;
use crate::models::UserRow;
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
        let users: Vec<UserRow> = match sqlx::query_as(
            "SELECT id, name, timezone, pulsoid_access_token,
                    EXTRACT(EPOCH FROM pulsoid_last_connected_at)::BIGINT as pulsoid_last_connected_at,
                    pulsoid_last_error,
                    EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
                    EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
             FROM users WHERE pulsoid_access_token IS NOT NULL",
        )
        .fetch_all(&self.db)
        .await
        {
            Ok(users) => users,
            Err(e) => {
                tracing::error!("Failed to fetch active users: {e}");
                return;
            }
        };

        tracing::info!("Starting {} active workers", users.len());

        for user in users {
            self.start(user).await;
        }
    }

    pub async fn start(&self, user: UserRow) {
        let mut workers = self.workers.lock().await;

        // Stop existing worker if any
        if let Some(handle) = workers.remove(&user.id) {
            handle.abort();
        }

        let db = self.db.clone();
        let redis = self.redis.clone();
        let hr_tx = self.hr_tx.clone();
        let user_id = user.id.clone();
        let handle = tokio::spawn(run_worker(db, redis, hr_tx, user));
        workers.insert(user_id, handle);
    }

    pub async fn stop(&self, user_id: &str) {
        let mut workers = self.workers.lock().await;
        if let Some(handle) = workers.remove(user_id) {
            handle.abort();
            tracing::info!(user_id, "Worker stopped");
        }
    }
}
