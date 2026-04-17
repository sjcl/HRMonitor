mod ws;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};
use std::sync::Arc;
use tokio::sync::broadcast as tokio_broadcast;

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

/// Rejects WebSocket upgrade requests whose `Origin` header is not the
/// configured public origin.
pub async fn require_ws_origin(
    State(state): State<Arc<WsState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    match origin {
        None => Ok(next.run(req).await),
        Some(o) if o == state.allowed_ws_origin => Ok(next.run(req).await),
        Some(o) => {
            tracing::warn!(origin = %o, "Rejecting WS upgrade from disallowed origin");
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Populates `latest_bpm:{user_id}` from the last 6h of `heart_rate_records`
/// using `SET NX EX`, so pulsoid-ingest's authoritative plain `SET` is never
/// overwritten. Failures are logged but not fatal — the WS self_heal path
/// will recover once writes resume.
async fn warm_latest_bpm_cache(
    pool: sqlx::PgPool,
    mut redis_conn: redis::aio::MultiplexedConnection,
) {
    let rows: Vec<(String, i32, i64, i64)> = match sqlx::query_as(
        "SELECT DISTINCT ON (user_id) user_id, bpm, \
         EXTRACT(EPOCH FROM recorded_at)::BIGINT AS recorded_at, \
         EXTRACT(EPOCH FROM received_at)::BIGINT AS received_at \
         FROM heart_rate_records \
         WHERE recorded_at >= NOW() - INTERVAL '6 hours' \
         ORDER BY user_id, recorded_at DESC, received_at DESC, id DESC",
    )
    .fetch_all(&pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "warm-up DB scan failed; skipping latest_bpm warm-up");
            return;
        }
    };

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

    // Warm latest_bpm cache in the background so axum::serve can start
    // accepting WS upgrades immediately. `SET NX EX` preserves any fresher
    // value pulsoid-ingest has already written, so warm-up ordering against
    // live writes is a non-issue. Clients that connect mid-warm-up may see
    // a null initial snapshot for inactive users; the WS handler's 10s
    // self_heal_interval converts that to an Update once warm-up lands.
    let warm_up_task = tokio::spawn(warm_latest_bpm_cache(pool.clone(), redis_conn.clone()));

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

    let state = Arc::new(WsState {
        db: pool.clone(),
        redis: redis_conn.clone(),
        hr_broadcast: hr_tx.clone(),
        auth_config,
        allowed_ws_origin,
    });

    // Spawn hr.received NATS subscriber.
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

    let app = Router::new().merge(ws_routes).with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3002")
        .await
        .expect("Failed to bind to port 3002");

    tracing::info!("Server listening on 0.0.0.0:3002");

    let server = axum::serve(listener, app);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    let mut task_failed = false;

    tokio::select! {
        res = server => {
            match res {
                Ok(()) => tracing::error!("Server returned unexpectedly"),
                Err(e) => tracing::error!("Server error: {e}"),
            }
            task_failed = true;
            hr_sub_task.abort();
            log_task_exit("NATS hr.received subscriber (sibling)", hr_sub_task.await);
        }
        res = &mut hr_sub_task => {
            log_task_exit("NATS hr.received subscriber", res);
            task_failed = true;
            tracing::info!("HTTP server will be cancelled as select! exits");
        }
        _ = &mut shutdown => {
            tracing::info!("Received shutdown signal");
            hr_sub_task.abort();
            let _ = hr_sub_task.await;
        }
    }

    warm_up_task.abort();
    let _ = warm_up_task.await;

    nats.flush().await.ok();

    if task_failed {
        tracing::error!("ws-gateway exiting due to task failure");
        std::process::exit(1);
    }
    tracing::info!("ws-gateway shut down gracefully");
}

#[cfg(test)]
mod tests {
    use super::canonical_ws_origin;

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
}
