use axum::Extension;
use axum::Json;
use axum::extract::State;
use common::time::unix_now_secs;
use std::sync::Arc;

use crate::AppState;
use crate::auth::{AppError, AuthenticatedUser, ViewableUserId};
use crate::models::{HeartRateProfile, SelfUser, UpdateUserRequest, UserRow};

const SELECT_USER_ROW: &str = "SELECT u.id, u.display_name, u.timezone,
            a.provider_image as avatar_url,
            u.heart_rate_visibility
     FROM users u
     LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'";

const VALID_VISIBILITIES: &[&str] = &["group_default", "private"];

pub async fn get_self_user(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<SelfUser>, AppError> {
    let query = format!("{SELECT_USER_ROW} WHERE u.id = $1");
    let row: UserRow = sqlx::query_as(&query)
        .bind(&auth_user.id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(SelfUser::from(row)))
}

pub async fn get_heart_rate_profile(
    State(state): State<Arc<AppState>>,
    ViewableUserId(id): ViewableUserId,
) -> Result<Json<HeartRateProfile>, AppError> {
    let query = format!("{SELECT_USER_ROW} WHERE u.id = $1");
    let row: UserRow = sqlx::query_as(&query)
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(HeartRateProfile::from(row)))
}

pub async fn update_user(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<SelfUser>, AppError> {
    let id = auth_user.id.clone();

    let display_name = body
        .display_name
        .as_deref()
        .map(|s| crate::validation::validate_required_name(s, "display_name"))
        .transpose()?;

    let timezone = body
        .timezone
        .as_deref()
        .map(|s| crate::validation::validate_timezone(s).map(str::to_string))
        .transpose()?;

    if let Some(ref vis) = body.heart_rate_visibility
        && !VALID_VISIBILITIES.contains(&vis.as_str())
    {
        return Err(AppError::BadRequest(
            "heart_rate_visibility must be one of: group_default, private".into(),
        ));
    }

    let now = unix_now_secs();

    let result = sqlx::query(
        "UPDATE users SET display_name = COALESCE($1, display_name), timezone = COALESCE($2, timezone), heart_rate_visibility = COALESCE($3, heart_rate_visibility), updated_at = to_timestamp($4) WHERE id = $5"
    )
        .bind(&display_name)
        .bind(&timezone)
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

    Ok(Json(SelfUser::from(row)))
}
