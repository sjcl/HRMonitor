use axum::Extension;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use common::messages::{ConnectionChangedEvent, subjects};

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::error::AppError;
use crate::models::{PulsoidTokenResponse, SetManualTokenRequest};

pub async fn get_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<PulsoidTokenResponse>, AppError> {
    let user_id = &auth_user.id;

    let row: Option<(String, String, i64, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT source,
                connection_state,
                EXTRACT(EPOCH FROM state_updated_at)::BIGINT as state_updated_at,
                EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
                last_error
         FROM pulsoid_connections
         WHERE user_id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?;

    let (source, connection_state, state_updated_at, last_connected_at, last_error) =
        row.ok_or_else(|| AppError::NotFound("Pulsoid token not configured".into()))?;

    Ok(Json(PulsoidTokenResponse {
        source,
        connection_state,
        state_updated_at,
        last_connected_at,
        last_error,
    }))
}

pub async fn delete_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Response, AppError> {
    let user_id = &auth_user.id;

    let result = sqlx::query("DELETE FROM pulsoid_connections WHERE user_id = $1")
        .bind(&user_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Pulsoid token not configured".into()));
    }

    // Notify pulsoid-ingest via NATS
    let event = ConnectionChangedEvent {
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
        tracing::warn!(user_id, "Failed to publish connection.changed: {e}");
        return Ok(Json(json!({"notification": "pending"})).into_response());
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn set_manual_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<SetManualTokenRequest>,
) -> Result<Response, AppError> {
    let user_id = &auth_user.id;

    let token = body.access_token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Access token cannot be empty".into()));
    }

    let (enc_access, key_version) = state.token_encryption.encrypt(token);

    sqlx::query(
        "INSERT INTO pulsoid_connections (user_id, source, access_token, key_version, refresh_token, token_expires_at, last_connected_at, last_error, refresh_blocked, connection_state, state_updated_at)
         VALUES ($1, 'manual', $2, $3, NULL, NULL, NULL, NULL, false, 'pending', now())
         ON CONFLICT (user_id) DO UPDATE SET
            source = 'manual',
            access_token = EXCLUDED.access_token,
            key_version = EXCLUDED.key_version,
            refresh_token = NULL,
            token_expires_at = NULL,
            last_connected_at = NULL,
            last_error = NULL,
            refresh_blocked = false,
            connection_state = 'pending',
            state_updated_at = now(),
            config_version = pulsoid_connections.config_version + 1",
    )
    .bind(&user_id)
    .bind(&enc_access)
    .bind(key_version as i32)
    .execute(&state.db)
    .await?;

    // Notify pulsoid-ingest via NATS
    let event = ConnectionChangedEvent {
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
        tracing::warn!(user_id, "Failed to publish connection.changed: {e}");
        return Ok(Json(json!({"notification": "pending"})).into_response());
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}
