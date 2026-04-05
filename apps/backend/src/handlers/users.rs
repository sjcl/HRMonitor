use axum::Json;
use axum::Extension;
use axum::extract::{Path, State};
use redis::AsyncCommands;
use std::sync::Arc;

use crate::AppState;
use crate::auth::{AuthenticatedUser, ensure_can_view_user, ensure_self};
use crate::broadcast::LatestHeartRateUpdate;
use crate::error::AppError;
use crate::models::{UpdateUserRequest, User, UserListItem, UserRow};

const SELECT_USER_ROW: &str =
    "SELECT u.id, u.display_name, u.timezone,
            a.provider_image as avatar_url,
            u.heart_rate_visibility,
            EXTRACT(EPOCH FROM u.created_at)::BIGINT as created_at,
            EXTRACT(EPOCH FROM u.updated_at)::BIGINT as updated_at
     FROM users u
     LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'";

const VALID_VISIBILITIES: &[&str] = &["public", "group", "private"];

#[derive(Debug, sqlx::FromRow)]
struct UserListRow {
    id: String,
    display_name: String,
    avatar_url: Option<String>,
    has_pulsoid_token: bool,
    created_at: i64,
}

pub async fn list_users(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<Vec<UserListItem>>, AppError> {
    let rows: Vec<UserListRow> = sqlx::query_as(
        "SELECT
            u.id,
            u.display_name,
            a.provider_image as avatar_url,
            EXTRACT(EPOCH FROM u.created_at)::BIGINT as created_at,
            EXISTS (SELECT 1 FROM pulsoid_connections WHERE user_id = u.id) as has_pulsoid_token
        FROM users u
        LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'
        WHERE u.heart_rate_visibility = 'public' OR u.id = $1
        ORDER BY u.created_at DESC",
    )
    .bind(&auth_user.id)
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
            avatar_url: row.avatar_url,
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
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<User>, AppError> {
    ensure_can_view_user(&state.db, &auth_user, &id).await?;

    let query = format!("{SELECT_USER_ROW} WHERE u.id = $1");
    let row: UserRow = sqlx::query_as(&query)
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(User::from(row)))
}

pub async fn update_user(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<User>, AppError> {
    ensure_self(&auth_user, &id)?;

    if let Some(ref display_name) = body.display_name
        && display_name.trim().is_empty()
    {
        return Err(AppError::BadRequest("Display name cannot be empty".into()));
    }

    if let Some(ref vis) = body.heart_rate_visibility
        && !VALID_VISIBILITIES.contains(&vis.as_str())
    {
        return Err(AppError::BadRequest(
            "heart_rate_visibility must be one of: public, group, private".into(),
        ));
    }

    let now = now_unix();

    let result = sqlx::query(
        "UPDATE users SET display_name = COALESCE($1, display_name), timezone = COALESCE($2, timezone), heart_rate_visibility = COALESCE($3, heart_rate_visibility), updated_at = to_timestamp($4) WHERE id = $5"
    )
        .bind(&body.display_name)
        .bind(&body.timezone)
        .bind(&body.heart_rate_visibility)
        .bind(now)
        .bind(&id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    let query = format!("{SELECT_USER_ROW} WHERE u.id = $1");
    let row: UserRow = sqlx::query_as(&query)
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
