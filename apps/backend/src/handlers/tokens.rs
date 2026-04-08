use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use std::sync::Arc;
use axum::Json;

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::error::AppError;
use crate::models::{PulsoidTokenResponse, SetManualTokenRequest};

pub async fn get_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<PulsoidTokenResponse>, AppError> {
    let user_id = &auth_user.id;

    let row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT source,
                EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
                last_error
         FROM pulsoid_connections
         WHERE user_id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?;

    let (source, last_connected_at, last_error) = row
        .ok_or_else(|| AppError::NotFound("Pulsoid token not configured".into()))?;

    Ok(Json(PulsoidTokenResponse {
        source,
        last_connected_at,
        last_error,
    }))
}

pub async fn delete_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<StatusCode, AppError> {
    let user_id = &auth_user.id;

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

pub async fn set_manual_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<SetManualTokenRequest>,
) -> Result<StatusCode, AppError> {
    let user_id = &auth_user.id;

    let token = body.access_token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Access token cannot be empty".into()));
    }

    let (enc_access, key_version) = state.token_encryption.encrypt(token);

    sqlx::query(
        "INSERT INTO pulsoid_connections (user_id, source, access_token, key_version, refresh_token, token_expires_at, last_connected_at, last_error)
         VALUES ($1, 'manual', $2, $3, NULL, NULL, NULL, NULL)
         ON CONFLICT (user_id) DO UPDATE SET
            source = 'manual',
            access_token = EXCLUDED.access_token,
            key_version = EXCLUDED.key_version,
            refresh_token = NULL,
            token_expires_at = NULL,
            last_connected_at = NULL,
            last_error = NULL",
    )
    .bind(&user_id)
    .bind(&enc_access)
    .bind(key_version as i32)
    .execute(&state.db)
    .await?;

    state.worker_manager.notify_connection_changed(&user_id).await;

    Ok(StatusCode::NO_CONTENT)
}
