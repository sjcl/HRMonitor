use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::worker::run_worker;

type ReconcileSnapshot = (HashMap<String, i32>, Vec<(String, JoinHandle<()>)>);

/// Connections eligible for worker spawning.
/// Excludes refresh_blocked error connections (terminal OAuth failure — user must re-authorize).
const SPAWNABLE_CONNECTIONS_SQL: &str = "SELECT user_id, config_version FROM pulsoid_connections \
     WHERE connection_state IN ('pending', 'connected') \
       OR (connection_state = 'error' AND NOT refresh_blocked \
           AND state_updated_at < now() - interval '5 minutes')";

struct WorkerState {
    handle: JoinHandle<()>,
    config_version: i32,
}

pub struct NotifyOutcome {
    pub stale: bool,
    pub actual_config_version: Option<i32>,
}

enum NotifyAction {
    /// Command is stale — a newer worker is already running or DB moved past expected.
    Stale,
    /// DB state matches the running worker — nothing to do.
    NoChange,
    /// Replace the current worker (stop old, start new if DB has a connection).
    Replace,
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

        for (user_id, config_version) in rows {
            self.replace_worker(&user_id, config_version, None).await;
        }
    }

    /// Notify that a user's pulsoid connection changed (created, updated, or deleted).
    /// All mutations are version-guarded to prevent stale commands from rolling back
    /// a newer worker. Queries the DB first so a transient DB failure leaves the
    /// existing worker running.
    pub async fn notify_connection_changed(
        &self,
        user_id: &str,
        expected_config_version: Option<i32>,
    ) -> Result<NotifyOutcome, String> {
        let Some(expected) = expected_config_version else {
            // All NATS commands should include config_version after the DELETE
            // handler fix. If we receive None, treat as error so the ack maps
            // to applied: false.
            tracing::warn!(
                user_id,
                "Received connection change without config_version, rejecting"
            );
            return Err("missing config_version in connection change command".to_string());
        };

        // Step 1: Query DB FIRST — if this fails, old worker keeps running
        let conn: Option<(i32,)> =
            sqlx::query_as("SELECT config_version FROM pulsoid_connections WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| format!("DB error: {e}"))?;

        let actual_config_version = conn.map(|(cv,)| cv);

        // Step 2: Decide action under lock (read-only — no mutations)
        let action = {
            let state = self.state.lock().await;
            let current_version = state.get(user_id).map(|c| c.config_version);

            match current_version {
                Some(cv) if cv != expected => NotifyAction::Stale,
                Some(cv) if actual_config_version == Some(cv) => NotifyAction::NoChange,
                Some(_) => NotifyAction::Replace,
                None => {
                    // No worker running. If DB moved past our version, stale.
                    if actual_config_version.is_some()
                        && actual_config_version != Some(expected)
                    {
                        NotifyAction::Stale
                    } else {
                        NotifyAction::Replace
                    }
                }
            }
        };

        // Step 3: Execute
        match action {
            NotifyAction::Stale => {
                tracing::info!(
                    user_id,
                    expected,
                    actual = ?actual_config_version,
                    "Stale connection change command"
                );
                Ok(NotifyOutcome {
                    stale: true,
                    actual_config_version,
                })
            }
            NotifyAction::NoChange => {
                tracing::debug!(
                    user_id,
                    version = ?actual_config_version,
                    "Connection change is a no-op, worker already at correct version"
                );
                Ok(NotifyOutcome {
                    stale: false,
                    actual_config_version,
                })
            }
            NotifyAction::Replace => {
                let applied = if let Some(config_version) = actual_config_version {
                    let ok = self
                        .replace_worker(user_id, config_version, Some(expected))
                        .await;
                    if !ok {
                        tracing::info!(
                            user_id,
                            config_version,
                            "Replace skipped: worker already updated by another call"
                        );
                    }
                    ok
                } else {
                    // Connection deleted — stop with version guard
                    let ok = self.guarded_stop(user_id, expected).await;
                    if !ok {
                        tracing::info!(
                            user_id,
                            expected,
                            "Stop skipped: worker already updated by another call"
                        );
                    }
                    ok
                };

                // If the guarded operation did not apply, another call raced
                // ahead of us — treat as stale.
                Ok(NotifyOutcome {
                    stale: !applied,
                    actual_config_version,
                })
            }
        }
    }

    /// Reconcile active workers with DB state.
    /// Detects new connections, removed connections, and config_version changes.
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
                .map(|(k, v)| (k.clone(), v.config_version))
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

        let db_user_ids: HashSet<String> = db_connections.keys().cloned().collect();
        let active_user_ids: HashSet<String> = snapshot.keys().cloned().collect();

        // DB にあって active にない → spawn
        for user_id in db_user_ids.difference(&active_user_ids) {
            tracing::info!(user_id = %user_id, "Reconcile: spawning missing worker");
            if !self
                .replace_worker(user_id, *db_connections.get(user_id).unwrap(), None)
                .await
            {
                tracing::debug!(
                    user_id = %user_id,
                    "Reconcile: spawn skipped, slot no longer vacant"
                );
            }
        }

        // active にあって DB にない → stop
        for user_id in active_user_ids.difference(&db_user_ids) {
            tracing::info!(user_id = %user_id, "Reconcile: stopping orphaned worker");
            if let Some(&active_ver) = snapshot.get(user_id)
                && !self.guarded_stop(user_id, active_ver).await
            {
                tracing::debug!(
                    user_id = %user_id,
                    "Reconcile: orphan stop skipped, worker already updated"
                );
            }
        }

        // 両方にあるが config_version が変わった → 再起動
        for user_id in db_user_ids.intersection(&active_user_ids) {
            if let (Some(&active_ver), Some(&db_ver)) =
                (snapshot.get(user_id), db_connections.get(user_id))
                && db_ver != active_ver
            {
                tracing::info!(
                    user_id = %user_id,
                    old_version = active_ver,
                    new_version = db_ver,
                    "Reconcile: config_version changed, restarting worker"
                );
                if !self.replace_worker(user_id, db_ver, Some(active_ver)).await {
                    tracing::debug!(
                        user_id = %user_id,
                        "Reconcile: version-change restart skipped, worker already updated"
                    );
                }
            }
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
        new_config_version: i32,
        expected_current_version: Option<i32>,
    ) -> bool {
        // Phase 1: lock, check, remove handle if allowed
        let old_handle = {
            let mut state = self.state.lock().await;
            match state.get(user_id) {
                Some(current) => match expected_current_version {
                    Some(expected) if current.config_version == expected => {
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
        let uid = user_id.to_string();
        let handle = tokio::spawn(run_worker(db, nats, uid, new_config_version));
        state.insert(
            user_id.to_string(),
            WorkerState {
                handle,
                config_version: new_config_version,
            },
        );
        true
    }

    /// Stop a worker only if it holds exactly `expected_version`.
    /// Two-phase to avoid awaiting under the lock.
    /// Returns true if the worker was stopped.
    async fn guarded_stop(&self, user_id: &str, expected_version: i32) -> bool {
        let old_handle = {
            let mut state = self.state.lock().await;
            match state.get(user_id) {
                Some(current) if current.config_version == expected_version => {
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
