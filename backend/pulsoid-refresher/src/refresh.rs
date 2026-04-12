//! Per-user proactive refresh of a Pulsoid OAuth token.
//!
//! This module is a port of the old `handle_token_refresh` function that
//! lived in api-backend, restructured around three invariants:
//!
//! 1. **Cross-process dedup via Postgres advisory lock.** `Tx A` holds
//!    `pg_try_advisory_xact_lock(4242, hashtext('pulsoid_refresh:' || user_id))`
//!    for the entire lifetime of the refresh. Other refresher instances
//!    attempting the same user see `false` and skip. The lock is an xact
//!    lock so it is auto-released on any exit path (commit / rollback /
//!    panic / connection drop).
//!
//! 2. **No long-held row locks.** The advisory lock is taken in a dedicated
//!    transaction that performs *only* the lock SELECT — no row locks are
//!    ever acquired in `Tx A`. Actual SELECT / UPDATE of `pulsoid_connections`
//!    happens in short-lived `Tx B` (pending UPDATE) and `Tx C` (final
//!    UPDATE) on separate connections. The Pulsoid OAuth HTTP call (up to
//!    ~30s) happens with no transaction open at all on the DML side, so
//!    OAuth callback / manual token PUT handlers are never blocked waiting
//!    on a row lock held by the refresher.
//!
//! 3. **Sticky-error invariant preserved.** Every write to
//!    `pulsoid_connections` here keeps the existing guard
//!    `AND ($target = 'error' OR connection_state != 'error')` so the
//!    refresher can never resurrect a terminal row. Disambiguation of
//!    zero-row updates is delegated to [`classify_no_op`] for identical
//!    logging shape with the other services.

use common::pulsoid_oauth::{OAuthError, PulsoidOAuthConfig};
use common::pulsoid_state::{WriteOutcome, classify_no_op};
use common::time::unix_now_secs;
use common::token_encryption::TokenEncryption;
use sqlx::PgPool;

/// Refresh window. The scanner picks up any row whose `token_expires_at`
/// is within this many seconds of `now()`, and `refresh_if_expiring` uses
/// the same constant in its internal re-check to avoid a "scanner picked
/// it, function skipped it" gap.
///
/// Sized to comfortably cover: Pulsoid OAuth refresh call (up to ~30s) +
/// `CONNECTION_CHANGED` NATS hint propagation + `WorkerManager`
/// `reconcile_user` + `replace_worker` swap + one fallback scan cycle.
/// The worker-side `REFRESH_SAFETY_MARGIN_SECS` in pulsoid-ingest is a
/// *different* threshold (the "do not reconnect with a token this close to
/// expiry" floor) and must remain strictly smaller than this value.
pub const REFRESH_SAFETY_MARGIN_SECS: i64 = 300;

/// Namespace int for `pg_try_advisory_xact_lock`. Fixed constant so other
/// advisory-lock users (current or future) can pick different namespaces
/// and never collide even on `hashtext` bucket overlap.
const ADVISORY_LOCK_NAMESPACE: i32 = 4242;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // variants are constructed via enum returns; fields used in tracing
pub enum RefreshOutcome {
    /// Refresh succeeded and the new `revision` landed in the DB.
    Refreshed { new_revision: i32 },
    /// Token is still valid (internal re-check after advisory lock).
    SkippedStillValid,
    /// Row was superseded before we could apply the update — a concurrent
    /// writer (OAuth callback, manual PUT) bumped `revision` or
    /// removed the row entirely.
    SkippedSuperseded,
    /// Row is in the terminal `'error'` state. Only fresh re-auth can
    /// resurrect it.
    SkippedStickyError,
    /// Another refresher instance was already handling this user.
    SkippedLockContended,
    /// Pulsoid returned 401 / `invalid_grant` — refresh token is dead.
    TerminalFailure,
    /// Transient failure (network error, non-401 HTTP error). Next scan
    /// will try again.
    TransientFailure,
}

