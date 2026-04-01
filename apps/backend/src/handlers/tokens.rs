use axum::Json;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use std::sync::Arc;

use crate::AppState;
use crate::auth::{AuthenticatedUser, ensure_self};
use crate::error::AppError;
use crate::models::{PulsoidTokenResponse, SetPulsoidTokenRequest, UserRow};

const SELECT_USER_ROW: &str = "SELECT id, display_name, timezone, NULL::TEXT as avatar_url, pulsoid_access_token,
            EXTRACT(EPOCH FROM pulsoid_last_connected_at)::BIGINT as pulsoid_last_connected_at,
            pulsoid_last_error,
            EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
            EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
     FROM users";

pub async fn get_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<PulsoidTokenResponse>, AppError> {
    ensure_self(&auth_user, &user_id)?;

    let query = format!("{SELECT_USER_ROW} WHERE id = $1");
    let user: UserRow = sqlx::query_as(&query)
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    if user.pulsoid_access_token.is_none() {
        return Err(AppError::NotFound("Pulsoid token not configured".into()));
    }

    Ok(Json(PulsoidTokenResponse {
        last_connected_at: user.pulsoid_last_connected_at,
        last_error: user.pulsoid_last_error,
    }))
}

pub async fn set_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<SetPulsoidTokenRequest>,
) -> Result<Json<PulsoidTokenResponse>, AppError> {
    ensure_self(&auth_user, &user_id)?;

    if body.access_token.trim().is_empty() {
        return Err(AppError::BadRequest("Access token cannot be empty".into()));
    }

    // Stop existing worker before DB update to prevent race
    state.worker_manager.stop(&user_id).await;

    let now = now_unix();
    let result = sqlx::query(
        "UPDATE users SET pulsoid_access_token = $1, pulsoid_last_connected_at = NULL, pulsoid_last_error = NULL, updated_at = to_timestamp($2) WHERE id = $3"
    )
    .bind(&body.access_token)
    .bind(now)
    .bind(&user_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    // Re-fetch user row for worker
    let query = format!("{SELECT_USER_ROW} WHERE id = $1");
    let user: UserRow = sqlx::query_as(&query)
        .bind(&user_id)
        .fetch_one(&state.db)
        .await?;

    // Start new worker
    state.worker_manager.start(user).await;

    Ok(Json(PulsoidTokenResponse {
        last_connected_at: None,
        last_error: None,
    }))
}

pub async fn delete_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<StatusCode, AppError> {
    ensure_self(&auth_user, &user_id)?;

    // Stop worker before DB update
    state.worker_manager.stop(&user_id).await;

    let now = now_unix();
    let result = sqlx::query(
        "UPDATE users SET pulsoid_access_token = NULL, pulsoid_last_connected_at = NULL, pulsoid_last_error = NULL, updated_at = to_timestamp($1) WHERE id = $2"
    )
    .bind(now)
    .bind(&user_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
