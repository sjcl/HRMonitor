mod auth;
mod db;
mod error;
mod handlers;
mod models;

use common::signal::shutdown_signal;

use axum::middleware;
use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};
use std::sync::Arc;
use tokio::sync::broadcast as tokio_broadcast;
use tower_http::cors::CorsLayer;

use auth::AuthConfig;
use common::messages::{subjects, HeartRateReceived};
use common::nats_backoff::{advance_backoff, INITIAL_BACKOFF, STABILITY_THRESHOLD};
use common::pulsoid_oauth::PulsoidOAuthConfig;
use common::redis_keys::{latest_bpm_key, serialize_latest_bpm, LATEST_BPM_TTL_SECS};
use common::time::unix_now_secs;
use common::token_encryption::TokenEncryption;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::MultiplexedConnection,
    pub nats: async_nats::Client,
    pub hr_broadcast: tokio_broadcast::Sender<HeartRateReceived>,
    pub auth_config: AuthConfig,
    pub pulsoid_oauth: PulsoidOAuthConfig,
    pub token_encryption: TokenEncryption,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "api_backend=info".parse().unwrap()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor".into());

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());

    let pool = db::init_pool(&database_url)
        .await
        .expect("Failed to initialize database");

    let redis_client = redis::Client::open(redis_url).expect("Invalid REDIS_URL");
    let mut redis_conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to connect to Redis");

    tracing::info!("Connected to Redis");

    // Warm latest_bpm cache from DB: each user's latest record within 6h.
    // Redis is now the authoritative latest-state store (no DB fallback on WS
    // read), so on boot we rehydrate from the hypertable once. Individual TTLs
    // are set to the record's remaining freshness so older warmed values decay
    // out naturally, preserving the "≤ 6h old" invariant.
    //
    // The write is `SET NX EX`: pulsoid-ingest keeps running while api-backend
    // restarts, and its Redis value is strictly fresher than anything we can
    // read back from the hypertable (pulsoid-ingest commits to DB first, then
    // to Redis, and may publish more frames before this warm-up even runs).
    // An unconditional SET would clobber that live value with a stale DB row,
    // so we only populate keys that are missing (true cold start, or keys
    // whose TTL lapsed while api-backend was down).
    {
        let rows: Vec<(String, i32, i64, i64)> = sqlx::query_as(
            "SELECT DISTINCT ON (user_id) user_id, bpm, \
             EXTRACT(EPOCH FROM recorded_at)::BIGINT AS recorded_at, \
             EXTRACT(EPOCH FROM received_at)::BIGINT AS received_at \
             FROM heart_rate_records \
             WHERE recorded_at >= NOW() - INTERVAL '6 hours' \
             ORDER BY user_id, recorded_at DESC, received_at DESC, id DESC",
        )
        .fetch_all(&pool)
        .await
        .expect("Failed to warm latest_bpm cache from DB");

        let now = unix_now_secs();

        let mut warmed = 0u64;
        let mut skipped = 0u64;
        for (user_id, bpm, recorded_at, received_at) in rows {
            let update = HeartRateReceived {
                user_id: user_id.clone(),
                bpm,
                recorded_at,
                received_at,
            };
            let value = serialize_latest_bpm(&update);
            let key = latest_bpm_key(&user_id);
            let age = (now - recorded_at).max(0) as u64;
            let ttl = LATEST_BPM_TTL_SECS.saturating_sub(age).max(60);
            let opts = SetOptions::default()
                .conditional_set(ExistenceCheck::NX)
                .with_expiration(SetExpiry::EX(ttl));
            match redis_conn
                .set_options::<_, _, Option<String>>(&key, &value, opts)
                .await
            {
                Ok(None) => {
                    // Key already existed — pulsoid-ingest has a fresher
                    // value, leave it alone.
                    skipped += 1;
                }
                Ok(Some(_)) => {
                    warmed += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        user_id = %user_id,
                        error = %e,
                        "warm-up SET NX failed"
                    );
                }
            }
        }
        tracing::info!(warmed, skipped, "Warmed latest_bpm cache from DB");
    }

    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    tracing::info!("Connected to NATS at {nats_url}");

    let (hr_tx, _) = tokio_broadcast::channel::<HeartRateReceived>(256);

    let pulsoid_oauth = PulsoidOAuthConfig::from_env_full();
    let token_encryption = TokenEncryption::from_env();

    let auth_config = AuthConfig::default();
    tracing::info!(
        cookie_name = %auth_config.cookie_name,
        cookie_name_secure = %auth_config.cookie_name_secure,
        "Auth config loaded"
    );

    let state = Arc::new(AppState {
        db: pool.clone(),
        redis: redis_conn.clone(),
        nats: nats.clone(),
        hr_broadcast: hr_tx.clone(),
        auth_config,
        pulsoid_oauth,
        token_encryption,
    });

    // Spawn hr.received NATS subscriber.
    // pulsoid-ingest writes Redis directly, so api-backend only broadcasts
    // the event to connected WebSocket clients. NATS delivery is best-effort
    // — WS self-heal (every 10s) backfills any missed updates from Redis.
    //
    // Wrapped in an outer reconnect loop so the task does not silently die
    // if the Subscriber stream ends. Backoff is only reset after a
    // subscription has stayed up for STABILITY_THRESHOLD — a flapping
    // "subscribe → immediate end" cycle still backs off exponentially.
    let mut hr_sub_task = {
        let hr_tx = hr_tx.clone();
        let nats = nats.clone();
        tokio::spawn(async move {
            let mut backoff = INITIAL_BACKOFF;
            loop {
                let mut hr_sub = match nats.subscribe(subjects::HR_RECEIVED).await {
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
                        tokio::time::sleep(backoff).await;
                        backoff = advance_backoff(backoff);
                        continue;
                    }
                };

                let subscribed_at = std::time::Instant::now();
                while let Some(msg) = hr_sub.next().await {
                    match serde_json::from_slice::<HeartRateReceived>(&msg.payload) {
                        Ok(update) => {
                            let _ = hr_tx.send(update);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse hr.received event: {e}");
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
                tokio::time::sleep(backoff).await;
            }
        })
    };

    // Spawn session cleanup task (runs every hour)
    let cleanup_pool = pool;
    let mut cleanup_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            match sqlx::query("DELETE FROM sessions WHERE expires < now()")
                .execute(&cleanup_pool)
                .await
            {
                Ok(result) => {
                    if result.rows_affected() > 0 {
                        tracing::info!(
                            count = result.rows_affected(),
                            "Cleaned up expired sessions"
                        );
                    }
                }
                Err(e) => tracing::error!("Session cleanup failed: {e}"),
            }
            // Clean up expired connect requests
            match sqlx::query(
                "DELETE FROM connect_requests WHERE expires_at < now() - INTERVAL '1 hour'",
            )
            .execute(&cleanup_pool)
            .await
            {
                Ok(result) => {
                    if result.rows_affected() > 0 {
                        tracing::info!(
                            count = result.rows_affected(),
                            "Cleaned up expired connect requests"
                        );
                    }
                }
                Err(e) => tracing::error!("Connect request cleanup failed: {e}"),
            }
        }
    });

    // Public routes (no auth required)
    let public_routes = Router::new().route("/healthz", get(|| async { "ok" }));

    // Protected routes (auth required)
    let protected_routes = Router::new()
        .route(
            "/api/users/me",
            get(handlers::users::get_self_user).patch(handlers::users::update_user),
        )
        .route(
            "/api/users/{id}/heart-rate-profile",
            get(handlers::users::get_heart_rate_profile),
        )
        .route(
            "/api/users/me/pulsoid-token",
            get(handlers::tokens::get_pulsoid_token)
                .put(handlers::tokens::set_manual_pulsoid_token)
                .delete(handlers::tokens::delete_pulsoid_token),
        )
        .route(
            "/api/oauth/pulsoid/connect",
            axum::routing::post(handlers::oauth::create_connect),
        )
        .route(
            "/api/oauth/pulsoid/connect/{request_id}",
            get(handlers::oauth::redirect_to_pulsoid),
        )
        .route(
            "/api/users/{id}/heart-rates/minute-stats",
            get(handlers::heart_rates::minute_stats),
        )
        .route(
            "/api/users/{id}/heart-rates/minute-stats/by-date",
            get(handlers::heart_rates::minute_stats_by_date),
        )
        .route(
            "/api/users/{id}/heart-rates/daily-stats",
            get(handlers::heart_rates::daily_stats),
        )
        .route(
            "/api/users/{id}/heart-rates/by-date",
            get(handlers::heart_rates::heart_rates_by_date),
        )
        .route(
            "/api/users/{id}/heart-rates",
            get(handlers::heart_rates::list_heart_rates),
        )
        .route(
            "/api/groups",
            get(handlers::groups::list_groups).post(handlers::groups::create_group),
        )
        .route(
            "/api/groups/{id}",
            get(handlers::groups::get_group)
                .patch(handlers::groups::update_group)
                .delete(handlers::groups::delete_group),
        )
        .route(
            "/api/groups/{id}/heart-rates",
            get(handlers::heart_rates::group_heart_rates),
        )
        .route(
            "/api/groups/{id}/heart-rates/minute-stats",
            get(handlers::heart_rates::group_minute_stats),
        )
        .route(
            "/api/groups/{id}/members/me",
            axum::routing::patch(handlers::groups::update_my_membership)
                .delete(handlers::groups::leave_group),
        )
        .route(
            "/api/groups/{id}/invites",
            get(handlers::groups::list_invites).post(handlers::groups::create_invite),
        )
        .route(
            "/api/groups/{id}/invites/{invite_id}",
            axum::routing::delete(handlers::groups::revoke_invite),
        )
        .route(
            "/api/invites/{token}",
            get(handlers::groups::get_invite_info),
        )
        .route(
            "/api/invites/{token}/accept",
            axum::routing::post(handlers::groups::accept_invite),
        )
        .route("/api/ws/me", get(handlers::ws::my_heart_rate_ws))
        .route("/api/ws/users/{id}", get(handlers::ws::user_heart_rate_ws))
        .route(
            "/api/ws/groups/{id}",
            get(handlers::ws::group_heart_rate_ws),
        )
        .route(
            "/api/oauth/pulsoid/callback",
            get(handlers::oauth::callback),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001")
        .await
        .expect("Failed to bind to port 3001");

    tracing::info!("Server listening on 0.0.0.0:3001");

    // Wait for shutdown signal, server exit, or unexpected background task exit.
    let server = axum::serve(listener, app);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    let mut task_failed = false;

    tokio::select! {
        res = server => {
            // axum::serve returns only on error or if all listeners close.
            // Without with_graceful_shutdown(), Ok(()) is also unexpected.
            match res {
                Ok(()) => tracing::error!("Server returned unexpectedly"),
                Err(e) => tracing::error!("Server error: {e}"),
            }
            task_failed = true;
            hr_sub_task.abort();
            cleanup_task.abort();
            log_task_exit("NATS hr.received subscriber (sibling)", hr_sub_task.await);
            log_task_exit("Session cleanup (sibling)", cleanup_task.await);
        }
        res = &mut hr_sub_task => {
            log_task_exit("NATS hr.received subscriber", res);
            task_failed = true;
            // server future is cancelled (dropped) when select! exits
            tracing::info!("HTTP server will be cancelled as select! exits");
            cleanup_task.abort();
            log_task_exit("Session cleanup (sibling)", cleanup_task.await);
        }
        res = &mut cleanup_task => {
            log_task_exit("Session cleanup", res);
            task_failed = true;
            // server future is cancelled (dropped) when select! exits
            tracing::info!("HTTP server will be cancelled as select! exits");
            hr_sub_task.abort();
            log_task_exit("NATS hr.received subscriber (sibling)", hr_sub_task.await);
        }
        _ = &mut shutdown => {
            tracing::info!("Received shutdown signal");
            // server future is not spawned — it is cancelled (dropped) when
            // select! picks this branch, stopping the HTTP listener immediately.
            // Unbounded graceful drain is avoided because long-lived WebSocket
            // connections could block shutdown indefinitely. If bounded graceful
            // drain is needed later, use with_graceful_shutdown() + a timeout.
            hr_sub_task.abort();
            cleanup_task.abort();
            let _ = hr_sub_task.await;
            let _ = cleanup_task.await;
        }
    }

    nats.flush().await.ok();

    if task_failed {
        tracing::error!("api-backend exiting due to task failure");
        std::process::exit(1);
    }
    tracing::info!("api-backend shut down gracefully");
}

fn log_task_exit(name: &str, result: Result<(), tokio::task::JoinError>) {
    match result {
        Ok(()) => tracing::error!("{name} returned unexpectedly"),
        Err(e) if e.is_panic() => tracing::error!("{name} panicked: {e}"),
        Err(e) if e.is_cancelled() => tracing::debug!("{name} cancelled"),
        Err(e) => tracing::error!("{name} failed: {e}"),
    }
}


