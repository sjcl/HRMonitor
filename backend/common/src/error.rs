use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    Gone(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            AppError::Gone(msg) => (StatusCode::GONE, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::Database(db_err) => {
                if db_err.is_foreign_key_violation() || db_err.is_unique_violation() {
                    tracing::warn!("Database constraint violation: {e}");
                } else {
                    tracing::error!("Database error: {e}");
                }
            }
            sqlx::Error::PoolClosed => {
                tracing::warn!("Database pool closed: {e}");
            }
            sqlx::Error::RowNotFound => {
                tracing::debug!(
                    "Database row not found (should be handled by fetch_optional): {e}"
                );
            }
            _ => {
                tracing::error!("Database error: {e}");
            }
        }
        AppError::Internal("Internal server error".into())
    }
}
