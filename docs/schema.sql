CREATE TABLE users (
                       id TEXT PRIMARY KEY,
                       name TEXT NOT NULL,
                       created_at INTEGER NOT NULL,
                       updated_at INTEGER NOT NULL
);

CREATE TABLE pulsoid_tokens (
                                id TEXT PRIMARY KEY,
                                user_id TEXT NOT NULL,
                                label TEXT,
                                access_token TEXT NOT NULL,
                                is_active INTEGER NOT NULL,
                                last_connected_at INTEGER,
                                last_error TEXT,
                                created_at INTEGER NOT NULL,
                                updated_at INTEGER NOT NULL
);

CREATE TABLE heart_rate_records (
                                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                                    user_id TEXT NOT NULL,
                                    pulsoid_token_id TEXT NOT NULL,
                                    recorded_at INTEGER NOT NULL,
                                    bpm INTEGER NOT NULL,
                                    received_at INTEGER NOT NULL
);

CREATE INDEX idx_hr_user_time
    ON heart_rate_records(user_id, recorded_at DESC);