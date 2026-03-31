use axum::Json;
use axum::extract::{Path, State};
use redis::AsyncCommands;
use std::sync::Arc;

use crate::AppState;
use crate::broadcast::LatestHeartRateUpdate;
use crate::error::AppError;
use crate::models::{UpdateUserRequest, User, UserListItem, UserRow};

#[derive(Debug, sqlx::FromRow)]
struct UserListRow {
    id: String,
    display_name: String,
    has_pulsoid_token: bool,
    created_at: i64,
}

pub async fn list_users(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<UserListItem>>, AppError> {
    let rows: Vec<UserListRow> = sqlx::query_as(
        "SELECT
            u.id,
            u.display_name,
            EXTRACT(EPOCH FROM u.created_at)::BIGINT as created_at,
            (u.pulsoid_access_token IS NOT NULL) as has_pulsoid_token
        FROM users u
        ORDER BY u.created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;

    // Read latest_bpm from Redis for all users
    let mut redis = state.redis.lock().await;
    let mut users = Vec::with_capacity(rows.len());
    let mut missing_bpm_indices: Vec<usize> = Vec::new();
    let mut missing_bpm_user_ids: Vec<String> = Vec::new();

    for row in rows {
        let key = format!("latest_bpm:{}", row.id);
        let latest_bpm: Option<i32> = redis
            .get::<_, Option<String>>(&key)
            .await
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str::<LatestHeartRateUpdate>(&v).ok())
            .map(|u| u.bpm);

        if latest_bpm.is_none() {
            missing_bpm_indices.push(users.len());
            missing_bpm_user_ids.push(row.id.clone());
        }

        users.push(UserListItem {
            id: row.id,
            display_name: row.display_name,
            latest_bpm,
            has_pulsoid_token: row.has_pulsoid_token,
            created_at: row.created_at,
        });
    }
    drop(redis);

    // Fall back to DB for users without cached latest_bpm
    if !missing_bpm_user_ids.is_empty() {
        let db_rows: Vec<(String, i32, i64)> = sqlx::query_as(
            "SELECT DISTINCT ON (user_id) user_id, bpm,
                    EXTRACT(EPOCH FROM recorded_at)::BIGINT as recorded_at
             FROM heart_rate_records
             WHERE user_id = ANY($1)
             ORDER BY user_id, recorded_at DESC",
        )
        .bind(&missing_bpm_user_ids)
        .fetch_all(&state.db)
        .await?;

        let bpm_map: std::collections::HashMap<&str, (i32, i64)> = db_rows
            .iter()
            .map(|(uid, bpm, ts)| (uid.as_str(), (*bpm, *ts)))
            .collect();

        // Write back to Redis
        {
            let mut redis = state.redis.lock().await;
            for (uid, &(bpm, recorded_at)) in &bpm_map {
                let update = LatestHeartRateUpdate {
                    user_id: uid.to_string(),
                    bpm,
                    recorded_at,
                    received_at: recorded_at,
                };
                if let Ok(json) = serde_json::to_string(&update) {
                    let key = format!("latest_bpm:{uid}");
                    let _: Result<Option<String>, _> = redis::cmd("SET")
                        .arg(&key)
                        .arg(&json)
                        .arg("NX")
                        .query_async(&mut *redis)
                        .await;
                }
            }
        }

        for &idx in &missing_bpm_indices {
            if let Some(&(bpm, _)) = bpm_map.get(users[idx].id.as_str()) {
                users[idx].latest_bpm = Some(bpm);
            }
        }
    }

    Ok(Json(users))
}

pub async fn get_user(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<User>, AppError> {
    let row: UserRow = sqlx::query_as(
        "SELECT id, display_name, timezone, pulsoid_access_token,
                EXTRACT(EPOCH FROM pulsoid_last_connected_at)::BIGINT as pulsoid_last_connected_at,
                pulsoid_last_error,
                EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
                EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
         FROM users WHERE id = $1",
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
    if let Some(ref display_name) = body.display_name
        && display_name.trim().is_empty()
    {
        return Err(AppError::BadRequest("Display name cannot be empty".into()));
    }

    let now = now_unix();

    let result = sqlx::query(
        "UPDATE users SET display_name = COALESCE($1, display_name), timezone = COALESCE($2, timezone), updated_at = to_timestamp($3) WHERE id = $4"
    )
        .bind(&body.display_name)
        .bind(&body.timezone)
        .bind(now)
        .bind(&id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    let row: UserRow = sqlx::query_as(
        "SELECT id, display_name, timezone, pulsoid_access_token,
                EXTRACT(EPOCH FROM pulsoid_last_connected_at)::BIGINT as pulsoid_last_connected_at,
                pulsoid_last_error,
                EXTRACT(EPOCH FROM created_at)::BIGINT as created_at,
                EXTRACT(EPOCH FROM updated_at)::BIGINT as updated_at
         FROM users WHERE id = $1",
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