/// Attempt to refresh the OAuth token for `user_id` if its `revision`
/// still matches the value the scanner observed. All side effects are
/// described in the module docs; the returned [`RefreshOutcome`] is the
/// only channel the caller has to tell what happened.
pub async fn refresh_if_expiring(
    db: &PgPool,
    nats: &async_nats::Client,
    encryption: &TokenEncryption,
    oauth: &PulsoidOAuthConfig,
    user_id: &str,
    expected_revision: i32,
) -> RefreshOutcome {
    // ---- Tx A: advisory lock only ------------------------------------------------
    // This transaction exists solely to hold the xact-scoped advisory lock
    // for the full refresh lifetime. It must not execute any DML, and must
    // not be rolled back until we are done with everything else — otherwise
    // the lock is released early and a concurrent refresher could start
    // working on the same user.
    let mut lock_tx = match db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(user_id, "Failed to open advisory-lock tx: {e}");
            return RefreshOutcome::TransientFailure;
        }
    };

    let acquired: Result<(bool,), _> = sqlx::query_as(
        "SELECT pg_try_advisory_xact_lock($1, hashtext('pulsoid_refresh:' || $2))",
    )
    .bind(ADVISORY_LOCK_NAMESPACE)
    .bind(user_id)
    .fetch_one(&mut *lock_tx)
    .await;

    match acquired {
        Ok((true,)) => {}
        Ok((false,)) => {
            tracing::debug!(user_id, "Advisory lock contended, skipping");
            let _ = lock_tx.rollback().await;
            return RefreshOutcome::SkippedLockContended;
        }
        Err(e) => {
            tracing::error!(user_id, "Advisory lock acquisition failed: {e}");
            let _ = lock_tx.rollback().await;
            return RefreshOutcome::TransientFailure;
        }
    }

    // From this point on `lock_tx` must be kept alive until the end of the
    // function. All DML runs on *different* connections obtained from the
    // pool, so the row locks they take are short-lived while the advisory
    // lock stays held.
    let outcome = refresh_inner(db, nats, encryption, oauth, user_id, expected_revision)
        .await;

    // Release the advisory lock. Either direction is fine (commit vs
    // rollback — no DML was executed) but commit is cheaper and clearer.
    if let Err(e) = lock_tx.commit().await {
        tracing::warn!(user_id, "Failed to commit advisory-lock tx: {e}");
    }

    outcome
}

#[derive(sqlx::FromRow)]
struct ConnectionRow {
    source: String,
    refresh_token: Option<Vec<u8>>,
    key_version: i32,
    token_expires_at: Option<i64>,
    connection_state: String,
    revision: i32,
}

