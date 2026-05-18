use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use common::token_encryption::TokenEncryption;

use crate::worker::run_worker;

type ReconcileSnapshot = (HashMap<String, i32>, Vec<(String, JoinHandle<()>)>);

#[derive(Debug, PartialEq, Eq)]
enum WorkerAction {
    Spawn {
        revision: i32,
    },
    Replace {
        old_revision: i32,
        new_revision: i32,
    },
    Stop {
        revision: i32,
    },
    Noop,
}

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
        let db_version: Option<i32> = match self.fetch_spawnable_user_revision(user_id).await {
            Ok(revision) => revision,
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
        let active_version = {
            let state = self.state.lock().await;
            state.get(user_id).map(|ws| ws.revision)
        };

        if worker_action(db_version, active_version) == WorkerAction::Noop {
            log_noop(user_id, db_version, active_version);
            return;
        }

        // The caller's DB snapshot may be stale by the time this task wins the
        // manager lock. Re-read the latest DB state before changing the active
        // worker slot so the decision reflects current DB rather than a
        // pre-lock snapshot (the divergence can be in either direction).
        let fresh_db_version = match self.fetch_spawnable_user_revision(user_id).await {
            Ok(revision) => revision,
            Err(e) => {
                tracing::warn!(user_id, "apply DB recheck error: {e}");
                return;
            }
        };

        let mut old_handle = None;
        let (action, fresh_active_version) = {
            let mut state = self.state.lock().await;
            let active_version = state.get(user_id).map(|ws| ws.revision);
            let action = worker_action(fresh_db_version, active_version);

            match action {
                WorkerAction::Spawn { revision } => {
                    tracing::debug!(user_id, revision, "apply: spawn branch");
                    let worker = self.spawn_worker(user_id, revision);
                    state.insert(user_id.to_string(), worker);
                }
                WorkerAction::Replace {
                    old_revision: _,
                    new_revision,
                } => {
                    tracing::debug!(
                        user_id,
                        old_version = active_version,
                        new_version = new_revision,
                        "apply: replace branch"
                    );
                    old_handle = state.remove(user_id).map(|old_worker| old_worker.handle);
                    let worker = self.spawn_worker(user_id, new_revision);
                    state.insert(user_id.to_string(), worker);
                }
                WorkerAction::Stop { .. } => {
                    tracing::debug!(user_id, active_version, "apply: stop branch");
                    old_handle = state.remove(user_id).map(|old_worker| old_worker.handle);
                }
                WorkerAction::Noop => {}
            }

            (action, active_version)
        };

        if let Some(handle) = old_handle {
            handle.abort();
            let _ = handle.await;
        }

        match action {
            WorkerAction::Spawn { revision } => {
                tracing::info!(user_id, new_revision = revision, "Worker spawned");
            }
            WorkerAction::Replace {
                old_revision,
                new_revision,
            } => {
                tracing::info!(user_id, old_revision, new_revision, "Worker replaced");
            }
            WorkerAction::Stop { revision } => {
                tracing::info!(user_id, active_ver = revision, "Worker stopped");
            }
            WorkerAction::Noop => {
                log_noop(user_id, fresh_db_version, fresh_active_version);
            }
        }
    }

    async fn fetch_spawnable_user_revision(
        &self,
        user_id: &str,
    ) -> Result<Option<i32>, sqlx::Error> {
        sqlx::query_as::<_, (i32,)>(SPAWNABLE_USER_SQL)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await
            .map(|row| row.map(|(rev,)| rev))
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

/// Decide what to do with a user's worker slot from the DB revision and the
/// in-memory worker's revision.
///
/// Any `db_ver != active_ver` is a `Replace`. `pulsoid_revision_seq` is
/// monotonic, so a DB revision *lower* than the active worker's only happens
/// after a DB restore / PITR / manual surgery. Rather than confirm such a
/// rewind with an extra DB round-trip, we just replace: the worst case is a
/// possibly-stale worker for at most one reconcile cycle, which the periodic
/// reconcile (or the next `connection.changed` hint) corrects anyway.
fn worker_action(db_version: Option<i32>, active_version: Option<i32>) -> WorkerAction {
    match (db_version, active_version) {
        (Some(db_ver), None) => WorkerAction::Spawn { revision: db_ver },
        (Some(db_ver), Some(active_ver)) if db_ver != active_ver => WorkerAction::Replace {
            old_revision: active_ver,
            new_revision: db_ver,
        },
        (None, Some(active_ver)) => WorkerAction::Stop {
            revision: active_ver,
        },
        _ => WorkerAction::Noop,
    }
}

fn log_noop(user_id: &str, db_version: Option<i32>, active_version: Option<i32>) {
    if let (Some(db_ver), Some(active_ver)) = (db_version, active_version)
        && db_ver == active_ver
    {
        tracing::debug!(user_id, db_ver, "apply: in-sync, no-op");
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkerAction, worker_action};

    #[test]
    fn lower_db_revision_replaces_active_worker() {
        // A DB revision lower than the active worker's normally never happens
        // (`pulsoid_revision_seq` is monotonic), but a DB restore / PITR /
        // manual surgery can rewind it. We replace immediately rather than
        // spend a DB round-trip confirming the rewind — a one-cycle stale
        // worker is acceptable and the next reconcile corrects it anyway.
        assert_eq!(
            worker_action(Some(7), Some(8)),
            WorkerAction::Replace {
                old_revision: 8,
                new_revision: 7,
            }
        );
    }

    #[test]
    fn newer_db_revision_replaces_active_worker() {
        assert_eq!(
            worker_action(Some(8), Some(7)),
            WorkerAction::Replace {
                old_revision: 7,
                new_revision: 8,
            }
        );
    }

    #[test]
    fn spawnable_db_row_without_active_worker_spawns() {
        assert_eq!(
            worker_action(Some(3), None),
            WorkerAction::Spawn { revision: 3 }
        );
    }

    #[test]
    fn missing_spawnable_row_stops_active_worker() {
        assert_eq!(
            worker_action(None, Some(3)),
            WorkerAction::Stop { revision: 3 }
        );
    }

    #[test]
    fn matching_revisions_are_noop() {
        assert_eq!(worker_action(Some(3), Some(3)), WorkerAction::Noop);
    }

    #[test]
    fn missing_row_without_active_worker_is_noop() {
        assert_eq!(worker_action(None, None), WorkerAction::Noop);
    }
}
