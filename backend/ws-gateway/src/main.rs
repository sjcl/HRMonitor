mod ws;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::{StreamExt, TryStreamExt};
use redis::{ExistenceCheck, SetExpiry, SetOptions};
use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast as tokio_broadcast;
use tokio_util::sync::CancellationToken;

use common::access::ensure_can_view_user;
use common::auth::{AuthConfig, AuthContext, AuthenticatedUser, UserIdParam};
use common::error::AppError;
use common::messages::{HeartRateReceived, subjects};
use common::nats_backoff::{INITIAL_BACKOFF, STABILITY_THRESHOLD, advance_backoff};
use common::redis_keys::{LATEST_BPM_TTL_SECS, latest_bpm_key, serialize_latest_bpm};
use common::signal::{log_task_exit, shutdown_signal};
use common::time::unix_now_secs;

pub struct WsState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::MultiplexedConnection,
    pub hr_broadcast: tokio_broadcast::Sender<HeartRateReceived>,
    pub auth_config: AuthConfig,
    /// Canonical browser origin (`scheme://host[:port]`) allowed to open
    /// WebSocket connections. Derived from `AUTH_URL` at startup.
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
}

pub struct ViewableUserId(pub String);

impl FromRequestParts<Arc<WsState>> for ViewableUserId {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<WsState>,
    ) -> Result<Self, Self::Rejection> {
        let auth_user = parts
            .extensions
            .get::<AuthenticatedUser>()
            .cloned()
            .ok_or_else(|| AppError::Unauthorized("Not authenticated".into()))?;
        let UserIdParam(target_id) = UserIdParam::from_request_parts(parts, state).await?;
        ensure_can_view_user(&state.db, &auth_user, &target_id).await?;
        Ok(ViewableUserId(target_id))
    }
}

/// Parses `AUTH_URL` into a canonical browser origin (`scheme://host[:port]`).
/// Fails closed: in release builds a missing `AUTH_URL` panics at startup so
/// misconfiguration cannot silently reopen cross-site WS access.
fn load_allowed_ws_origin() -> String {
    let raw = std::env::var("AUTH_URL").ok();
    let raw = match raw.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => {
            if cfg!(debug_assertions) {
                tracing::warn!(
                    "AUTH_URL is not set; defaulting allowed WS origin to \
                     http://localhost:3000 (debug build only — release builds panic)"
                );
                return canonical_ws_origin("http://localhost:3000");
            } else {
                panic!(
                    "AUTH_URL must be set in release builds. It is used to \
                     validate the Origin header on /api/ws/* handshakes."
                );
            }
        }
    };

    canonical_ws_origin(raw)
}

fn canonical_ws_origin(raw: &str) -> String {
    let parsed = url::Url::parse(raw)
        .unwrap_or_else(|e| panic!("AUTH_URL is not a valid URL ({raw:?}): {e}"));
    let origin = parsed.origin();
    if !origin.is_tuple() {
        panic!("AUTH_URL has no host or has an opaque origin ({raw:?})");
    }
    origin.ascii_serialization()
}

/// Returns `Ok(())` if the request may proceed, `Err(reason)` if the upgrade
/// must be rejected with 403. The reason is a static string used for logging.
/// Missing and non-UTF-8/invalid Origin headers are collapsed into the same
/// `None` case at the caller and rejected here (fail-closed).
fn check_ws_origin(header: Option<&str>, allowed: &str) -> Result<(), &'static str> {
    match header {
        None => Err("missing or invalid Origin header"),
        Some(o) if o == allowed => Ok(()),
        Some(_) => Err("disallowed Origin"),
    }
}

