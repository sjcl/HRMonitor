use axum::extract::{Path, Query, State};
use axum::Json;
use std::sync::Arc;

use crate::error::AppError;
use crate::models::{HeartRateQuery, HeartRateResponse};
use crate::AppState;

pub async fn list_heart_rates(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(params): Query<HeartRateQuery>,
) -> Result<Json<Vec<HeartRateResponse>>, AppError> {
    let limit = params.limit.unwrap_or(1000).min(10000);

    let records: Vec<HeartRateResponse> = match (params.from, params.to) {
        (Some(from), Some(to)) => {
            sqlx::query_as(
                "SELECT bpm, recorded_at as timestamp FROM heart_rate_records WHERE user_id = ? AND recorded_at >= ? AND recorded_at <= ? ORDER BY recorded_at DESC LIMIT ?"
            )
            .bind(&user_id)
            .bind(from)
            .bind(to)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
        (Some(from), None) => {
            sqlx::query_as(
                "SELECT bpm, recorded_at as timestamp FROM heart_rate_records WHERE user_id = ? AND recorded_at >= ? ORDER BY recorded_at DESC LIMIT ?"
            )
            .bind(&user_id)
            .bind(from)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
        (None, Some(to)) => {
            sqlx::query_as(
                "SELECT bpm, recorded_at as timestamp FROM heart_rate_records WHERE user_id = ? AND recorded_at <= ? ORDER BY recorded_at DESC LIMIT ?"
            )
            .bind(&user_id)
            .bind(to)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
        (None, None) => {
            sqlx::query_as(
                "SELECT bpm, recorded_at as timestamp FROM heart_rate_records WHERE user_id = ? ORDER BY recorded_at DESC LIMIT ?"
            )
            .bind(&user_id)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
    };

    Ok(Json(records))
}

pub async fn latest_heart_rate(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Result<Json<HeartRateResponse>, AppError> {
    let record: HeartRateResponse = sqlx::query_as(
        "SELECT bpm, recorded_at as timestamp FROM heart_rate_records WHERE user_id = ? ORDER BY recorded_at DESC LIMIT 1"
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("No heart rate data found".into()))?;

    Ok(Json(record))
}
