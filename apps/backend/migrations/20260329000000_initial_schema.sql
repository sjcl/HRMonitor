CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS pulsoid_tokens (
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

CREATE TABLE IF NOT EXISTS heart_rate_records (
    id BIGINT GENERATED ALWAYS AS IDENTITY,
    user_id TEXT NOT NULL,
    pulsoid_token_id TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    bpm INTEGER NOT NULL,
    received_at TIMESTAMPTZ NOT NULL
);

SELECT create_hypertable('heart_rate_records', 'recorded_at', if_not_exists => TRUE);

CREATE INDEX IF NOT EXISTS idx_hr_user_time
    ON heart_rate_records(user_id, recorded_at DESC);
