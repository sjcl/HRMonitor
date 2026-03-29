use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

use crate::error::AppError;
use crate::models::{CreateTokenRequest, PulsoidToken, TokenResponse, UpdateTokenRequest};
use crate::AppState;

const SELECT_TOKEN_COLUMNS: &str =
    "SELECT id, user_id, label, access_token, is_active,
            EXTRACT(EPOCH FROM last_connected_at)::BIGINT as last_connected_at,
            last_error,
            EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
            EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
     FROM pulsoid_tokens";

pub async fn list_tokens(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<TokenResponse>>, AppError> {
    let query = format!("{SELECT_TOKEN_COLUMNS} WHERE user_id = $1 ORDER BY created_at DESC");
    let tokens: Vec<PulsoidToken> = sqlx::query_as(&query)
        .bind(&user_id)
        .fetch_all(&state.db)
        .await?;

    Ok(Json(tokens.into_iter().map(TokenResponse::from).collect()))
}

pub async fn create_token(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Json(body): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<TokenResponse>), AppError> {
    if body.access_token.trim().is_empty() {
        return Err(AppError::BadRequest("Access token cannot be empty".into()));
    }

    // Verify user exists
    let user_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(&user_id)
            .fetch_one(&state.db)
            .await?;

    if !user_exists {
        return Err(AppError::NotFound("User not found".into()));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();

    sqlx::query(
        "INSERT INTO pulsoid_tokens (id, user_id, label, access_token, is_active, created_at, updated_at) VALUES ($1, $2, $3, $4, true, to_timestamp($5), to_timestamp($6))"
    )
    .bind(&id)
    .bind(&user_id)
    .bind(&body.label)
    .bind(&body.access_token)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    let token = PulsoidToken {
        id,
        user_id,
        label: body.label,
        access_token: body.access_token,
        is_active: true,
        last_connected_at: None,
        last_error: None,
        created_at: now,
        updated_at: now,
    };

    // Start worker for new active token
    state.worker_manager.start(token.clone()).await;

    Ok((StatusCode::CREATED, Json(TokenResponse::from(token))))
}

pub async fn update_token(
    State(state): State<Arc<AppState>>,
    Path(token_id): Path<String>,
    Json(body): Json<UpdateTokenRequest>,
) -> Result<Json<TokenResponse>, AppError> {
    let query = format!("{SELECT_TOKEN_COLUMNS} WHERE id = $1");
    let token: PulsoidToken = sqlx::query_as(&query)
        .bind(&token_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Token not found".into()))?;

    let new_label = body.label.as_deref().unwrap_or(token.label.as_deref().unwrap_or(""));
    let new_is_active = body.is_active.unwrap_or(token.is_active);
    let now = now_unix();

    sqlx::query("UPDATE pulsoid_tokens SET label = $1, is_active = $2, updated_at = to_timestamp($3) WHERE id = $4")
        .bind(new_label)
        .bind(new_is_active)
        .bind(now)
        .bind(&token_id)
        .execute(&state.db)
        .await?;

    // Start or stop worker based on is_active change
    if new_is_active && !token.is_active {
        let mut updated_token = token.clone();
        updated_token.is_active = true;
        updated_token.label = Some(new_label.to_string());
        state.worker_manager.start(updated_token).await;
    } else if !new_is_active && token.is_active {
        state.worker_manager.stop(&token_id).await;
    }

    let updated: PulsoidToken = sqlx::query_as(&query)
        .bind(&token_id)
        .fetch_one(&state.db)
        .await?;

    Ok(Json(TokenResponse::from(updated)))
}

pub async fn delete_token(
    State(state): State<Arc<AppState>>,
    Path(token_id): Path<String>,
) -> Result<StatusCode, AppError> {
    state.worker_manager.stop(&token_id).await;

    let result = sqlx::query("DELETE FROM pulsoid_tokens WHERE id = $1")
        .bind(&token_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Token not found".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
