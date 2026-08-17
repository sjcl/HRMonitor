mod ws;

use axum::Router;
use axum::middleware;
use axum::routing::get;
use futures_util::{StreamExt, TryStreamExt};
use redis::{ExistenceCheck, SetExpiry, SetOptions};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast as tokio_broadcast;
use tokio_util::sync::CancellationToken;

use common::auth::{AuthConfig, AuthContext};
use common::jwt::JwtVerifier;
use common::messages::{HeartRateReceived, subjects};
use common::nats_backoff::{INITIAL_BACKOFF, advance_backoff};
use common::origin::OriginContext;
use common::redis_keys::{latest_bpm_key, latest_bpm_ttl_secs, serialize_latest_bpm};
use common::signal::{shutdown_signal, spawn_critical_task};
use common::time::unix_now_secs;

pub struct WsState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::MultiplexedConnection,
    pub hr_broadcast: tokio_broadcast::Sender<HeartRateReceived>,
    pub auth_config: AuthConfig,
    /// Verify-only. ws-gateway enables `common`'s `jwt` feature but not
    /// `jwt-issue`, so no signing key or code that reads one exists in this
    /// binary at all.
    pub jwt_verifier: JwtVerifier,
    /// Canonical browser origin (`scheme://host[:port]`) allowed to open
    /// WebSocket connections. Derived from `PUBLIC_ORIGIN` at startup.
    pub allowed_ws_origin: String,
    pub shutdown: CancellationToken,
}

impl AuthContext for WsState {
    fn db(&self) -> &sqlx::PgPool {
        &self.db
    }
    fn auth_config(&self) -> &AuthConfig {
        &self.auth_config
    }
    fn jwt_verifier(&self) -> &JwtVerifier {
        &self.jwt_verifier
    }
}

impl OriginContext for WsState {
    fn allowed_origin(&self) -> &str {
        &self.allowed_ws_origin
    }
}

/// One-RTT upper bound for a warm-up Redis pipeline. Large enough to amortise
/// the round-trip, small enough that one reply stays well under socket buffers.
const WARM_UP_CHUNK_SIZE: usize = 500;

type WarmUpRow = (String, i32, i64, i64);

