use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::worker::run_worker;

struct WorkerState {
    handle: JoinHandle<()>,
    config_version: i32,
}

pub struct WorkerManager {
    db: PgPool,
    nats: async_nats::Client,
    state: Mutex<HashMap<String, WorkerState>>,
}

impl WorkerManager {
    pub fn new(db: PgPool, nats: async_nats::Client) -> Arc<Self> {
        Arc::new(Self {
            db,
            nats,
            state: Mutex::new(HashMap::new()),
        })
    }

    pub async fn start_all_active(&self) {
        let rows: Vec<(String, i32)> =
            match sqlx::query_as("SELECT user_id, config_version FROM pulsoid_connections")
                .fetch_all(&self.db)
                .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::error!("Failed to fetch active connections: {e}");
                    return;
                }
            };

        tracing::info!("Starting {} active workers", rows.len());

        for (user_id, config_version) in rows {
            self.spawn_worker(&user_id, config_version).await;
        }
    }

    /// Notify that a user's pulsoid connection changed (created, updated, or deleted).
    /// Stops the old worker and starts a new one if a connection still exists.
    pub async fn notify_connection_changed(&self, user_id: &str) {
        // Step 1: Remove old handle under lock
        let old_state = {
            let mut state = self.state.lock().await;
            state.remove(user_id)
        };

        // Step 2: Abort + await outside lock
        if let Some(ws) = old_state {
            ws.handle.abort();
            let _ = ws.handle.await;
        }

        // Step 3: Check if connection still exists and get config_version
        let conn: Option<(i32,)> = match sqlx::query_as(
            "SELECT config_version FROM pulsoid_connections WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    user_id,
                    "Failed to check pulsoid connection, assuming exists: {e}"
                );
                None
            }
        };

        // Step 4: Spawn new worker if needed
        if let Some((config_version,)) = conn {
            self.spawn_worker(user_id, config_version).await;
        } else {
            tracing::info!(user_id, "No pulsoid connection, worker not started");
        }
    }

    /// Reconcile active workers with DB state.
    /// Detects new connections, removed connections, and config_version changes.
    pub async fn reconcile(&self) {
        let db_rows: Vec<(String, i32)> = match sqlx::query_as(
            "SELECT user_id, config_version FROM pulsoid_connections",
        )
        .fetch_all(&self.db)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("Reconciliation failed to query DB: {e}");
                return;
            }
        };

        let db_connections: HashMap<String, i32> =
            db_rows.into_iter().collect();

        let snapshot: HashMap<String, i32> = {
            let state = self.state.lock().await;
            state
                .iter()
                .map(|(k, v)| (k.clone(), v.config_version))
                .collect()
        };

        let db_user_ids: HashSet<String> = db_connections.keys().cloned().collect();
        let active_user_ids: HashSet<String> = snapshot.keys().cloned().collect();

        // DB にあって active にない → spawn
        for user_id in db_user_ids.difference(&active_user_ids) {
            tracing::info!(user_id = %user_id, "Reconcile: spawning missing worker");
            self.spawn_worker(user_id, *db_connections.get(user_id).unwrap())
                .await;
        }

        // active にあって DB にない → stop
        for user_id in active_user_ids.difference(&db_user_ids) {
            tracing::info!(user_id = %user_id, "Reconcile: stopping orphaned worker");
            self.stop(user_id).await;
        }

        // 両方にあるが config_version が変わった → 再起動
        for user_id in db_user_ids.intersection(&active_user_ids) {
            if let (Some(&active_ver), Some(&db_ver)) =
                (snapshot.get(user_id), db_connections.get(user_id))
            {
                if db_ver != active_ver {
                    tracing::info!(
                        user_id = %user_id,
                        old_version = active_ver,
                        new_version = db_ver,
                        "Reconcile: config_version changed, restarting worker"
                    );
                    self.spawn_worker(user_id, db_ver).await;
                }
            }
        }
    }

    async fn spawn_worker(&self, user_id: &str, config_version: i32) {
        let mut state = self.state.lock().await;

        // Stop existing worker if any
        if let Some(old) = state.remove(user_id) {
            old.handle.abort();
            let _ = old.handle.await;
        }

        let db = self.db.clone();
        let nats = self.nats.clone();
        let uid = user_id.to_string();
        let handle = tokio::spawn(run_worker(db, nats, uid));
        state.insert(
            user_id.to_string(),
            WorkerState {
                handle,
                config_version,
            },
        );
    }

    pub async fn stop(&self, user_id: &str) {
        let old_state = {
            let mut state = self.state.lock().await;
            state.remove(user_id)
        };
        if let Some(ws) = old_state {
            ws.handle.abort();
            let _ = ws.handle.await;
            tracing::info!(user_id, "Worker stopped");
        }
    }
}
