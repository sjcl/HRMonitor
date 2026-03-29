-- NOTE: This file is for documentation reference only.
-- The canonical schema lives in apps/backend/migrations/.
-- Run `sqlx migrate add <name>` from apps/backend/ to create new migrations.

CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pulsoid_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label TEXT,
    access_token TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_connected_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE heart_rate_records (
    id BIGINT GENERATED ALWAYS AS IDENTITY,
    user_id TEXT NOT NULL,
    pulsoid_token_id TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    bpm INTEGER NOT NULL,
    received_at TIMESTAMPTZ NOT NULL
);

-- TimescaleDB hypertable (partitioned by recorded_at)
SELECT create_hypertable('heart_rate_records', by_range('recorded_at'), if_not_exists => TRUE);

CREATE INDEX idx_hr_user_time
    ON heart_rate_records(user_id, recorded_at DESC);