/// Rejects WebSocket upgrade requests whose `Origin` header is missing,
/// invalid (non-UTF-8), or does not exactly match the configured public origin.
pub async fn require_ws_origin(
    State(state): State<Arc<WsState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    match check_ws_origin(origin, &state.allowed_ws_origin) {
        Ok(()) => Ok(next.run(req).await),
        Err(reason) => {
            tracing::warn!(origin = ?origin, reason, "Rejecting WS upgrade");
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// One-RTT upper bound for a warm-up Redis pipeline. Large enough to amortise
/// the round-trip, small enough that one reply stays well under socket buffers.
const WARM_UP_CHUNK_SIZE: usize = 500;

type WarmUpRow = (String, i32, i64, i64);

/// Sends a single pipelined `SET NX EX` batch for `chunk` and updates the
/// warmed/skipped counters. No-op on empty. Failures are warned and the
/// chunk is dropped — warm-up is best-effort.
async fn flush_chunk(
    chunk: &mut Vec<WarmUpRow>,
    redis_conn: &mut redis::aio::MultiplexedConnection,
    now: i64,
    warmed: &mut u64,
    skipped: &mut u64,
) {
    if chunk.is_empty() {
        return;
    }
    let mut pipe = redis::pipe();
    for (user_id, bpm, recorded_at, received_at) in chunk.iter() {
        let update = HeartRateReceived {
            user_id: user_id.clone(),
            bpm: *bpm,
            recorded_at: *recorded_at,
            received_at: *received_at,
        };
        let value = serialize_latest_bpm(&update);
        let key = latest_bpm_key(user_id);
        let age = (now - *recorded_at).max(0) as u64;
        let ttl = LATEST_BPM_TTL_SECS.saturating_sub(age).max(60);
        let opts = SetOptions::default()
            .conditional_set(ExistenceCheck::NX)
            .with_expiration(SetExpiry::EX(ttl));
        pipe.set_options(&key, &value, opts);
    }
    let chunk_len = chunk.len();
    let first_user_id = chunk.first().map(|r| r.0.clone());
    let last_user_id = if chunk_len > 1 {
        chunk.last().map(|r| r.0.clone())
    } else {
        None
    };
    chunk.clear();
    match pipe
        .query_async::<Vec<Option<String>>>(redis_conn)
        .await
    {
        Ok(results) => {
            for r in results {
                if r.is_some() {
                    *warmed += 1;
                } else {
                    *skipped += 1;
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                chunk_len,
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
    let mut skipped = 0u64;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            row = stream.try_next() => match row {
                Ok(Some(r)) => {
                    chunk.push(r);
                    if chunk.len() >= WARM_UP_CHUNK_SIZE {
                        flush_chunk(&mut chunk, &mut redis_conn, now, &mut warmed, &mut skipped).await;
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
    flush_chunk(&mut chunk, &mut redis_conn, now, &mut warmed, &mut skipped).await;
    tracing::info!(
        warmed,
        skipped,
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
    let mut warm_up_task = tokio::spawn({
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

    let (hr_tx, _) = tokio_broadcast::channel::<HeartRateReceived>(256);

    let auth_config = AuthConfig::default();
    tracing::info!(
        cookie_name = %auth_config.cookie_name,
        cookie_name_secure = %auth_config.cookie_name_secure,
        "Auth config loaded"
    );

    let allowed_ws_origin = load_allowed_ws_origin();
    tracing::info!(allowed_ws_origin = %allowed_ws_origin, "WS origin allowlist loaded");

    // Detached task: flips the shutdown token when SIGTERM/SIGINT arrives.
    // Dropped with the tokio runtime at end of main.
    tokio::spawn({
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
        allowed_ws_origin,
        shutdown: shutdown.clone(),
    });

    // Spawn hr.received NATS subscriber. Every .await inside the task is
    // wrapped in a biased select! against `shutdown.cancelled()` so the task
    // returns cooperatively; `.abort()` in Phase 2 is a fallback, not the
    // primary shutdown path.
    let mut hr_sub_task = {
        let hr_tx = hr_tx.clone();
        let nats = nats.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
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

                let subscribed_at = std::time::Instant::now();
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return,
                        next = hr_sub.next() => match next {
                            Some(msg) => match serde_json::from_slice::<HeartRateReceived>(&msg.payload) {
                                Ok(update) => {
                                    let _ = hr_tx.send(update);
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse hr.received event: {e}");
                                }
                            },
                            None => break,
                        }
                    }
                }

                if subscribed_at.elapsed() >= STABILITY_THRESHOLD {
                    backoff = INITIAL_BACKOFF;
                } else {
                    backoff = advance_backoff(backoff);
                }
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
        })
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
            require_ws_origin,
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

    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .into_future();
    tokio::pin!(serve);

    let mut task_failed = false;
    let mut serve_done = false;
    let mut hr_sub_done = false;

    // Phase 1: wait for the first terminal event. No timeout here — `serve`
    // resolves only after the shutdown token triggers the graceful drain.
    //
    // `biased;` makes `shutdown.cancelled()` win any tie against a
    // cooperative `hr_sub_task` exit, so a task that returns `Ok(())` on
    // shutdown is not misclassified as `task_failed`. Abnormal `serve` /
    // task completions where `shutdown` is not yet ready still fall through
    // to their respective branches.
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {
            // Normal shutdown path. The detached signal-watcher task flipped
            // the token; with_graceful_shutdown is already draining.
        }
        res = &mut hr_sub_task => {
            log_task_exit("NATS hr.received subscriber", res);
            task_failed = true;
            hr_sub_done = true;
            // Trip the token so WS handlers get a best-effort 1001 and
            // with_graceful_shutdown stops accepting new connections.
            shutdown.cancel();
        }
        res = &mut serve => {
            match res {
                Ok(()) => tracing::error!("Server returned unexpectedly before shutdown"),
                Err(e) => tracing::error!("Server error: {e}"),
            }
            task_failed = true;
            serve_done = true;
        }
    }

    // Phase 2: grace clock starts here — only after shutdown is active.
    if !serve_done {
        match tokio::time::timeout(Duration::from_secs(5), &mut serve).await {
            Ok(Ok(())) => tracing::info!("HTTP server shut down cleanly"),
            Ok(Err(e)) => {
                tracing::error!("Server error during drain: {e}");
                task_failed = true;
            }
            Err(_) => tracing::warn!("Graceful shutdown timed out after 5s"),
        }
    }

    // Guard against re-polling an already-completed JoinHandle
    // (tokio panics on re-poll after Ready).
    //
    // Shutdown-policy pin:
    //   Ok(Ok(()))  = cooperative exit; do not flip `task_failed`.
    //   Err(_)      = grace expired → fallback `abort()`. By policy,
    //                 invoking the fallback is not a task failure. Flip here
    //                 if that policy changes.
    if !hr_sub_done {
        match tokio::time::timeout(Duration::from_secs(3), &mut hr_sub_task).await {
            Ok(Ok(())) => tracing::info!("NATS hr.received subscriber exited"),
            Ok(Err(e)) if e.is_panic() => {
                tracing::error!("NATS hr.received subscriber panicked: {e}");
                task_failed = true;
            }
            Ok(Err(e)) if e.is_cancelled() => {
                tracing::debug!("NATS hr.received subscriber cancelled");
            }
            Ok(Err(e)) => {
                tracing::error!("NATS hr.received subscriber failed: {e}");
                task_failed = true;
            }
            Err(_) => {
                tracing::warn!("NATS hr.received subscriber did not exit within 3s; aborting");
                hr_sub_task.abort();
                let _ = (&mut hr_sub_task).await;
            }
        }
    }
    match tokio::time::timeout(Duration::from_secs(2), &mut warm_up_task).await {
        Ok(Ok(())) => tracing::debug!("warm-up task exited"),
        Ok(Err(e)) if e.is_panic() => {
            tracing::error!("warm-up task panicked: {e}");
            task_failed = true;
        }
        Ok(Err(_)) => {}
        Err(_) => {
            tracing::warn!("warm-up task did not exit within 2s; aborting");
            warm_up_task.abort();
            let _ = (&mut warm_up_task).await;
        }
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

    if task_failed {
        tracing::error!("ws-gateway exiting due to task failure");
        std::process::exit(1);
    }
    tracing::info!("ws-gateway shut down gracefully");
}

#[cfg(test)]
mod tests {
    use super::{canonical_ws_origin, check_ws_origin};

    #[test]
    fn rejects_missing_origin() {
        assert!(check_ws_origin(None, "http://localhost:3000").is_err());
    }

    #[test]
    fn accepts_exact_match() {
        assert_eq!(
            check_ws_origin(Some("http://localhost:3000"), "http://localhost:3000"),
            Ok(())
        );
    }

    #[test]
    fn rejects_mismatched_origin() {
        assert!(check_ws_origin(Some("http://evil.example"), "http://localhost:3000").is_err());
    }

    #[test]
    fn preserves_non_default_port() {
        assert_eq!(
            canonical_ws_origin("http://localhost:3000"),
            "http://localhost:3000"
        );
        assert_eq!(
            canonical_ws_origin("https://example.com:8443"),
            "https://example.com:8443"
        );
    }

    #[test]
    fn strips_default_http_port() {
        assert_eq!(
            canonical_ws_origin("http://example.com:80"),
            "http://example.com"
        );
    }

    #[test]
    fn strips_default_https_port() {
        assert_eq!(
            canonical_ws_origin("https://example.com:443"),
            "https://example.com"
        );
    }

    #[test]
    fn no_port_unchanged() {
        assert_eq!(
            canonical_ws_origin("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn drops_path_and_query() {
        assert_eq!(
            canonical_ws_origin("https://example.com/path?x=1"),
            "https://example.com"
        );
    }

    #[test]
    #[should_panic(expected = "has no host or has an opaque origin")]
    fn rejects_opaque_origin() {
        canonical_ws_origin("file:///etc/passwd");
    }

    #[test]
    #[should_panic(expected = "is not a valid URL")]
    fn rejects_parse_error() {
        canonical_ws_origin("not a url");
    }

    // Guardrail for the `biased;` token in the Phase 1 select under a
    // pre-cancelled shutdown. NOT a proof of tie-break behaviour: the
    // tokio scheduler does not guarantee that both futures are ready on
    // the same poll, so this test only catches the obvious regression of
    // `biased;` being removed.
    #[tokio::test]
    async fn phase1_select_prefers_shutdown_when_already_cancelled_smoke() {
        use tokio_util::sync::CancellationToken;

        #[derive(Debug, PartialEq)]
        enum Branch {
            Shutdown,
            Task,
            Serve,
        }

        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let mut task: tokio::task::JoinHandle<()> = tokio::spawn(async {});

        let branch = tokio::select! {
            biased;
            _ = shutdown.cancelled() => Branch::Shutdown,
            _ = &mut task => Branch::Task,
            _ = std::future::pending::<()>() => Branch::Serve,
        };

        assert_eq!(branch, Branch::Shutdown);
        let _ = task.await;
    }
}
