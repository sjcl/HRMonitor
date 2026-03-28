use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::models::PulsoidToken;
use crate::worker::run_worker;

pub struct WorkerManager {
    db: SqlitePool,
    workers: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl WorkerManager {
    pub fn new(db: SqlitePool) -> Arc<Self> {
        Arc::new(Self {
            db,
            workers: Mutex::new(HashMap::new()),
        })
    }

    pub async fn start_all_active(&self) {
        let tokens: Vec<PulsoidToken> = sqlx::query_as(
            "SELECT * FROM pulsoid_tokens WHERE is_active = 1",
        )
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();

        tracing::info!("Starting {} active workers", tokens.len());

        for token in tokens {
            self.start(token).await;
        }
    }

    pub async fn start(&self, token: PulsoidToken) {
        let mut workers = self.workers.lock().await;

        // Stop existing worker if any
        if let Some(handle) = workers.remove(&token.id) {
            handle.abort();
        }

        let db = self.db.clone();
        let token_id = token.id.clone();
        let handle = tokio::spawn(run_worker(db, token));
        workers.insert(token_id, handle);
    }

    pub async fn stop(&self, token_id: &str) {
        let mut workers = self.workers.lock().await;
        if let Some(handle) = workers.remove(token_id) {
            handle.abort();
            tracing::info!(token_id, "Worker stopped");
        }
    }
}
