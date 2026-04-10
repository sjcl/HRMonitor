//! Scan `pulsoid_connections` for rows whose tokens are expiring soon and
//! serially refresh each one via [`crate::refresh::refresh_if_expiring`].
//!
//! The scan is serial by design: Pulsoid OAuth refresh is idempotent per
//! row but not safe to run twice concurrently for the same user (single-use
//! refresh tokens). Serial processing in combination with the advisory
//! lock inside `refresh_if_expiring` gives us both per-process and
//! cross-process exclusivity.

use std::time::Instant;

use common::pulsoid_oauth::PulsoidOAuthConfig;
use common::token_encryption::TokenEncryption;
use sqlx::PgPool;

use crate::refresh::{self, RefreshOutcome};

pub async fn scan_and_refresh_once(
    db: &PgPool,
    nats: &async_nats::Client,
    encryption: &TokenEncryption,
    oauth: &PulsoidOAuthConfig,
) {
    let started = Instant::now();

    // ORDER BY token_expires_at ASC: under load (many tokens expiring in
    // the same window) this guarantees the most urgent row is always
    // processed first, even though processing is serial. Combined with
    // the 300s safety margin this gives us a theoretical upper bound of
    // ~10 concurrent expirations per scan (30s worst-case HTTP × 10 =
    // 300s) before the last one misses the deadline.
    let candidates: Vec<(String, i32)> = match sqlx::query_as(
        "SELECT user_id, revision FROM pulsoid_connections
         WHERE source = 'oauth'
           AND connection_state != 'error'
           AND token_expires_at IS NOT NULL
           AND token_expires_at <= now() + make_interval(secs => $1)
         ORDER BY token_expires_at ASC",
    )
    .bind(refresh::REFRESH_SAFETY_MARGIN_SECS as f64)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("Scanner SELECT failed: {e}");
            return;
        }
    };

    if candidates.is_empty() {
        tracing::debug!(elapsed_ms = started.elapsed().as_millis() as u64, "Scan: no candidates");
        return;
    }

    let total = candidates.len();
    tracing::info!(total, "Scan: {total} candidate(s) picked up");

    let mut refreshed = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;

    for (user_id, expected_revision) in candidates {
        let outcome = refresh::refresh_if_expiring(
            db,
            nats,
            encryption,
            oauth,
            &user_id,
            expected_revision,
        )
        .await;

        match outcome {
            RefreshOutcome::Refreshed { .. } => refreshed += 1,
            RefreshOutcome::SkippedStillValid
            | RefreshOutcome::SkippedSuperseded
            | RefreshOutcome::SkippedStickyError
            | RefreshOutcome::SkippedLockContended => skipped += 1,
            RefreshOutcome::TerminalFailure | RefreshOutcome::TransientFailure => failed += 1,
        }
    }

    tracing::info!(
        total,
        refreshed,
        skipped,
        failed,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Scan complete"
    );
}
