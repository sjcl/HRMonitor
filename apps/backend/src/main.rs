mod db;
mod error;
mod handlers;
mod models;
mod worker;
mod worker_manager;

use axum::routing::{get, patch};
use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use worker_manager::WorkerManager;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub worker_manager: Arc<WorkerManager>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hrmonitor_backend=info".parse().unwrap()),
        )
        .init();

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:///data/app.db".into());

    let pool = db::init_pool(&database_url).await.expect("Failed to initialize database");

    let worker_manager = WorkerManager::new(pool.clone());
    worker_manager.start_all_active().await;

    let state = Arc::new(AppState {
        db: pool,
        worker_manager,
    });

    let app = Router::new()
        .route("/api/users", get(handlers::users::list_users).post(handlers::users::create_user))
        .route("/api/users/{id}", patch(handlers::users::update_user))
        .route(
            "/api/users/{id}/pulsoid-tokens",
            get(handlers::tokens::list_tokens).post(handlers::tokens::create_token),
        )
        .route(
            "/api/pulsoid-tokens/{id}",
            patch(handlers::tokens::update_token).delete(handlers::tokens::delete_token),
        )
        .route("/api/users/{id}/heart-rates", get(handlers::heart_rates::list_heart_rates))
        .route("/api/users/{id}/latest-heart-rate", get(handlers::heart_rates::latest_heart_rate))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001")
        .await
        .expect("Failed to bind to port 3001");

    tracing::info!("Server listening on 0.0.0.0:3001");
    axum::serve(listener, app).await.expect("Server error");
}
