mod db;
mod handlers;
mod models;
mod validation;

use common::signal::{log_task_exit, shutdown_signal};

use axum::middleware;
use axum::routing::get;
use axum::Router;
use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use common::auth::{AuthConfig, AuthContext};
use common::pulsoid_oauth::PulsoidOAuthConfig;
use common::token_encryption::TokenEncryption;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub nats: async_nats::Client,
    pub auth_config: AuthConfig,
    pub pulsoid_oauth: PulsoidOAuthConfig,
    pub token_encryption: TokenEncryption,
}

impl AuthContext for AppState {
    fn db(&self) -> &sqlx::PgPool {
        &self.db
    }
    fn auth_config(&self) -> &AuthConfig {
        &self.auth_config
    }
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

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());

    let pool = db::init_pool(&database_url)
        .await
        .expect("Failed to initialize database");

    let nats = async_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    tracing::info!("Connected to NATS at {nats_url}");

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
        nats: nats.clone(),
        auth_config,
        pulsoid_oauth,
        token_encryption,
    });

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
        .route(
            "/api/oauth/pulsoid/callback",
            get(handlers::oauth::callback),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            common::auth::require_auth::<AppState>,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001")
        .await
        .expect("Failed to bind to port 3001");

    tracing::info!("Server listening on 0.0.0.0:3001");

    let shutdown = CancellationToken::new();

    // Detached task: flips the shutdown token when SIGTERM/SIGINT arrives.
    tokio::spawn({
        let token = shutdown.clone();
        async move {
            shutdown_signal().await;
            tracing::info!("Received shutdown signal");
            token.cancel();
        }
    });

    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .into_future();
    tokio::pin!(serve);

    let mut task_failed = false;
    let mut serve_done = false;
    let mut cleanup_done = false;

    // Phase 1: wait for the first terminal event. `biased` ensures shutdown
    // signal takes priority so graceful drain begins immediately.
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => {
            // Normal shutdown — `serve` is already draining via with_graceful_shutdown.
        }
        res = &mut cleanup_task => {
            log_task_exit("Session cleanup", res);
            task_failed = true;
            cleanup_done = true;
            shutdown.cancel();
        }
        res = &mut serve => {
            match res {
                Ok(()) => tracing::error!("Server returned unexpectedly before shutdown"),
                Err(e) => tracing::error!("Server error: {e}"),
            }
            task_failed = true;
            serve_done = true;
            shutdown.cancel();
        }
    }

    // Phase 2: wait for remaining tasks with timeout.
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

    if !cleanup_done {
        match tokio::time::timeout(Duration::from_secs(1), &mut cleanup_task).await {
            Ok(res) => {
                log_task_exit("Session cleanup", res);
            }
            Err(_) => {
                tracing::warn!("Session cleanup did not exit within 1s; aborting");
                cleanup_task.abort();
                let _ = (&mut cleanup_task).await;
            }
        }
    }

    match tokio::time::timeout(Duration::from_secs(1), nats.flush()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("NATS flush on shutdown failed: {e}"),
        Err(_) => tracing::warn!("NATS flush timed out after 1s on shutdown"),
    }

    if task_failed {
        tracing::error!("api-backend exiting due to task failure");
        std::process::exit(1);
    }
    tracing::info!("api-backend shut down gracefully");
}

