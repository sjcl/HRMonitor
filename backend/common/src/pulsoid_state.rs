//! Shared primitives for reasoning about `pulsoid_connections` write outcomes.
//!
//! The `connection_state = 'error'` column acts as a sticky terminal signal:
//! once a row is in that state, only a fresh re-auth (OAuth callback or manual
//! token upload) may transition it out. All other writes in both `pulsoid-ingest`
//! and `api-backend` carry a `WHERE ... AND ($target = 'error' OR connection_state
//! != 'error')` guard so they can't resurrect a dead row.
//!
//! When a guarded UPDATE returns `rows_affected = 0` there are three distinct
//! causes (row gone, stale `revision`, sticky error refused). This module
//! provides the disambiguation SELECT so both binaries log the cases identically.

/// Outcome of a guarded UPDATE against `pulsoid_connections`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The UPDATE affected a row — the write succeeded.
    Applied,
    /// The row no longer exists or the caller's `revision` no longer
    /// matches the DB. Caller should treat itself as superseded and exit.
    StaleOrMissing,
    /// The row is in `connection_state = 'error'` and the sticky guard refused
    /// the transition. Caller should NOT retry — resurrect requires fresh
    /// re-auth (OAuth callback / manual token upload).
    StickyError,
}

/// Disambiguate the reason a guarded UPDATE returned `rows_affected = 0`.
///
/// This is a slow-path helper: it only runs when the write already failed to
/// apply, so the extra round trip does not affect the hot path.
pub async fn classify_no_op<'e, E>(
    db: E,
    user_id: &str,
    expected_revision: i32,
) -> Result<WriteOutcome, sqlx::Error>
where
    E: sqlx::PgExecutor<'e>,
{
    let row: Option<(String, i32)> = sqlx::query_as(
        "SELECT connection_state, revision FROM pulsoid_connections WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        None => WriteOutcome::StaleOrMissing,
        Some((_, rev)) if rev != expected_revision => WriteOutcome::StaleOrMissing,
        Some((cs, _)) if cs == "error" => WriteOutcome::StickyError,
        // Defensive fallback: the row exists, revision matches, and it
        // is not in error — some concurrent recovery write must have landed
        // between our guarded UPDATE and this SELECT. Treat as stale so the
        // caller bails out; the recovery write will spawn a fresh worker.
        Some(_) => WriteOutcome::StaleOrMissing,
    })
}
