use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::error::AppError;

// --- ReturnTo enum (open redirect prevention) ---

#[derive(Debug, Clone, Copy)]
enum ReturnTo {
    Settings,
    Dashboard,
}

impl ReturnTo {
    fn from_str(s: &str) -> Self {
        match s {
            "/dashboard" => ReturnTo::Dashboard,
            _ => ReturnTo::Settings,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ReturnTo::Settings => "/settings",
            ReturnTo::Dashboard => "/dashboard",
        }
    }
}

// --- Request / Response types ---

#[derive(Debug, Deserialize)]
pub struct CreateConnectRequest {
    pub return_to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateConnectResponse {
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

// --- POST /api/oauth/pulsoid/connect ---

pub async fn create_connect(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<CreateConnectRequest>,
) -> Result<Json<CreateConnectResponse>, AppError> {
    let user_id = &auth_user.id;
    let return_to = ReturnTo::from_str(body.return_to.as_deref().unwrap_or("/settings"));

    // Invalidate existing unused tickets for this user+provider
    sqlx::query(
        "UPDATE connect_requests SET used_at = now()
         WHERE user_id = $1 AND provider = 'pulsoid' AND used_at IS NULL AND expires_at > now()",
    )
    .bind(user_id)
    .execute(&state.db)
    .await?;

    // Create new connect request
    let state_value = uuid::Uuid::new_v4().to_string();
    let request_id: (String,) = sqlx::query_as(
        "INSERT INTO connect_requests (user_id, provider, state, expires_at, return_to)
         VALUES ($1, 'pulsoid', $2, now() + INTERVAL '5 minutes', $3)
         RETURNING id",
    )
    .bind(user_id)
    .bind(&state_value)
    .bind(return_to.as_str())
    .fetch_one(&state.db)
    .await?;

    Ok(Json(CreateConnectResponse {
        request_id: request_id.0,
    }))
}

// --- GET /api/oauth/pulsoid/connect/{request_id} ---

pub async fn redirect_to_pulsoid(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Response, AppError> {
    let user_id = &auth_user.id;

    // Fetch connect request — validate ownership, expiry, unused
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT state FROM connect_requests
         WHERE id = $1 AND user_id = $2 AND provider = 'pulsoid'
           AND used_at IS NULL AND expires_at > now()",
    )
    .bind(&request_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;

    let (oauth_state,) = row.ok_or_else(|| {
        tracing::warn!(
            request_id_prefix = &request_id[..request_id.len().min(8)],
            "Invalid or expired connect request"
        );
        AppError::BadRequest("Invalid or expired request".into())
    })?;

    let url = state.pulsoid_oauth.authorization_url(&oauth_state);
    Ok(Redirect::to(&url).into_response())
}

// --- GET /api/oauth/pulsoid/callback ---

pub async fn callback(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<OAuthCallbackQuery>,
) -> Response {
    // 1. No state → 400
    let oauth_state = match params.state {
        Some(ref s) if !s.is_empty() => s.clone(),
        _ => {
            return (StatusCode::BAD_REQUEST, "Missing state parameter").into_response();
        }
    };

    // 2. Atomically consume the ticket (only if owned by current session user)
    let ticket: Option<(String,)> = match sqlx::query_as(
        "UPDATE connect_requests SET used_at = now()
         WHERE state = $1 AND user_id = $2 AND used_at IS NULL AND expires_at > now() AND provider = 'pulsoid'
         RETURNING return_to",
    )
    .bind(&oauth_state)
    .bind(&auth_user.id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("DB error consuming OAuth ticket: {e}");
            return Redirect::to("/settings?pulsoid=exchange_failed").into_response();
        }
    };

    let (return_to_str,) = match ticket {
        Some(t) => t,
        None => {
            tracing::warn!("Callback with invalid/expired/used state");
            return Redirect::to("/settings?pulsoid=invalid_state").into_response();
        }
    };

    let user_id = &auth_user.id;
    let return_to = ReturnTo::from_str(&return_to_str).as_str();

    // 3. Error from Pulsoid (user denied)
    if params.error.is_some() {
        tracing::info!(user_id = %user_id, "Pulsoid authorization denied by user");
        return Redirect::to(&format!("{return_to}?pulsoid=denied")).into_response();
    }

    // 4. No code and no error
    let code = match params.code {
        Some(ref c) if !c.is_empty() => c.clone(),
        _ => {
            tracing::warn!(user_id = %user_id, "Callback without code or error");
            return Redirect::to(&format!("{return_to}?pulsoid=exchange_failed")).into_response();
        }
    };

    // 5. Exchange code for tokens
    let token_response = match state.pulsoid_oauth.exchange_code(&code).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(user_id = %user_id, "Token exchange failed: {e}");
            return Redirect::to(&format!("{return_to}?pulsoid=exchange_failed")).into_response();
        }
    };

    // 6. refresh_token is required
    let refresh_token_plain = match token_response.refresh_token {
        Some(rt) => rt,
        None => {
            tracing::warn!(user_id = %user_id, "Token exchange returned no refresh_token");
            return Redirect::to(&format!("{return_to}?pulsoid=exchange_failed")).into_response();
        }
    };

    // 7. Encrypt tokens
    let (enc_access, key_version) = state.token_encryption.encrypt(&token_response.access_token);
    let (enc_refresh, _) = state.token_encryption.encrypt(&refresh_token_plain);

    // 8. UPSERT into pulsoid_connections
    let upsert_result: Result<(i32,), _> = sqlx::query_as(
        "INSERT INTO pulsoid_connections (user_id, source, access_token, refresh_token, key_version, token_expires_at, last_error, refresh_blocked, connection_state, state_updated_at)
         VALUES ($1, 'oauth', $2, $3, $4, now() + make_interval(secs => $5), NULL, false, 'pending', now())
         ON CONFLICT (user_id) DO UPDATE SET
            source = 'oauth',
            access_token = EXCLUDED.access_token,
            refresh_token = EXCLUDED.refresh_token,
            key_version = EXCLUDED.key_version,
            token_expires_at = EXCLUDED.token_expires_at,
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
    .bind(&enc_refresh)
    .bind(key_version as i32)
    .bind(token_response.expires_in as f64)
    .fetch_one(&state.db)
    .await;

    let config_version = match upsert_result {
        Ok((cv,)) => cv,
        Err(e) => {
            tracing::error!(user_id = %user_id, "Failed to save tokens: {e}");
            return Redirect::to(&format!("{return_to}?pulsoid=exchange_failed")).into_response();
        }
    };

    // 9. Notify pulsoid-ingest via NATS request/reply
    let cmd = common::messages::ConnectionChangeCommand {
        user_id: user_id.to_string(),
        config_version: Some(config_version),
    };
    let payload = serde_json::to_vec(&cmd).unwrap().into();
    let pulsoid_status = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        state
            .nats
            .request(common::messages::subjects::CONNECTION_CHANGED, payload),
    )
    .await
    {
        Ok(Ok(reply)) => {
            match serde_json::from_slice::<common::messages::ConnectionChangeAck>(&reply.payload) {
                Ok(ack) if ack.applied => "authorized",
                _ => "authorized_pending",
            }
        }
        _ => {
            tracing::warn!(user_id = %user_id, "NATS request failed or timed out");
            "authorized_pending"
        }
    };

    tracing::info!(user_id = %user_id, "Pulsoid authorized successfully");
    Redirect::to(&format!("{return_to}?pulsoid={pulsoid_status}")).into_response()
}
