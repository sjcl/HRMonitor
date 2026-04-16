use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::NaiveDate;
use common::time::unix_now_secs;
use std::sync::Arc;

use crate::AppState;
use crate::auth::{AppError, AuthenticatedUser, ViewableUserId, ensure_active_member};
use crate::models::{
    DailyStatsQuery, DailyStatsResponse, GroupHeartRateResponse, GroupMinuteStatsResponse,
    HeartRateByDateQuery, HeartRateQuery, HeartRateResponse, MinuteStatsResponse,
};

fn parse_date(s: &str) -> Result<NaiveDate, AppError> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| AppError::BadRequest(format!("Invalid date: {s}, expected YYYY-MM-DD")))
}

fn parse_period(s: &str) -> Result<i64, AppError> {
    match s {
        "10m" => Ok(600),
        "30m" => Ok(1800),
        "1h" => Ok(3600),
        "3h" => Ok(10800),
        "6h" => Ok(21600),
        "12h" => Ok(43200),
        "24h" => Ok(86400),
        _ => Err(AppError::BadRequest(format!(
            "Invalid period: {s}. Allowed: 10m, 30m, 1h, 3h, 6h, 12h, 24h"
        ))),
    }
}

pub async fn list_heart_rates(
    State(state): State<Arc<AppState>>,
    ViewableUserId(user_id): ViewableUserId,
    Query(params): Query<HeartRateQuery>,
) -> Result<Json<Vec<HeartRateResponse>>, AppError> {
    let seconds = parse_period(&params.period)?;
    let now = unix_now_secs();
    let from = now - seconds;

    let records: Vec<HeartRateResponse> = sqlx::query_as(
        "SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp
         FROM heart_rate_records
         WHERE user_id = $1 AND recorded_at >= to_timestamp($2)
         ORDER BY recorded_at DESC
         LIMIT $3",
    )
    .bind(&user_id)
    .bind(from)
    .bind(seconds)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn heart_rates_by_date(
    State(state): State<Arc<AppState>>,
    ViewableUserId(user_id): ViewableUserId,
    Query(params): Query<HeartRateByDateQuery>,
) -> Result<Json<Vec<HeartRateResponse>>, AppError> {
    parse_date(&params.date)?;

    let records: Vec<HeartRateResponse> = sqlx::query_as(
        "WITH tz AS (SELECT timezone FROM users WHERE id = $1)
         SELECT bpm, EXTRACT(EPOCH FROM recorded_at)::BIGINT as timestamp
         FROM heart_rate_records, tz
         WHERE user_id = $1
           AND recorded_at >= ($2::date::timestamp AT TIME ZONE tz.timezone)
           AND recorded_at <  (($2::date + INTERVAL '1 day')::timestamp AT TIME ZONE tz.timezone)
         ORDER BY recorded_at",
    )
    .bind(&user_id)
    .bind(&params.date)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn daily_stats(
    State(state): State<Arc<AppState>>,
    ViewableUserId(user_id): ViewableUserId,
    Query(params): Query<DailyStatsQuery>,
) -> Result<Json<Option<DailyStatsResponse>>, AppError> {
    parse_date(&params.date)?;

    let record: Option<DailyStatsResponse> = sqlx::query_as(
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
           AND r.recorded_at <  (($2::date + INTERVAL '1 day')::timestamp AT TIME ZONE tz.timezone)
         GROUP BY 1",
    )
    .bind(&user_id)
    .bind(&params.date)
    .fetch_optional(&state.db)
    .await?;

    Ok(Json(record))
}

pub async fn minute_stats(
    State(state): State<Arc<AppState>>,
    ViewableUserId(user_id): ViewableUserId,
    Query(params): Query<HeartRateQuery>,
) -> Result<Json<Vec<MinuteStatsResponse>>, AppError> {
    let seconds = parse_period(&params.period)?;
    let now = unix_now_secs();
    let from = now - seconds;

    let records: Vec<MinuteStatsResponse> = sqlx::query_as(
        "SELECT
             EXTRACT(EPOCH FROM bucket)::BIGINT AS timestamp,
             avg_bpm,
             min_bpm,
             max_bpm,
             sample_count
         FROM heart_rate_1m
         WHERE user_id = $1
           AND bucket >= to_timestamp($2)
         ORDER BY bucket",
    )
    .bind(&user_id)
    .bind(from)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn minute_stats_by_date(
    State(state): State<Arc<AppState>>,
    ViewableUserId(user_id): ViewableUserId,
    Query(params): Query<HeartRateByDateQuery>,
) -> Result<Json<Vec<MinuteStatsResponse>>, AppError> {
    parse_date(&params.date)?;

    let records: Vec<MinuteStatsResponse> = sqlx::query_as(
        "WITH tz AS (SELECT timezone FROM users WHERE id = $1)
         SELECT
             EXTRACT(EPOCH FROM bucket)::BIGINT AS timestamp,
             avg_bpm,
             min_bpm,
             max_bpm,
             sample_count
         FROM heart_rate_1m, tz
         WHERE user_id = $1
           AND bucket >= ($2::date::timestamp AT TIME ZONE tz.timezone)
           AND bucket <  (($2::date + INTERVAL '1 day')::timestamp AT TIME ZONE tz.timezone)
         ORDER BY bucket",
    )
    .bind(&user_id)
    .bind(&params.date)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn group_heart_rates(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<HeartRateQuery>,
) -> Result<Json<Vec<GroupHeartRateResponse>>, AppError> {
    let seconds = parse_period(&params.period)?;
    let now = unix_now_secs();
    let from = now - seconds;

    ensure_active_member(&state.db, &group_id, &auth_user.id).await?;

    let records: Vec<GroupHeartRateResponse> = sqlx::query_as(
        "SELECT hr.user_id,
                hr.bpm,
                EXTRACT(EPOCH FROM hr.recorded_at)::BIGINT AS timestamp
         FROM heart_rate_records hr
         JOIN group_members gm ON gm.user_id = hr.user_id
         JOIN users u ON u.id = gm.user_id
         WHERE gm.group_id = $1
           AND gm.status = 'active'
           AND (gm.sharing = true OR gm.user_id = $2)
           AND (u.heart_rate_visibility != 'private' OR gm.user_id = $2)
           AND hr.recorded_at >= to_timestamp($3)
         ORDER BY hr.recorded_at",
    )
    .bind(&group_id)
    .bind(&auth_user.id)
    .bind(from)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}

pub async fn group_minute_stats(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<HeartRateQuery>,
) -> Result<Json<Vec<GroupMinuteStatsResponse>>, AppError> {
    let seconds = parse_period(&params.period)?;
    let now = unix_now_secs();
    let from = now - seconds;

    ensure_active_member(&state.db, &group_id, &auth_user.id).await?;

    let records: Vec<GroupMinuteStatsResponse> = sqlx::query_as(
        "SELECT hm.user_id,
                EXTRACT(EPOCH FROM hm.bucket)::BIGINT AS timestamp,
                hm.avg_bpm,
                hm.min_bpm,
                hm.max_bpm,
                hm.sample_count
         FROM heart_rate_1m hm
         JOIN group_members gm ON gm.user_id = hm.user_id
         JOIN users u ON u.id = gm.user_id
         WHERE gm.group_id = $1
           AND gm.status = 'active'
           AND (gm.sharing = true OR gm.user_id = $2)
           AND (u.heart_rate_visibility != 'private' OR gm.user_id = $2)
           AND hm.bucket >= to_timestamp($3)
         ORDER BY hm.bucket",
    )
    .bind(&group_id)
    .bind(&auth_user.id)
    .bind(from)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(records))
}
