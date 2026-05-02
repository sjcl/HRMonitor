use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use common::token_encryption::TokenEncryption;

use crate::worker::run_worker;

type ReconcileSnapshot = (HashMap<String, i32>, Vec<(String, JoinHandle<()>)>);

/// Connections eligible for worker spawning.
/// Error states are terminal — recovery requires a revision bump
/// (re-auth, new manual token, successful token refresh, etc.), which
/// re-admits the row via the 'pending' path.
const SPAWNABLE_CONNECTIONS_SQL: &str = "SELECT user_id, revision FROM pulsoid_connections \
     WHERE connection_state IN ('pending', 'connected')";

/// Single-user variant of SPAWNABLE_CONNECTIONS_SQL used by reconcile_user.
const SPAWNABLE_USER_SQL: &str = "SELECT revision FROM pulsoid_connections \
     WHERE user_id = $1 \
       AND connection_state IN ('pending', 'connected')";

struct WorkerState {
    handle: JoinHandle<()>,
    revision: i32,
}

pub struct WorkerManager {
    db: PgPool,
    nats: async_nats::Client,
    redis: redis::aio::MultiplexedConnection,
    encryption: Arc<TokenEncryption>,
    state: Mutex<HashMap<String, WorkerState>>,
}

impl WorkerManager {
    pub fn new(
        db: PgPool,
        nats: async_nats::Client,
        redis: redis::aio::MultiplexedConnection,
        encryption: Arc<TokenEncryption>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            nats,
            redis,
            encryption,
            state: Mutex::new(HashMap::new()),
        })
    }

    pub async fn start_all_active(&self) {
        let rows: Vec<(String, i32)> = match sqlx::query_as(SPAWNABLE_CONNECTIONS_SQL)
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

        for (user_id, revision) in rows {
            self.apply_db_state_for_user(&user_id, Some(revision)).await;
        }
    }

    /// Reconcile the in-memory worker slot for a single user with DB state.
    /// Called by the NATS connection-change subscriber as a fire-and-forget
    /// hint. A DB error leaves the existing worker running and logs a warning.
    pub async fn reconcile_user(&self, user_id: &str) {
        // 1. Query DB (SPAWNABLE filter — same as periodic reconcile)
        let db_version: Option<i32> = match sqlx::query_as::<_, (i32,)>(SPAWNABLE_USER_SQL)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await
        {
            Ok(row) => row.map(|(rev,)| rev),
            Err(e) => {
                tracing::warn!(user_id, "reconcile_user DB error: {e}");
                return;
            }
        };

        // 2. Delegate to the shared decision function
        self.apply_db_state_for_user(user_id, db_version).await;
    }

    /// Apply DB state to the in-memory worker slot for one user.
    /// Logs the branch taken at debug level only — info-level "state actually
    /// changed" logs live in this function so that skipped and no-op
    /// invocations do not get double-logged.
    async fn apply_db_state_for_user(&self, user_id: &str, db_version: Option<i32>) {
        let mut state = self.state.lock().await;
        let active_version = state.get(user_id).map(|ws| ws.revision);

        match (db_version, active_version) {
            // DB has row, no worker → spawn
            (Some(db_ver), None) => {
                tracing::debug!(user_id, db_ver, "apply: spawn branch");
                let worker = self.spawn_worker(user_id, db_ver);
                state.insert(user_id.to_string(), worker);
                tracing::info!(user_id, new_revision = db_ver, "Worker spawned");
            }
            // DB has row, worker at same version → no-op
            (Some(db_ver), Some(active_ver)) if db_ver == active_ver => {
                tracing::debug!(user_id, db_ver, "apply: in-sync, no-op");
            }
            // DB has row, worker at different version → replace
            (Some(db_ver), Some(active_ver)) => {
                tracing::debug!(
                    user_id,
                    old_version = active_ver,
                    new_version = db_ver,
                    "apply: replace branch"
                );
                if let Some(old_worker) = state.remove(user_id) {
                    old_worker.handle.abort();
                    let _ = old_worker.handle.await;
                }

                let worker = self.spawn_worker(user_id, db_ver);
                state.insert(user_id.to_string(), worker);
                tracing::info!(
                    user_id,
                    old_revision = active_ver,
                    new_revision = db_ver,
                    "Worker replaced"
                );
            }
            // DB has no spawnable row, worker exists → stop
            (None, Some(active_ver)) => {
                tracing::debug!(user_id, active_ver, "apply: stop branch");
                if let Some(old_worker) = state.remove(user_id) {
                    old_worker.handle.abort();
                    let _ = old_worker.handle.await;
                    tracing::info!(user_id, active_ver, "Worker stopped");
                }
            }
            // Neither → no-op
            (None, None) => {}
        }
    }

    /// Reconcile active workers with DB state.
    /// Detects new connections, removed connections, and revision changes.
    pub async fn reconcile(&self) {
        let db_rows: Vec<(String, i32)> = match sqlx::query_as(SPAWNABLE_CONNECTIONS_SQL)
            .fetch_all(&self.db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("Reconciliation failed to query DB: {e}");
                return;
            }
        };

        let db_connections: HashMap<String, i32> = db_rows.into_iter().collect();

        let (snapshot, finished_workers): ReconcileSnapshot = {
            let mut state = self.state.lock().await;
            let finished_user_ids: Vec<String> = state
                .iter()
                .filter_map(|(user_id, worker)| {
                    worker.handle.is_finished().then_some(user_id.clone())
                })
                .collect();

            let finished_workers = finished_user_ids
                .into_iter()
                .filter_map(|user_id| {
                    state
                        .remove(&user_id)
                        .map(|worker| (user_id, worker.handle))
                })
                .collect();

            let snapshot = state.iter().map(|(k, v)| (k.clone(), v.revision)).collect();

            (snapshot, finished_workers)
        };

        for (user_id, handle) in finished_workers {
            match handle.await {
                Ok(()) => {
                    tracing::info!(user_id = %user_id, "Reconcile: removed finished worker");
                }
                Err(e) if e.is_cancelled() => {
                    tracing::info!(user_id = %user_id, "Reconcile: removed cancelled worker");
                }
                Err(e) => {
                    tracing::warn!(user_id = %user_id, "Reconcile: removed failed worker: {e}");
                }
            }
        }

        // Union of DB user_ids and active worker user_ids.
        // Runs once per 60s — cloned() to avoid borrow-lifetime gymnastics.
        let all_ids: HashSet<String> = db_connections
            .keys()
            .chain(snapshot.keys())
            .cloned()
            .collect();

        for user_id in &all_ids {
            let db_ver = db_connections.get(user_id).copied();
            self.apply_db_state_for_user(user_id, db_ver).await;
        }
    }

    fn spawn_worker(&self, user_id: &str, revision: i32) -> WorkerState {
        let db = self.db.clone();
        let nats = self.nats.clone();
        let redis = self.redis.clone();
        let encryption = self.encryption.clone();
        let uid = user_id.to_string();
        let handle = tokio::spawn(run_worker(db, nats, redis, encryption, uid, revision));
        WorkerState { handle, revision }
    }

    /// Abort and await all active workers. Called once during graceful shutdown.
    pub async fn shutdown_all(&self) {
        let workers: Vec<(String, JoinHandle<()>)> = {
            self.state
                .lock()
                .await
                .drain()
                .map(|(uid, ws)| (uid, ws.handle))
                .collect()
        };

        tracing::info!("Stopping {} worker(s)", workers.len());
        for (user_id, handle) in workers {
            handle.abort();
            match handle.await {
                Ok(()) => tracing::info!(user_id, "Worker exited"),
                Err(e) if e.is_cancelled() => tracing::debug!(user_id, "Worker cancelled"),
                Err(e) => tracing::warn!(user_id, "Worker join error: {e}"),
            }
        }
    }
}
