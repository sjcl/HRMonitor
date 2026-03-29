use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

use crate::error::AppError;
use crate::models::{CreateUserRequest, UpdateUserRequest, User, UserListItem, UserRow};
use crate::AppState;

pub async fn list_users(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<UserListItem>>, AppError> {
    let users: Vec<UserListItem> = sqlx::query_as(
        "SELECT
            u.id,
            u.name,
            EXTRACT(EPOCH FROM u.created_at)::BIGINT as created_at,
            (SELECT hr.bpm FROM heart_rate_records hr WHERE hr.user_id = u.id ORDER BY hr.recorded_at DESC LIMIT 1) as latest_bpm,
            (u.pulsoid_access_token IS NOT NULL) as has_pulsoid_token
        FROM users u
        ORDER BY u.created_at DESC"
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(users))
}

pub async fn create_user(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), AppError> {
    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("Name cannot be empty".into()));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_unix();
    let timezone = body.timezone.unwrap_or_else(|| "UTC".to_string());

    sqlx::query("INSERT INTO users (id, name, timezone, created_at, updated_at) VALUES ($1, $2, $3, to_timestamp($4), to_timestamp($5))")
        .bind(&id)
        .bind(&body.name)
        .bind(&timezone)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;

    let user = User {
        id,
        name: body.name,
        timezone,
        created_at: now,
        updated_at: now,
    };

    Ok((StatusCode::CREATED, Json(user)))
}

pub async fn get_user(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<User>, AppError> {
    let row: UserRow = sqlx::query_as(
        "SELECT id, name, timezone, pulsoid_access_token,
                EXTRACT(EPOCH FROM pulsoid_last_connected_at)::BIGINT as pulsoid_last_connected_at,
                pulsoid_last_error,
                EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
                EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
         FROM users WHERE id = $1"
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(User::from(row)))
}

pub async fn update_user(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<User>, AppError> {
    if let Some(ref name) = body.name {
        if name.trim().is_empty() {
            return Err(AppError::BadRequest("Name cannot be empty".into()));
        }
    }

    let now = now_unix();

    let result = sqlx::query(
        "UPDATE users SET name = COALESCE($1, name), timezone = COALESCE($2, timezone), updated_at = to_timestamp($3) WHERE id = $4"
    )
        .bind(&body.name)
        .bind(&body.timezone)
        .bind(now)
        .bind(&id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    let row: UserRow = sqlx::query_as(
        "SELECT id, name, timezone, pulsoid_access_token,
                EXTRACT(EPOCH FROM pulsoid_last_connected_at)::BIGINT as pulsoid_last_connected_at,
                pulsoid_last_error,
                EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
                EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
         FROM users WHERE id = $1"
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(User::from(row)))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
