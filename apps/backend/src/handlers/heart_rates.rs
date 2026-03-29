use axum::extract::{Path, Query, State};
use axum::Json;
use std::sync::Arc;

use crate::error::AppError;
use crate::models::{DailyStatsQuery, DailyStatsResponse, HeartRateQuery, HeartRateResponse};
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
                "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp FROM heart_rate_records WHERE user_id = $1 AND recorded_at >= to_timestamp($2) AND recorded_at <= to_timestamp($3) ORDER BY recorded_at DESC LIMIT $4"
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
                "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp FROM heart_rate_records WHERE user_id = $1 AND recorded_at >= to_timestamp($2) ORDER BY recorded_at DESC LIMIT $3"
            )
            .bind(&user_id)
            .bind(from)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
        (None, Some(to)) => {
            sqlx::query_as(
                "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp FROM heart_rate_records WHERE user_id = $1 AND recorded_at <= to_timestamp($2) ORDER BY recorded_at DESC LIMIT $3"
            )
            .bind(&user_id)
            .bind(to)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
        (None, None) => {
            sqlx::query_as(
                "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp FROM heart_rate_records WHERE user_id = $1 ORDER BY recorded_at DESC LIMIT $2"
            )
            .bind(&user_id)
            .bind(limit)
            .fetch_all(&state.db)
            .await?
        }
    };

    Ok(Json(records))
}

pub async fn daily_stats(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(params): Query<DailyStatsQuery>,
) -> Result<Json<Vec<DailyStatsResponse>>, AppError> {
    let records: Vec<DailyStatsResponse> = sqlx::query_as(
        "SELECT
            EXTRACT(EPOCH FROM date_trunc('day', recorded_at AT TIME ZONE 'UTC'))::BIGINT as day,
            ROUND(AVG(bpm)::numeric, 1)::FLOAT8 as avg_bpm,
            MIN(bpm) as min_bpm,
            MAX(bpm) as max_bpm,
            COUNT(*)::BIGINT as count
         FROM heart_rate_records
         WHERE user_id = $1 AND recorded_at >= to_timestamp($2) AND recorded_at < to_timestamp($3)
         GROUP BY date_trunc('day', recorded_at AT TIME ZONE 'UTC')
         ORDER BY day ASC",
    )
    .bind(&user_id)
    .bind(params.from)
    .bind(params.to)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn latest_heart_rate(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Result<Json<HeartRateResponse>, AppError> {
    let record: HeartRateResponse = sqlx::query_as(
        "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp FROM heart_rate_records WHERE user_id = $1 ORDER BY recorded_at DESC LIMIT 1"
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("No heart rate data found".into()))?;

    Ok(Json(record))
}
