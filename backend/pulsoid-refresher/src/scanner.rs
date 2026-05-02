//! Scan `pulsoid_connections` for rows whose tokens are expiring soon and
//! refresh them via [`crate::refresh::refresh_if_expiring`] with bounded
//! concurrency.
//!
//! Concurrency is bounded by a caller-supplied `concurrency` parameter
//! (sourced from `REFRESH_CONCURRENCY` env in `main.rs`). Within a single
//! scan pass, up to `concurrency` refreshes run in parallel using
//! [`FuturesUnordered`]. Per-user exclusivity is guaranteed by the Postgres
//! advisory lock inside `refresh_if_expiring` — if the same user somehow
//! appears twice in a candidate list, the second attempt will see
//! `SkippedLockContended` and return immediately.
//!
//! **Shutdown semantics (drain, don't drop):** when the shutdown flag is
//! set, the scan stops feeding new candidates into the work queue but lets
//! all already-started refreshes run to completion. This is necessary
//! because `refresh_if_expiring` commits `connection_state = 'pending'` in
//! Tx B before the HTTP call; dropping a mid-flight future would leave the
//! row stuck in `pending` with no Tx C to finalize it and no NATS
//! notification to pulsoid-ingest.

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use common::pulsoid_oauth::PulsoidOAuthConfig;
use common::token_encryption::TokenEncryption;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use sqlx::PgPool;
use tokio::sync::watch;

use crate::refresh::{self, RefreshOutcome};

type RefreshFuture<'a> = Pin<Box<dyn Future<Output = RefreshOutcome> + Send + 'a>>;

/// Run one scan pass. Returns `true` if interrupted by a shutdown signal.
pub async fn scan_and_refresh_once(
    db: &PgPool,
    nats: &async_nats::Client,
    encryption: &TokenEncryption,
    oauth: &PulsoidOAuthConfig,
    shutdown: &watch::Receiver<bool>,
    concurrency: usize,
) -> bool {
    let started = Instant::now();

    // ORDER BY token_expires_at ASC: most urgent rows are fed into the
    // concurrent work queue first, so they start refreshing immediately
    // even when the total candidate count exceeds `concurrency`.
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
            return false;
        }
    };

    if candidates.is_empty() {
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Scan: no candidates"
        );
        return false;
    }

    let total = candidates.len();
    tracing::info!(total, concurrency, "Scan: {total} candidate(s) picked up");

    let mut refreshed = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;

    // Manual feeding into FuturesUnordered gives us precise control over
    // shutdown: we stop submitting new work but drain all in-flight futures.
    let mut in_flight: FuturesUnordered<RefreshFuture<'_>> = FuturesUnordered::new();
    let mut iter = candidates.into_iter();
    let mut shutting_down = false;

    let make_future = |user_id: String, expected_revision: i32| -> RefreshFuture<'_> {
        Box::pin(async move {
            refresh::refresh_if_expiring(db, nats, encryption, oauth, &user_id, expected_revision)
                .await
        })
    };

    // Seed the initial batch (up to `concurrency` items).
    for (user_id, expected_revision) in iter.by_ref().take(concurrency) {
        in_flight.push(make_future(user_id, expected_revision));
    }

    while let Some(outcome) = in_flight.next().await {
        match outcome {
            RefreshOutcome::Refreshed { .. } => refreshed += 1,
            RefreshOutcome::SkippedStillValid
            | RefreshOutcome::SkippedSuperseded
            | RefreshOutcome::SkippedStickyError
            | RefreshOutcome::SkippedLockContended => skipped += 1,
            RefreshOutcome::TerminalFailure | RefreshOutcome::TransientFailure => failed += 1,
        }

        // Check shutdown: stop feeding new work, but let in-flight complete.
        if !shutting_down && *shutdown.borrow() {
            shutting_down = true;
            let remaining_in_flight = in_flight.len();
            tracing::info!(
                remaining_in_flight,
                "Shutdown requested, draining {remaining_in_flight} in-flight refresh(es)"
            );
            // Don't feed any more candidates; the while loop will drain
            // what's already in `in_flight`.
            continue;
        }

        // Feed the next candidate if we're not shutting down.
        if !shutting_down && let Some((user_id, expected_revision)) = iter.next() {
            in_flight.push(make_future(user_id, expected_revision));
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
    shutting_down
}
