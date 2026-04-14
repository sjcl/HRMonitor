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
            self.replace_worker(&user_id, revision, None).await;
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

        // 2. Snapshot the in-memory worker version
        let active_version = {
            let state = self.state.lock().await;
            state.get(user_id).map(|ws| ws.revision)
        };

        // 3. Delegate to the shared decision function
        self.apply_db_state_for_user(user_id, db_version, active_version)
            .await;
    }

    /// Apply DB state to the in-memory worker slot for one user.
    /// Both versions are snapshotted by the caller (under lock or via atomic query).
    /// Logs the branch taken at debug level only — info-level "state actually
    /// changed" logs live in `replace_worker` / `guarded_stop` so that skipped
    /// and no-op invocations do not get double-logged.
    ///
    /// If the inner CAS loses a slot race (e.g. a stale reconcile inserted
    /// first), re-snapshots DB + in-memory state and retries once so the
    /// fresh caller can overtake the stale worker.
    async fn apply_db_state_for_user(
        &self,
        user_id: &str,
        db_version: Option<i32>,
        active_version: Option<i32>,
    ) {
        if self.try_apply(user_id, db_version, active_version).await {
            return;
        }

        // replace_worker or guarded_stop lost a CAS race.
        // Re-snapshot both sides and retry once so the fresh caller
        // can overtake a stale insert.
        tracing::debug!(user_id, ?db_version, ?active_version, "apply: CAS failed, re-snapshotting for retry");

        let db_version = match sqlx::query_as::<_, (i32,)>(SPAWNABLE_USER_SQL)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await
        {
            Ok(row) => row.map(|(rev,)| rev),
            Err(e) => {
                tracing::warn!(user_id, "apply retry: DB error: {e}");
                return;
            }
        };
        let active_version = {
            let state = self.state.lock().await;
            state.get(user_id).map(|ws| ws.revision)
        };

        self.try_apply(user_id, db_version, active_version).await;
    }

    /// Core decision logic. Returns true if the action succeeded or state is
    /// already converged; false means a CAS/slot race — caller may retry.
    async fn try_apply(
        &self,
        user_id: &str,
        db_version: Option<i32>,
        active_version: Option<i32>,
    ) -> bool {
        match (db_version, active_version) {
            // DB has row, no worker → spawn
            (Some(db_ver), None) => {
                tracing::debug!(user_id, db_ver, "apply: spawn branch");
                self.replace_worker(user_id, db_ver, None).await
            }
            // DB has row, worker at same version → no-op
            (Some(db_ver), Some(active_ver)) if db_ver == active_ver => {
                tracing::debug!(user_id, db_ver, "apply: in-sync, no-op");
                true
            }
            // DB has row, worker at different version → guarded replace
            (Some(db_ver), Some(active_ver)) => {
                tracing::debug!(
                    user_id,
                    old_version = active_ver,
                    new_version = db_ver,
                    "apply: replace branch"
                );
                self.replace_worker(user_id, db_ver, Some(active_ver)).await
            }
            // DB has no spawnable row, worker exists → guarded stop
            (None, Some(active_ver)) => {
                tracing::debug!(user_id, active_ver, "apply: stop branch");
                self.guarded_stop(user_id, active_ver).await
            }
            // Neither → no-op
            (None, None) => true,
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

            let snapshot = state
                .iter()
                .map(|(k, v)| (k.clone(), v.revision))
                .collect();

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
            let active_ver = snapshot.get(user_id).copied();
            self.apply_db_state_for_user(user_id, db_ver, active_ver)
                .await;
        }
    }

    /// Replace a worker, guarded by version.
    ///
    /// Behavior depends on current slot state and `expected_current_version`:
    /// - Slot vacant: insert (regardless of expected_current_version).
    /// - Slot occupied, `Some(v)`, current version == v: remove + abort + insert.
    /// - Slot occupied, `Some(v)`, current version != v: return false.
    /// - Slot occupied, `None`: return false (never overwrite without a version token).
    ///
    /// This is NOT "unconditional replace" — it never overwrites a worker whose
    /// version is unknown or doesn't match `expected_current_version`.
    ///
    /// Two-phase to avoid awaiting under the lock.
    /// Returns true if a new worker was spawned.
    async fn replace_worker(
        &self,
        user_id: &str,
        new_revision: i32,
        expected_current_version: Option<i32>,
    ) -> bool {
        // Phase 1: lock, check, remove handle if allowed
        let old_handle = {
            let mut state = self.state.lock().await;
            match state.get(user_id) {
                Some(current) => match expected_current_version {
                    Some(expected) if current.revision == expected => {
                        state.remove(user_id).map(|ws| ws.handle)
                    }
                    _ => return false,
                },
                None => None,
            }
        };

        // Phase 2: abort + await outside lock
        if let Some(handle) = old_handle {
            handle.abort();
            let _ = handle.await;
        }

        // Phase 3: re-lock, verify slot is still vacant, insert
        let mut state = self.state.lock().await;
        if state.contains_key(user_id) {
            // Another call inserted a worker between unlock and re-lock.
            return false;
        }

        let db = self.db.clone();
        let nats = self.nats.clone();
        let redis = self.redis.clone();
        let encryption = self.encryption.clone();
        let uid = user_id.to_string();
        let handle = tokio::spawn(run_worker(
            db,
            nats,
            redis,
            encryption,
            uid,
            new_revision,
        ));
        state.insert(
            user_id.to_string(),
            WorkerState {
                handle,
                revision: new_revision,
            },
        );
        tracing::info!(
            user_id,
            new_revision,
            ?expected_current_version,
            "Worker spawned"
        );
        true
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

    /// Stop a worker only if it holds exactly `expected_version`.
    /// Two-phase to avoid awaiting under the lock.
    /// Returns true if the worker was stopped.
    async fn guarded_stop(&self, user_id: &str, expected_version: i32) -> bool {
        let old_handle = {
            let mut state = self.state.lock().await;
            match state.get(user_id) {
                Some(current) if current.revision == expected_version => {
                    state.remove(user_id).map(|ws| ws.handle)
                }
                _ => return false,
            }
        };

        if let Some(handle) = old_handle {
            handle.abort();
            let _ = handle.await;
            tracing::info!(user_id, expected_version, "Worker stopped (guarded)");
        }
        true
    }
}
