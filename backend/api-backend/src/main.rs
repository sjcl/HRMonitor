mod auth;
mod db;
mod error;
mod handlers;
mod models;
mod pulsoid_oauth;

use axum::Router;
use axum::middleware;
use axum::routing::get;
use futures_util::StreamExt;
use redis::AsyncCommands;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::broadcast as tokio_broadcast;
use tower_http::cors::CorsLayer;

use auth::AuthConfig;
use common::messages::{HeartRateReceived, TokenRefreshRequest, subjects};
use common::token_encryption::TokenEncryption;
use pulsoid_oauth::PulsoidOAuthConfig;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: tokio::sync::Mutex<redis::aio::MultiplexedConnection>,
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

    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());

    let pool = db::init_pool(&database_url)
        .await
        .expect("Failed to initialize database");

    let redis_client = redis::Client::open(redis_url).expect("Invalid REDIS_URL");
    let redis_conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to connect to Redis");

    tracing::info!("Connected to Redis");

    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    tracing::info!("Connected to NATS at {nats_url}");

    let (hr_tx, _) = tokio_broadcast::channel::<HeartRateReceived>(256);

    let pulsoid_oauth = PulsoidOAuthConfig::from_env();
    let token_encryption = TokenEncryption::from_env();

    let auth_config = AuthConfig::default();
    tracing::info!(
        cookie_name = %auth_config.cookie_name,
        cookie_name_secure = %auth_config.cookie_name_secure,
        "Auth config loaded"
    );

    let state = Arc::new(AppState {
        db: pool.clone(),
        redis: tokio::sync::Mutex::new(redis_conn.clone()),
        nats: nats.clone(),
        hr_broadcast: hr_tx.clone(),
        auth_config,
        pulsoid_oauth,
        token_encryption,
    });

    // Spawn hr.received NATS subscriber
    {
        let mut redis_conn = redis_conn.clone();
        let hr_tx = hr_tx.clone();
        let mut hr_sub = nats
            .subscribe(subjects::HR_RECEIVED)
            .await
            .expect("Failed to subscribe to hr.received");

        tokio::spawn(async move {
            while let Some(msg) = hr_sub.next().await {
                match serde_json::from_slice::<HeartRateReceived>(&msg.payload) {
                    Ok(update) => {
                        // Write to Redis cache
                        let redis_value = serde_json::to_string(&update).unwrap();
                        let key = format!("latest_bpm:{}", update.user_id);
                        if let Err(e) = redis_conn.set::<_, _, ()>(&key, &redis_value).await {
                            tracing::warn!(
                                user_id = %update.user_id,
                                "Failed to write to Redis: {e}"
                            );
                        }
                        // Broadcast to WebSocket subscribers
                        let _ = hr_tx.send(update);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse hr.received event: {e}");
                    }
                }
            }
        });
    }

    // Spawn pulsoid.token.refresh_needed NATS subscriber
    {
        let state = state.clone();
        let nats = nats.clone();
        let mut refresh_sub = nats
            .subscribe(subjects::TOKEN_REFRESH_NEEDED)
            .await
            .expect("Failed to subscribe to token.refresh_needed");

        tokio::spawn(async move {
            let in_flight: Arc<std::sync::Mutex<HashSet<String>>> =
                Arc::new(std::sync::Mutex::new(HashSet::new()));

            while let Some(msg) = refresh_sub.next().await {
                let req = match serde_json::from_slice::<TokenRefreshRequest>(&msg.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to parse refresh_needed event: {e}");
                        continue;
                    }
                };

                // Skip if a refresh is already in-flight for this user
                {
                    let mut set = in_flight.lock().unwrap();
                    if !set.insert(req.user_id.clone()) {
                        tracing::debug!(user_id = %req.user_id, "Refresh already in-flight, skipping");
                        continue;
                    }
                }

                tracing::info!(user_id = %req.user_id, "Received token refresh request");
                let state = state.clone();
                let in_flight = in_flight.clone();
                let user_id = req.user_id.clone();
                tokio::spawn(async move {
                    let _guard = InFlightGuard {
                        user_id: user_id.clone(),
                        set: in_flight,
                    };
                    handle_token_refresh(&state, &user_id).await;
                });
            }
        });
    }

    // Spawn session cleanup task (runs every hour)
    let cleanup_pool = pool;
    tokio::spawn(async move {
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
            "/api/users/{id}/latest-heart-rate",
            get(handlers::heart_rates::latest_heart_rate),
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
    axum::serve(listener, app).await.expect("Server error");
}

struct InFlightGuard {
    user_id: String,
    set: Arc<std::sync::Mutex<HashSet<String>>>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.user_id);
    }
}