/// Body of the refresh. Split out so the caller can keep `lock_tx` alive
/// over its entire lifetime via RAII without having to thread the
/// transaction handle through every branch.
async fn refresh_inner(
    db: &PgPool,
    nats: &async_nats::Client,
    encryption: &TokenEncryption,
    oauth: &PulsoidOAuthConfig,
    user_id: &str,
    expected_revision: i32,
) -> RefreshOutcome {
    // ---- Tx B: SELECT current row + flip to 'pending' -----------------------------
    // Short-lived. Uses a fresh connection from the pool (NOT `lock_tx`).
    // Row-lock hold time is < 100ms because no HTTP call happens inside it.
    let mut tx_b = match db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(user_id, "Failed to open Tx B: {e}");
            return RefreshOutcome::TransientFailure;
        }
    };

    let row: Option<ConnectionRow> = match sqlx::query_as(
        "SELECT source, refresh_token, key_version,
                EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at,
                connection_state, revision
         FROM pulsoid_connections WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx_b)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(user_id, "Failed to fetch connection for refresh: {e}");
            let _ = tx_b.rollback().await;
            return RefreshOutcome::TransientFailure;
        }
    };

    let ConnectionRow {
        source,
        refresh_token: refresh_token_enc,
        key_version,
        token_expires_at,
        connection_state,
        revision: db_revision,
    } = match row {
        Some(r) => r,
        None => {
            tracing::info!(user_id, "No pulsoid connection found, skipping refresh");
            let _ = tx_b.rollback().await;
            return RefreshOutcome::SkippedSuperseded;
        }
    };

    if db_revision != expected_revision {
        tracing::info!(
            user_id,
            expected_revision,
            db_revision,
            "Refresh skipped: connection superseded (revision mismatch)"
        );
        let _ = tx_b.rollback().await;
        return RefreshOutcome::SkippedSuperseded;
    }

    if source != "oauth" {
        tracing::debug!(user_id, "Skipping non-OAuth connection");
        let _ = tx_b.rollback().await;
        return RefreshOutcome::SkippedSuperseded;
    }

    if connection_state == "error" {
        tracing::debug!(
            user_id,
            "Connection in terminal 'error' state, skipping refresh"
        );
        let _ = tx_b.rollback().await;
        return RefreshOutcome::SkippedStickyError;
    }

    // Internal re-check against the same threshold the scanner SQL uses.
    // Without this, a row that was expiring 5 minutes out at scan time but
    // got refreshed by an OAuth callback between scan and now would still
    // go through a full refresh cycle unnecessarily.
    if let Some(expires_at) = token_expires_at {
        let now = unix_now_secs();
        if now < expires_at - REFRESH_SAFETY_MARGIN_SECS {
            tracing::debug!(user_id, "Token still valid, skipping refresh");
            let _ = tx_b.rollback().await;
            return RefreshOutcome::SkippedStillValid;
        }
    }

    // Decrypt refresh token BEFORE flipping to 'pending'. If decryption
    // fails we want to write the terminal error without ever transitioning
    // through 'pending' first.
    let refresh_token_bytes = match refresh_token_enc {
        Some(rt) => rt,
        None => {
            tracing::error!(user_id, "OAuth connection has no refresh_token");
            let _ = tx_b.rollback().await;
            write_error_state(
                db,
                nats,
                user_id,
                expected_revision,
                "No refresh token available",
                true,
            )
            .await;
            return RefreshOutcome::TerminalFailure;
        }
    };

    let refresh_token_plain = match encryption.decrypt(&refresh_token_bytes, key_version as u32) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(user_id, "Failed to decrypt refresh token: {e}");
            let _ = tx_b.rollback().await;
            write_error_state(
                db,
                nats,
                user_id,
                expected_revision,
                &format!("Failed to decrypt refresh token: {e}"),
                true,
            )
            .await;
            return RefreshOutcome::TerminalFailure;
        }
    };

    // Flip row to 'pending'. Sticky-error guard: the UPDATE refuses to
    // touch an 'error' row even though we re-checked above — a concurrent
    // writer could have flipped it between our SELECT and this UPDATE.
    match sqlx::query(
        "UPDATE pulsoid_connections SET connection_state = 'pending', state_updated_at = now() \
         WHERE user_id = $1 AND revision = $2 AND connection_state != 'error'",
    )
    .bind(user_id)
    .bind(expected_revision)
    .execute(&mut *tx_b)
    .await
    {
        Ok(r) if r.rows_affected() == 0 => {
            let _ = tx_b.rollback().await;
            match classify_no_op(db, user_id, expected_revision).await {
                Ok(WriteOutcome::StickyError) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Refresh abandoned: row is in sticky error state"
                    );
                    return RefreshOutcome::SkippedStickyError;
                }
                Ok(_) => {
                    tracing::info!(
                        user_id,
                        expected_revision,
                        "Token refresh abandoned: connection superseded"
                    );
                    return RefreshOutcome::SkippedSuperseded;
                }
                Err(e) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Failed to classify zero-row update: {e}"
                    );
                    return RefreshOutcome::TransientFailure;
                }
            }
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(user_id, "Failed to set pending state: {e}");
            let _ = tx_b.rollback().await;
            return RefreshOutcome::TransientFailure;
        }
    }

    if let Err(e) = tx_b.commit().await {
        tracing::error!(user_id, "Tx B commit failed: {e}");
        return RefreshOutcome::TransientFailure;
    }

    // ---- HTTP call: no transaction open on the DML side --------------------------
    // The advisory lock (Tx A) is still held on a *different* connection,
    // so concurrent refreshers are still blocked. But no row locks are
    // held, so OAuth callback / manual PUT are free to run.
    let token_resp = match oauth.refresh_token(&refresh_token_plain).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(user_id, "Token refresh failed: {e}");
            let is_terminal = match &e {
                OAuthError::TokenEndpoint { status, body } => {
                    *status == 401
                        || serde_json::from_str::<serde_json::Value>(body)
                            .ok()
                            .and_then(|v| {
                                v.get("error")?.as_str().map(|s| s == "invalid_grant")
                            })
                            .unwrap_or(false)
                }
                OAuthError::Request(_) => false,
            };
            write_error_state(
                db,
                nats,
                user_id,
                expected_revision,
                &format!("Token refresh failed: {e}"),
                is_terminal,
            )
            .await;
            return if is_terminal {
                RefreshOutcome::TerminalFailure
            } else {
                RefreshOutcome::TransientFailure
            };
        }
    };

    // ---- Tx C: final UPDATE ------------------------------------------------------
    let refresh_token_for_upd = token_resp.refresh_token.as_deref();
    let (enc_access, new_key_version) = encryption.encrypt(&token_resp.access_token);
    let enc_refresh: Vec<u8> = match refresh_token_for_upd {
        Some(new_rt) => encryption.encrypt(new_rt).0,
        // Pulsoid returned no new refresh token: re-encrypt the existing
        // plaintext under the current key. The schema stores a single
        // `key_version` column shared by `access_token` and `refresh_token`,
        // and the UPDATE below overwrites it with `new_key_version`. Reusing
        // the stale ciphertext (which may have been encrypted under an
        // older key version) would leave the refresh_token associated with
        // the wrong key_version the moment a rotation introduces a second
        // key, breaking decrypt on the next refresh tick.
        None => encryption.encrypt(&refresh_token_plain).0,
    };

    let mut tx_c = match db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(user_id, "Failed to open Tx C: {e}");
            return RefreshOutcome::TransientFailure;
        }
    };

    let result: Result<Option<(i32,)>, _> = sqlx::query_as(
        "UPDATE pulsoid_connections
         SET access_token = $1, refresh_token = $2, key_version = $3,
             token_expires_at = now() + make_interval(secs => $4),
             last_error = NULL,
             connection_state = 'pending', state_updated_at = now(),
             revision = nextval('pulsoid_revision_seq')
         WHERE user_id = $5 AND source = 'oauth' AND revision = $6
           AND connection_state != 'error'
         RETURNING revision",
    )
    .bind(&enc_access)
    .bind(&enc_refresh)
    .bind(new_key_version as i32)
    .bind(token_resp.expires_in as f64)
    .bind(user_id)
    .bind(expected_revision)
    .fetch_optional(&mut *tx_c)
    .await;

    let new_revision = match result {
        Ok(Some((rev,))) => rev,
        Ok(None) => {
            let _ = tx_c.rollback().await;
            match classify_no_op(db, user_id, expected_revision).await {
                Ok(WriteOutcome::StickyError) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Refreshed tokens discarded: row is in sticky error state (resurrect only via fresh re-auth)"
                    );
                    return RefreshOutcome::SkippedStickyError;
                }
                Ok(_) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Refreshed tokens discarded: connection superseded"
                    );
                    return RefreshOutcome::SkippedSuperseded;
                }
                Err(e) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Failed to classify zero-row update: {e}"
                    );
                    return RefreshOutcome::TransientFailure;
                }
            }
        }
        Err(e) => {
            tracing::error!(user_id, "Failed to save refreshed tokens: {e}");
            let _ = tx_c.rollback().await;
            return RefreshOutcome::TransientFailure;
        }
    };

    if let Err(e) = tx_c.commit().await {
        tracing::error!(user_id, "Tx C commit failed: {e}");
        return RefreshOutcome::TransientFailure;
    }

    tracing::info!(
        user_id,
        revision = new_revision,
        "Token refreshed successfully"
    );

    publish_connection_changed(nats, user_id).await;

    RefreshOutcome::Refreshed { new_revision }
}