/// Sends a single pipelined `SET NX EX` batch for `chunk` and updates the
/// counters. No-op on empty. Rows whose `recorded_at` is already past
/// `LATEST_BPM_TTL_SECS` are dropped without ever entering the pipeline —
/// otherwise we'd resurrect stale readings that the snapshot path is supposed
/// to consider expired. Failures are warned and the chunk is dropped — warm-up
/// is best-effort.
async fn flush_chunk(
    chunk: &mut Vec<WarmUpRow>,
    redis_conn: &mut redis::aio::MultiplexedConnection,
    now: i64,
    warmed: &mut u64,
    nx_skipped: &mut u64,
    stale_skipped: &mut u64,
) {
    if chunk.is_empty() {
        return;
    }
    let mut pipe = redis::pipe();
    let mut pipeline_len: usize = 0;
    for (user_id, bpm, recorded_at, received_at) in chunk.iter() {
        let ttl = match latest_bpm_ttl_secs(now, *recorded_at) {
            Some(t) => t,
            None => {
                *stale_skipped += 1;
                continue;
            }
        };
        let update = HeartRateReceived {
            user_id: user_id.clone(),
            bpm: *bpm,
            recorded_at: *recorded_at,
            received_at: *received_at,
        };
        let value = serialize_latest_bpm(&update);
        let key = latest_bpm_key(user_id);
        let opts = SetOptions::default()
            .conditional_set(ExistenceCheck::NX)
            .with_expiration(SetExpiry::EX(ttl));
        pipe.set_options(&key, &value, opts);
        pipeline_len += 1;
    }
    let chunk_len = chunk.len();
    let first_user_id = chunk.first().map(|r| r.0.clone());
    let last_user_id = if chunk_len > 1 {
        chunk.last().map(|r| r.0.clone())
    } else {
        None
    };
    chunk.clear();

    // Every row in the chunk was stale — don't fire an empty pipeline; the
    // backend's behavior on a zero-command pipe is unspecified.
    if pipeline_len == 0 {
        return;
    }

    match pipe.query_async::<Vec<Option<String>>>(redis_conn).await {
        Ok(results) => {
            if results.len() != pipeline_len {
                tracing::warn!(
                    pipeline_len,
                    result_len = results.len(),
                    "warm-up pipeline result length mismatch"
                );
            }
            for r in results {
                if r.is_some() {
                    *warmed += 1;
                } else {
                    *nx_skipped += 1;
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                chunk_len,
                pipeline_len,
                first_user_id = ?first_user_id,
                last_user_id = ?last_user_id,
                error = %e,
                "warm-up SET NX pipeline failed"
            );
        }
    }
}

/// Populates `latest_bpm:{user_id}` from the last 6h of `heart_rate_records`
/// using `SET NX EX`, so pulsoid-ingest's authoritative plain `SET` is never
/// overwritten. Failures are logged but not fatal — the WS self_heal path
/// will recover once writes resume.
///
/// Rows stream from Postgres and are flushed to Redis in `WARM_UP_CHUNK_SIZE`
/// pipelined batches. On shutdown we stop pulling new rows but flush the
/// buffered tail once so already-fetched work isn't discarded.
async fn warm_latest_bpm_cache(
    pool: sqlx::PgPool,
    mut redis_conn: redis::aio::MultiplexedConnection,
    shutdown: CancellationToken,
) {
    // SQL prefilter: drop rows already past the 6h horizon at query time so the
    // happy path doesn't pull six months of history. The final stale check
    // lives in `latest_bpm_ttl_secs` (called inside `flush_chunk`) — that
    // guarantees rows that crossed the horizon between the query and the flush
    // are still rejected, and centralises the boundary semantics.
    let mut stream = sqlx::query_as::<_, WarmUpRow>(
        "SELECT DISTINCT ON (user_id) user_id, bpm, \
         EXTRACT(EPOCH FROM recorded_at)::BIGINT AS recorded_at, \
         EXTRACT(EPOCH FROM received_at)::BIGINT AS received_at \
         FROM heart_rate_records \
         WHERE recorded_at >= NOW() - INTERVAL '6 hours' \
         ORDER BY user_id, recorded_at DESC, received_at DESC, id DESC",
    )
    .fetch(&pool);

    let now = unix_now_secs();
    let mut chunk: Vec<WarmUpRow> = Vec::with_capacity(WARM_UP_CHUNK_SIZE);
    let mut warmed = 0u64;
    let mut nx_skipped = 0u64;
    let mut stale_skipped = 0u64;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            row = stream.try_next() => match row {
                Ok(Some(r)) => {
                    chunk.push(r);
                    if chunk.len() >= WARM_UP_CHUNK_SIZE {
                        flush_chunk(
                            &mut chunk,
                            &mut redis_conn,
                            now,
                            &mut warmed,
                            &mut nx_skipped,
                            &mut stale_skipped,
                        )
                        .await;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "warm-up DB scan failed mid-stream");
                    break;
                }
            }
        }
    }

    // Flush the buffered tail: cancel (and mid-stream DB errors) stop new
    // reads, but rows we already paid to fetch are still best-effort-warmable.
    flush_chunk(
        &mut chunk,
        &mut redis_conn,
        now,
        &mut warmed,
        &mut nx_skipped,
        &mut stale_skipped,
    )
    .await;
    tracing::info!(
        warmed,
        nx_skipped,
        stale_skipped,
        cancelled = shutdown.is_cancelled(),
        "Warmed latest_bpm cache from DB"
    );
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ws_gateway=info".parse().unwrap()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor".into());

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("Failed to initialize database");

    let redis_client = redis::Client::open(redis_url).expect("Invalid REDIS_URL");
    let redis_conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to connect to Redis");

    tracing::info!("Connected to Redis");

    let shutdown = CancellationToken::new();

    // Warm latest_bpm cache in the background so axum::serve can start
    // accepting WS upgrades immediately. `SET NX EX` preserves any fresher
    // value pulsoid-ingest has already written, so warm-up ordering against
    // live writes is a non-issue. Clients that connect mid-warm-up may see
    // a null initial snapshot for inactive users; the WS handler's 10s
    // self_heal_interval converts that to an Update once warm-up lands.
    //
    // The task opens its own MultiplexedConnection so its 500-command
    // pipelines cannot queue in front of WS read_snapshot() MGETs on the
    // connection stored in WsState. Acquisition happens inside the task:
    // warm-up is best-effort, so a failure here must not block startup.
    let _warm_up_task = tokio::spawn({
        let pool = pool.clone();
        let redis_client = redis_client.clone();
        let shutdown = shutdown.clone();
        async move {
            let conn = match redis_client.get_multiplexed_async_connection().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "warm-up Redis connection unavailable; skipping warm-up \
                         — self_heal will recover"
                    );
                    return;
                }
            };
            warm_latest_bpm_cache(pool, conn, shutdown).await;
        }
    });

    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    tracing::info!("Connected to NATS at {nats_url}");

    let (hr_tx, _) = tokio_broadcast::channel::<HeartRateReceived>(4096);

    let allowed_ws_origin = common::origin::load_allowed_origin();
    let public_origin = url::Url::parse(&allowed_ws_origin).expect("canonical origin must parse");
    let auth_config = AuthConfig::from_env(&public_origin);
    let jwt_verifier = JwtVerifier::from_env();
    tracing::info!(
        allowed_ws_origin = %allowed_ws_origin,
        access_cookie = %auth_config.access_cookie_name,
        "Auth config and WS origin allowlist loaded"
    );

    // Detached task: flips the shutdown token when SIGTERM/SIGINT arrives.
    // Dropped with the tokio runtime at end of main.
    let _signal_task = tokio::spawn({
        let token = shutdown.clone();
        async move {
            shutdown_signal().await;
            tracing::info!("Received shutdown signal");
            token.cancel();
        }
    });

    let state = Arc::new(WsState {
        db: pool.clone(),
        redis: redis_conn.clone(),
        hr_broadcast: hr_tx.clone(),
        auth_config,
        jwt_verifier,
        allowed_ws_origin,
        shutdown: shutdown.clone(),
    });

    // Spawn hr.received NATS subscriber. Every .await inside the task is
    // wrapped in a biased select! against `shutdown.cancelled()` so the task
    // returns cooperatively during HTTP graceful shutdown.
    let _hr_sub_task = {
        let hr_tx = hr_tx.clone();
        let nats = nats.clone();
        let shutdown = shutdown.clone();
        let task_shutdown = shutdown.clone();
        spawn_critical_task(
            "NATS hr.received subscriber",
            Some(task_shutdown),
            async move {
                let mut backoff = INITIAL_BACKOFF;
                loop {
                    let sub_result = tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return,
                        r = nats.subscribe(subjects::HR_RECEIVED) => r,
                    };
                    let mut hr_sub = match sub_result {
                        Ok(s) => {
                            tracing::info!("Subscribed to {}", subjects::HR_RECEIVED);
                            s
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to subscribe to {}: {e}; retrying in {:?}",
                                subjects::HR_RECEIVED,
                                backoff
                            );
                            tokio::select! {
                                biased;
                                _ = shutdown.cancelled() => return,
                                _ = tokio::time::sleep(backoff) => {}
                            }
                            backoff = advance_backoff(backoff);
                            continue;
                        }
                    };

                    loop {
                        tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => return,
                            next = hr_sub.next() => match next {
                                Some(msg) => {
                                    // Receiving any message proves the subscription
                                    // is healthy; reset the resubscribe backoff.
                                    backoff = INITIAL_BACKOFF;
                                    match serde_json::from_slice::<HeartRateReceived>(&msg.payload) {
                                        Ok(update) => {
                                            let _ = hr_tx.send(update);
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to parse hr.received event: {e}");
                                        }
                                    }
                                }
                                None => break,
                            }
                        }
                    }

                    // A "subscribe succeeds → stream ends immediately" flap must
                    // not hot-loop: back off exponentially here, reset only on a
                    // received message above.
                    backoff = advance_backoff(backoff);
                    tracing::warn!(
                        "{} subscription ended; resubscribing in {:?}",
                        subjects::HR_RECEIVED,
                        backoff
                    );
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                }
            },
        )
    };

    let ws_routes = Router::new()
        .route("/api/ws/me", get(ws::my_heart_rate_ws))
        .route("/api/ws/users/{id}", get(ws::user_heart_rate_ws))
        .route("/api/ws/groups/{id}", get(ws::group_heart_rate_ws))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            common::auth::require_auth::<WsState>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            common::origin::require_origin_always::<WsState>,
        ));

    let public_routes = Router::new().route("/healthz", get(|| async { "ok" }));

    let app = Router::new()
        .merge(public_routes)
        .merge(ws_routes)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3002")
        .await
        .expect("Failed to bind to port 3002");

    tracing::info!("Server listening on 0.0.0.0:3002");

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .await;

    if let Err(e) = serve_result {
        tracing::error!("Server error: {e}");
        std::process::exit(1);
    }

    // Best-effort: ws-gateway does not publish on `hr.received`, so a
    // timed-out flush drops only UNSUB/PING-class frames. We log and exit
    // 0 to keep total shutdown inside the Compose 10s grace. Revisit if
    // this service ever gains a publish responsibility.
    match tokio::time::timeout(Duration::from_secs(1), nats.flush()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("NATS flush on shutdown failed: {e}"),
        Err(_) => tracing::warn!("NATS flush timed out after 1s on shutdown"),
    }

    tracing::info!("ws-gateway shut down gracefully");
}

#[cfg(test)]
mod tests {}
