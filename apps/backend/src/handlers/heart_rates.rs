use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use std::sync::Arc;

use crate::error::AppError;
use crate::models::{
    DailyStatsQuery, DailyStatsResponse, HeartRateByDateQuery, HeartRateQuery, HeartRateResponse,
};
use crate::AppState;

fn parse_date(s: &str) -> Result<NaiveDate, AppError> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| AppError::BadRequest(format!("Invalid date: {s}, expected YYYY-MM-DD")))
}

async fn check_user_exists(
    db: &sqlx::PgPool,
    user_id: &str,
) -> Result<(), AppError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(user_id)
            .fetch_one(db)
            .await?;
    if !exists {
        return Err(AppError::NotFound("User not found".into()));
    }
    Ok(())
}

fn parse_period(s: &str) -> Result<(i64, i64), AppError> {
    match s {
        "10m" => Ok((600, 600)),
        "30m" => Ok((1800, 1800)),
        "1h" => Ok((3600, 3600)),
        "3h" => Ok((10800, 10800)),
        "6h" => Ok((21600, 21600)),
        "12h" => Ok((43200, 43200)),
        "24h" => Ok((86400, 86400)),
        _ => Err(AppError::BadRequest(format!(
            "Invalid period: {s}. Allowed: 10m, 30m, 1h, 3h, 6h, 12h, 24h"
        ))),
    }
}

pub async fn list_heart_rates(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(params): Query<HeartRateQuery>,
) -> Result<Json<Vec<HeartRateResponse>>, AppError> {
    let (seconds, limit) = parse_period(&params.period)?;
    let now = chrono::Utc::now().timestamp();
    let from = now - seconds;

    check_user_exists(&state.db, &user_id).await?;

    let records: Vec<HeartRateResponse> = sqlx::query_as(
        "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp
         FROM heart_rate_records
         WHERE user_id = $1 AND recorded_at >= to_timestamp($2)
         ORDER BY recorded_at DESC
         LIMIT $3",
    )
    .bind(&user_id)
    .bind(from)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn heart_rates_by_date(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(params): Query<HeartRateByDateQuery>,
) -> Result<Json<Vec<HeartRateResponse>>, AppError> {
    parse_date(&params.date)?;
    check_user_exists(&state.db, &user_id).await?;

    let records: Vec<HeartRateResponse> = sqlx::query_as(
        "WITH tz AS (SELECT timezone FROM users WHERE id = $1)
         SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp
         FROM heart_rate_records, tz
         WHERE user_id = $1
           AND recorded_at >= ($2::date::timestamp AT TIME ZONE tz.timezone)
           AND recorded_at <  (($2::date + INTERVAL '1 day')::timestamp AT TIME ZONE tz.timezone)
         ORDER BY recorded_at DESC
         LIMIT 2880",
    )
    .bind(&user_id)
    .bind(&params.date)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn daily_stats(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(params): Query<DailyStatsQuery>,
) -> Result<Json<Vec<DailyStatsResponse>>, AppError> {
    parse_date(&params.from)?;
    parse_date(&params.to)?;

    check_user_exists(&state.db, &user_id).await?;

    let records: Vec<DailyStatsResponse> = sqlx::query_as(
        "WITH tz AS (SELECT timezone FROM users WHERE id = $1)
         SELECT
             (time_bucket(INTERVAL '1 day', r.recorded_at, timezone => tz.timezone)
                 AT TIME ZONE tz.timezone)::date::text AS day,
             ROUND(AVG(r.bpm)::numeric, 1)::FLOAT8 AS avg_bpm,
             MIN(r.bpm) AS min_bpm,
             MAX(r.bpm) AS max_bpm,
             COUNT(*)::BIGINT AS count
         FROM heart_rate_records r, tz
         WHERE r.user_id = $1
           AND r.recorded_at >= ($2::date::timestamp AT TIME ZONE tz.timezone)
           AND r.recorded_at <  ($3::date::timestamp AT TIME ZONE tz.timezone)
         GROUP BY 1
         ORDER BY day ASC",
    )
    .bind(&user_id)
    .bind(&params.from)
    .bind(&params.to)
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
