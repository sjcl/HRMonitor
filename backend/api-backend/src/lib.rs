//! HRMonitor HTTP API.
//!
//! Exposed as a library as well as a binary so that integration tests can drive
//! the session-rotation logic against a real Redis without duplicating it.

pub mod cookies;
pub mod db;
pub mod handlers;
pub mod keygen;
pub mod models;
pub mod sessions;
pub mod validation;

use common::signal::{shutdown_signal, spawn_critical_task};

use axum::Router;
use axum::middleware;
use axum::routing::get;
use std::sync::Arc;
use std::time::Duration;

use common::auth::{AuthConfig, AuthContext};
use common::discord_oauth::DiscordOAuthConfig;
use common::jwt::{JwtSigner, JwtVerifier};
use common::origin::OriginContext;
use common::pulsoid_oauth::PulsoidOAuthConfig;
use common::token_encryption::TokenEncryption;

use crate::sessions::SessionKeys;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::MultiplexedConnection,
    pub nats: async_nats::Client,
    pub auth_config: AuthConfig,
    pub jwt_verifier: JwtVerifier,
    pub jwt_signer: JwtSigner,
    pub session_keys: SessionKeys,
    pub discord_oauth: DiscordOAuthConfig,
    pub pulsoid_oauth: PulsoidOAuthConfig,
    pub token_encryption: TokenEncryption,
    /// Canonical public origin. Backs both the same-origin check and
    /// `return_to` validation, so the two can never disagree.
    pub public_origin: url::Url,
    pub allowed_origin: String,
}

impl AuthContext for AppState {
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

impl OriginContext for AppState {
    fn allowed_origin(&self) -> &str {
        &self.allowed_origin
    }
}

pub async fn run() {
    // `gen-keys` / `gen-jwt-key` run before any logging or I/O setup so the
    // output is paste-ready.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("gen-keys") => return keygen::gen_keys(),
        Some("gen-jwt-key") => {
            let kid = args
                .iter()
                .position(|a| a == "--kid")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| {
                    eprintln!("usage: api-backend gen-jwt-key --kid <kid>");
                    std::process::exit(2);
                });
            return keygen::gen_jwt_key(&kid);
        }
        _ => {}
    }

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

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let redis = redis::Client::open(redis_url.as_str())
        .expect("Invalid REDIS_URL")
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to connect to Redis");
    tracing::info!("Connected to Redis at {redis_url}");

    let pulsoid_oauth = PulsoidOAuthConfig::from_env_full();
    let discord_oauth = DiscordOAuthConfig::from_env();
    let token_encryption = TokenEncryption::from_env();
    let session_keys = SessionKeys::from_env();

    let allowed_origin = common::origin::load_allowed_origin();
    let public_origin = url::Url::parse(&allowed_origin).expect("canonical origin must parse");
    let auth_config = AuthConfig::from_env(&public_origin);

    let jwt_verifier = JwtVerifier::from_env();
    // Cross-checks the private key against the published JWK for the active
    // kid, so a half-finished key rotation fails here rather than as a wave of
    // 401s that nothing can explain.
    let jwt_signer = JwtSigner::from_env(&jwt_verifier);

    tracing::info!(
        origin = %allowed_origin,
        access_cookie = %auth_config.access_cookie_name,
        secure = auth_config.cookie_secure,
        "Auth config loaded"
    );

    let state = Arc::new(AppState {
        db: pool.clone(),
        redis,
        nats: nats.clone(),
        auth_config,
        jwt_verifier,
        jwt_signer,
        session_keys,
        discord_oauth,
        pulsoid_oauth,
        token_encryption,
        public_origin,
        allowed_origin,
    });

    // Periodic cleanup. Sessions are no longer stored in Postgres — Redis
    // expires them itself — so only OAuth connect tickets need sweeping.
    let cleanup_pool = pool;
    let _cleanup_task = spawn_critical_task("Connect request cleanup task", None, async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
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
    let public_routes = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // GET, so exempt from the Origin check by design: Discord redirects the
        // browser here as a top-level navigation with no Origin header. CSRF is
        // covered by the single-use state bound to the nonce cookie, plus PKCE.
        .route(
            "/api/auth/login/discord",
            get(handlers::auth::login_discord),
        )
        .route(
            "/api/auth/callback/discord",
            get(handlers::auth::callback_discord),
        )
        // Local token check only — no DB, no Redis. Used by the SPA before
        // opening a WebSocket, where a failed upgrade is otherwise opaque.
        .route("/api/auth/session", get(handlers::auth::session));

    // Credential actions. Origin-checked (POST), but deliberately *not* behind
    // `require_auth`: refresh exists precisely for when the access token has
    // expired, and logout must still revoke a session for an idle tab.
    let auth_action_routes = Router::new()
        .route(
            "/api/auth/refresh",
            axum::routing::post(handlers::auth::refresh),
        )
        .route(
            "/api/auth/logout",
            axum::routing::post(handlers::auth::logout),
        );

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
        .merge(auth_action_routes)
        .merge(protected_routes)
        // Added last, so it is the outermost layer and runs first: a
        // cross-origin POST is refused before any authentication or database
        // work happens.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            common::origin::require_origin_unsafe::<AppState>,
        ))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001")
        .await
        .expect("Failed to bind to port 3001");

    tracing::info!("Server listening on 0.0.0.0:3001");

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            tracing::info!("Received shutdown signal");
        })
        .await;

    if let Err(e) = serve_result {
        tracing::error!("Server error: {e}");
        std::process::exit(1);
    }

    match tokio::time::timeout(Duration::from_secs(1), nats.flush()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("NATS flush on shutdown failed: {e}"),
        Err(_) => tracing::warn!("NATS flush timed out after 1s on shutdown"),
    }

    tracing::info!("api-backend shut down gracefully");
}