/// Handle a token refresh request from pulsoid-ingest.
/// Fetches the connection from DB, refreshes the OAuth token, saves the new tokens,
/// and publishes a connection.changed event.
async fn handle_token_refresh(state: &AppState, user_id: &str) {
    // Fetch connection details
    let row: Option<(String, Vec<u8>, Option<Vec<u8>>, i32, Option<i64>)> = match sqlx::query_as(
        "SELECT source, access_token, refresh_token, key_version,
                EXTRACT(EPOCH FROM token_expires_at)::BIGINT as token_expires_at
         FROM pulsoid_connections WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(user_id, "Failed to fetch connection for refresh: {e}");
            return;
        }
    };

    let (source, _access_token, refresh_token_enc, key_version, token_expires_at) = match row {
        Some(r) => r,
        None => {
            tracing::warn!(user_id, "No pulsoid connection found for refresh");
            return;
        }
    };

    // Only process OAuth connections
    if source != "oauth" {
        tracing::debug!(user_id, "Ignoring refresh request for non-OAuth connection");
        return;
    }

    // Check if token is still expired (might have been refreshed already)
    if let Some(expires_at) = token_expires_at {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        if now < expires_at - 60 {
            tracing::debug!(user_id, "Token still valid, skipping refresh");
            return;
        }
    }

    // Decrypt refresh token
    let refresh_token_bytes = match refresh_token_enc {
        Some(rt) => rt,
        None => {
            tracing::error!(user_id, "OAuth connection has no refresh_token");
            let _ = sqlx::query(
                "UPDATE pulsoid_connections SET last_error = $1 WHERE user_id = $2",
            )
            .bind("No refresh token available")
            .bind(user_id)
            .execute(&state.db)
            .await;
            return;
        }
    };

    let refresh_token_plain = match state
        .token_encryption
        .decrypt(&refresh_token_bytes, key_version as u32)
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(user_id, "Failed to decrypt refresh token: {e}");
            let _ = sqlx::query(
                "UPDATE pulsoid_connections SET last_error = $1 WHERE user_id = $2",
            )
            .bind(format!("Failed to decrypt refresh token: {e}"))
            .bind(user_id)
            .execute(&state.db)
            .await;
            return;
        }
    };

    // Call Pulsoid OAuth refresh
    let token_resp = match state.pulsoid_oauth.refresh_token(&refresh_token_plain).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(user_id, "Token refresh failed: {e}");
            let _ = sqlx::query(
                "UPDATE pulsoid_connections SET last_error = $1 WHERE user_id = $2",
            )
            .bind(format!("Token refresh failed: {e}"))
            .bind(user_id)
            .execute(&state.db)
            .await;
            return;
        }
    };

    // Encrypt new tokens
    let (enc_access, new_key_version) = state.token_encryption.encrypt(&token_resp.access_token);
    let enc_refresh: Option<Vec<u8>> = if let Some(ref new_rt) = token_resp.refresh_token {
        Some(state.token_encryption.encrypt(new_rt).0)
    } else {
        Some(refresh_token_bytes) // Keep old refresh token
    };

    // Save to DB with config_version increment and last_error = NULL
    let result = sqlx::query(
        "UPDATE pulsoid_connections
         SET access_token = $1, refresh_token = $2, key_version = $3,
             token_expires_at = now() + make_interval(secs => $4),
             last_error = NULL, config_version = config_version + 1
         WHERE user_id = $5 AND source = 'oauth'",
    )
    .bind(&enc_access)
    .bind(&enc_refresh)
    .bind(new_key_version as i32)
    .bind(token_resp.expires_in as f64)
    .bind(user_id)
    .execute(&state.db)
    .await;

    if let Err(e) = result {
        tracing::error!(user_id, "Failed to save refreshed tokens: {e}");
        return;
    }

    tracing::info!(user_id, "Token refreshed successfully");

    // Notify pulsoid-ingest to reconnect with new token
    let event = common::messages::ConnectionChangedEvent {
        user_id: user_id.to_string(),
    };
    if let Err(e) = state
        .nats
        .publish(
            subjects::CONNECTION_CHANGED,
            serde_json::to_vec(&event).unwrap().into(),
        )
        .await
    {
        tracing::warn!(user_id, "Failed to publish connection.changed after refresh: {e}");
    }
}