/// Write an error state to `pulsoid_connections` in a dedicated short-lived
/// transaction on a fresh connection. Used both for terminal failures
/// (refresh token dead, decryption failed) and for transient recovery
/// attempts.
///
/// When `is_terminal` is true the write is unconditional within the
/// `revision` check (sticky-error guard is disabled). Otherwise the
/// guard is enforced so we don't overwrite an existing `'error'` row with
/// `'pending'`.
async fn write_error_state(
    db: &PgPool,
    nats: &async_nats::Client,
    user_id: &str,
    expected_revision: i32,
    last_error: &str,
    is_terminal: bool,
) {
    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(user_id, "Failed to open error-write tx: {e}");
            return;
        }
    };

    let res = sqlx::query(
        "UPDATE pulsoid_connections SET last_error = $1,
         connection_state = CASE WHEN $3 THEN 'error' ELSE 'pending' END,
         state_updated_at = now()
         WHERE user_id = $2 AND revision = $4
           AND ($3 OR connection_state != 'error')",
    )
    .bind(last_error)
    .bind(user_id)
    .bind(is_terminal)
    .bind(expected_revision)
    .execute(&mut *tx)
    .await;

    let did_write = match res {
        Ok(r) if r.rows_affected() == 0 => {
            match classify_no_op(db, user_id, expected_revision).await {
                Ok(WriteOutcome::StickyError) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Non-terminal refresh error not written: row is in sticky error state"
                    );
                }
                Ok(_) => {
                    tracing::info!(
                        user_id,
                        expected_revision,
                        "Error state not written: connection superseded"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        user_id,
                        expected_revision,
                        "Failed to classify zero-row update: {e}"
                    );
                }
            }
            false
        }
        Ok(_) => {
            if is_terminal {
                tracing::warn!(
                    user_id,
                    "Terminal refresh failure, blocking further attempts"
                );
            }
            true
        }
        Err(e) => {
            tracing::error!(user_id, "Failed to update connection error state: {e}");
            false
        }
    };

    if let Err(e) = tx.commit().await {
        tracing::warn!(user_id, "Error-write tx commit failed: {e}");
        return;
    }

    // Publish the hint only after a successful commit of a terminal error
    // state, so pulsoid-ingest can immediately stop the worker instead of
    // waiting for the next 60-second reconcile pass.
    if did_write && is_terminal {
        publish_connection_changed(nats, user_id).await;
    }
}

/// Publish a `CONNECTION_CHANGED` hint via NATS so pulsoid-ingest can
/// reconcile immediately. Best-effort: failures are logged but do not
/// affect the caller's outcome.
async fn publish_connection_changed(nats: &async_nats::Client, user_id: &str) {
    let cmd = common::messages::ConnectionChangeCommand {
        user_id: user_id.to_string(),
    };
    match serde_json::to_vec(&cmd) {
        Ok(payload) => {
            if let Err(e) = nats
                .publish(common::messages::subjects::CONNECTION_CHANGED, payload.into())
                .await
            {
                tracing::warn!(
                    user_id,
                    "Failed to publish connection change hint: {e}"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                user_id,
                "Failed to serialize connection change hint: {e}"
            );
        }
    }
}
