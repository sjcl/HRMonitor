use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

use crate::error::AppError;
use crate::models::{CreateUserRequest, UpdateUserRequest, User, UserListItem};
use crate::AppState;

pub async fn list_users(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<UserListItem>>, AppError> {
    let users: Vec<UserListItem> = sqlx::query_as(
        "SELECT
            u.id,
            u.name,
            u.created_at,
            (SELECT hr.bpm FROM heart_rate_records hr WHERE hr.user_id = u.id ORDER BY hr.recorded_at DESC LIMIT 1) as latest_bpm,
            (SELECT COUNT(*) FROM pulsoid_tokens pt WHERE pt.user_id = u.id) as token_count
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

    sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(&body.name)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;

    let user = User {
        id,
        name: body.name,
        created_at: now,
        updated_at: now,
    };

    Ok((StatusCode::CREATED, Json(user)))
}

pub async fn update_user(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<User>, AppError> {
    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("Name cannot be empty".into()));
    }

    let now = now_unix();

    let result = sqlx::query("UPDATE users SET name = ?, updated_at = ? WHERE id = ?")
        .bind(&body.name)
        .bind(now)
        .bind(&id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    let user: User = sqlx::query_as("SELECT * FROM users WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;

    Ok(Json(user))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
