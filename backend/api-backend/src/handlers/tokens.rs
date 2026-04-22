use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Extension;
use axum::Json;
use serde_json::json;
use std::sync::Arc;

use common::error::AppError;
use common::messages::{subjects, ConnectionChangeCommand};
use common::pulsoid_state::ConnectionState;

use common::auth::AuthenticatedUser;

use crate::AppState;
use crate::models::{PulsoidTokenResponse, SetManualTokenRequest};

type PulsoidConnectionRow = (String, ConnectionState, i64, Option<i64>, Option<String>);

pub async fn get_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<PulsoidTokenResponse>, AppError> {
    let user_id = &auth_user.id;

    let row: Option<PulsoidConnectionRow> = sqlx::query_as(
        "SELECT source,
                connection_state,
                EXTRACT(EPOCH FROM state_updated_at)::BIGINT as state_updated_at,
                EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
                last_error
         FROM pulsoid_connections
         WHERE user_id = $1",
    )
    .bind(user_id)
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

    let deleted: Option<(i32,)> = sqlx::query_as(
        "DELETE FROM pulsoid_connections WHERE user_id = $1 RETURNING revision",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;

    let revision = match deleted {
        Some((rev,)) => rev,
        None => return Err(AppError::NotFound("Pulsoid token not configured".into())),
    };

    tracing::info!(user_id, revision, "Pulsoid connection deleted");
    let payload = ConnectionChangeCommand::payload_for(user_id).into();
    if let Err(e) = state.nats.publish(subjects::CONNECTION_CHANGED, payload).await {
        tracing::warn!(user_id, revision, "Failed to publish connection change hint: {e}");
    }

    Ok(Json(json!({"status": "syncing"})).into_response())
}

pub async fn set_manual_pulsoid_token(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<SetManualTokenRequest>,
) -> Result<Response, AppError> {
    let user_id = &auth_user.id;

    let token = crate::validation::validate_secret(&body.access_token, 4096, "access_token")?;

    let (enc_access, key_version) = state.token_encryption.encrypt(token);

    let (revision,): (i32,) = sqlx::query_as(
        "INSERT INTO pulsoid_connections (user_id, source, access_token, key_version, refresh_token, token_expires_at, last_connected_at, last_error, connection_state, state_updated_at)
         VALUES ($1, 'manual', $2, $3, NULL, NULL, NULL, NULL, 'pending', now())
         ON CONFLICT (user_id) DO UPDATE SET
            source = 'manual',
            access_token = EXCLUDED.access_token,
            key_version = EXCLUDED.key_version,
            refresh_token = NULL,
            token_expires_at = NULL,
            last_connected_at = NULL,
            last_error = NULL,
            connection_state = 'pending',
            state_updated_at = now(),
            revision = nextval('pulsoid_revision_seq')
         RETURNING revision",
    )
    .bind(user_id)
    .bind(&enc_access)
    .bind(key_version as i32)
    .fetch_one(&state.db)
    .await?;

    tracing::info!(user_id, revision, "Manual Pulsoid token saved");
    let payload = ConnectionChangeCommand::payload_for(user_id).into();
    if let Err(e) = state.nats.publish(subjects::CONNECTION_CHANGED, payload).await {
        tracing::warn!(user_id, revision, "Failed to publish connection change hint: {e}");
    }

    Ok(Json(json!({"status": "syncing"})).into_response())
}
