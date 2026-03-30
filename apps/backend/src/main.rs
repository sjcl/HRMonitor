mod broadcast;
mod db;
mod error;
mod handlers;
mod models;
mod worker;
mod worker_manager;

use axum::Router;
use axum::routing::get;
use std::sync::Arc;
use tokio::sync::broadcast as tokio_broadcast;
use tower_http::cors::CorsLayer;

use broadcast::LatestHeartRateUpdate;
use worker_manager::WorkerManager;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: tokio::sync::Mutex<redis::aio::MultiplexedConnection>,
    pub worker_manager: Arc<WorkerManager>,
    pub hr_broadcast: tokio_broadcast::Sender<LatestHeartRateUpdate>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hrmonitor_backend=info".parse().unwrap()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor".into());

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());

    let pool = db::init_pool(&database_url)
        .await
        .expect("Failed to initialize database");

    let redis_client = redis::Client::open(redis_url).expect("Invalid REDIS_URL");
    let redis_conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to connect to Redis");

    tracing::info!("Connected to Redis");

    let (hr_tx, _) = tokio_broadcast::channel::<LatestHeartRateUpdate>(256);

    let worker_manager = WorkerManager::new(pool.clone(), redis_conn.clone(), hr_tx.clone());
    worker_manager.start_all_active().await;

    let state = Arc::new(AppState {
        db: pool,
        redis: tokio::sync::Mutex::new(redis_conn),
        worker_manager,
        hr_broadcast: hr_tx,
    });

    let app = Router::new()
        .route(
            "/api/users",
            get(handlers::users::list_users).post(handlers::users::create_user),
        )
        .route(
            "/api/users/{id}",
            get(handlers::users::get_user).patch(handlers::users::update_user),
        )
        .route(
            "/api/users/{id}/pulsoid-token",
            get(handlers::tokens::get_pulsoid_token)
                .put(handlers::tokens::set_pulsoid_token)
                .delete(handlers::tokens::delete_pulsoid_token),
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
        .route("/api/ws/heart-rates", get(handlers::ws::heart_rate_ws))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001")
        .await
        .expect("Failed to bind to port 3001");

    tracing::info!("Server listening on 0.0.0.0:3001");
    axum::serve(listener, app).await.expect("Server error");
}
