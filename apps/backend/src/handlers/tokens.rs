use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use std::sync::Arc;
use axum::Json;

use crate::AppState;
use crate::auth::{AuthenticatedUser, ensure_self};
use crate::error::AppError;
use crate::models::PulsoidTokenResponse;

pub async fn get_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<PulsoidTokenResponse>, AppError> {
    ensure_self(&auth_user, &user_id)?;

    let row: Option<(Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
                last_error
         FROM pulsoid_connections
         WHERE user_id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?;

    let (last_connected_at, last_error) = row
        .ok_or_else(|| AppError::NotFound("Pulsoid token not configured".into()))?;

    Ok(Json(PulsoidTokenResponse {
        last_connected_at,
        last_error,
    }))
}

pub async fn delete_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<StatusCode, AppError> {
    ensure_self(&auth_user, &user_id)?;

    let result = sqlx::query("DELETE FROM pulsoid_connections WHERE user_id = $1")
        .bind(&user_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Pulsoid token not configured".into()));
    }

    // Notify worker manager to stop the worker
    state.worker_manager.notify_connection_changed(&user_id).await;

    Ok(StatusCode::NO_CONTENT)
}
