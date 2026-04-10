use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Extension;
use axum::Json;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use common::messages::{subjects, ConnectionChangeAck, ConnectionChangeCommand};

use crate::auth::AuthenticatedUser;
use crate::error::AppError;
use crate::handlers::utils::connection_change_applied;
use crate::models::{PulsoidTokenResponse, SetManualTokenRequest};
use crate::AppState;

type PulsoidConnectionRow = (String, String, i64, Option<i64>, Option<String>);

const NATS_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

async fn nats_request_status(
    nats: &async_nats::Client,
    cmd: &ConnectionChangeCommand,
    user_id: &str,
) -> &'static str {
    let payload = serde_json::to_vec(cmd).unwrap().into();
    match tokio::time::timeout(
        NATS_REQUEST_TIMEOUT,
        nats.request(subjects::CONNECTION_CHANGED, payload),
    )
    .await
    {
        Ok(Ok(reply)) => match serde_json::from_slice::<ConnectionChangeAck>(&reply.payload) {
            Ok(ack) if connection_change_applied(&ack, user_id) => "applied",
            Ok(_) => "pending",
            Err(e) => {
                tracing::warn!(user_id, "Failed to parse ack: {e}");
                "pending"
            }
        },
        Ok(Err(e)) => {
            tracing::warn!(user_id, "NATS request failed: {e}");
            "pending"
        }
        Err(_) => {
            tracing::warn!(user_id, "NATS request timed out (ingest may be down)");
            "pending"
        }
    }
}

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
        "DELETE FROM pulsoid_connections WHERE user_id = $1 RETURNING config_version",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;

    let config_version = match deleted {
        Some((cv,)) => cv,
        None => return Err(AppError::NotFound("Pulsoid token not configured".into())),
    };

    // Notify pulsoid-ingest via NATS request/reply
    let cmd = ConnectionChangeCommand {
        user_id: user_id.to_string(),
        config_version: Some(config_version),
    };
    let status = nats_request_status(&state.nats, &cmd, user_id).await;

    Ok(Json(json!({"status": status})).into_response())
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

    let (config_version,): (i32,) = sqlx::query_as(
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
            config_version = nextval('pulsoid_config_version_seq')
         RETURNING config_version",
    )
    .bind(user_id)
    .bind(&enc_access)
    .bind(key_version as i32)
    .fetch_one(&state.db)
    .await?;

    // Notify pulsoid-ingest via NATS request/reply
    let cmd = ConnectionChangeCommand {
        user_id: user_id.to_string(),
        config_version: Some(config_version),
    };
    let status = nats_request_status(&state.nats, &cmd, user_id).await;

    Ok(Json(json!({"status": status})).into_response())
}
