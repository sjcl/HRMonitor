-- NOTE: This file is documentation only. It shows the schema as it stands
-- after all migrations have run, which is easier to read than 12 separate
-- files. The canonical schema lives in backend/migration/migrations/ and is
-- what actually executes.
--
-- Add a migration with `sqlx migrate add <name>` from backend/migration/.

CREATE EXTENSION IF NOT EXISTS timescaledb;

-- ---------------------------------------------------------------------------
-- Identity
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id                    TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    display_name          TEXT NOT NULL,
    timezone              TEXT NOT NULL DEFAULT 'UTC',
    primary_email         TEXT,
    role                  TEXT NOT NULL DEFAULT 'user',
    -- 'group_default': follow each group's sharing setting.
    -- 'private':       never visible to anyone else, whatever the groups say.
    heart_rate_visibility TEXT NOT NULL DEFAULT 'group_default'
        CHECK (heart_rate_visibility IN ('group_default', 'private')),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Links a local user to an external identity provider. The UNIQUE constraint
-- is what makes a returning Discord user resolve to the same users.id.
--
-- The token columns are legacy: the Discord login flow requests `identify`
-- only, uses the access token once, and never persists it. They stay NULL for
-- provider='discord' rows.
CREATE TABLE accounts (
    id                      TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    user_id                 TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider                TEXT NOT NULL,
    provider_account_id     TEXT NOT NULL,
    account_type            TEXT NOT NULL,
    provider_email          TEXT,
    provider_email_verified TIMESTAMPTZ,
    provider_name           TEXT,
    provider_image          TEXT,
    access_token            TEXT,
    refresh_token           TEXT,
    expires_at              BIGINT,
    token_type              TEXT,
    scope                   TEXT,
    id_token                TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_account_id)
);

-- DEPRECATED. Sessions moved to Redis when authentication became
-- Ed25519 JWT + refresh sessions; nothing reads or writes this table any more.
-- Kept only so that dropping it can be a separate change.
CREATE TABLE sessions (
    id            TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    session_token TEXT NOT NULL UNIQUE,
    user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires       TIMESTAMPTZ NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Heart rate
-- ---------------------------------------------------------------------------

CREATE TABLE heart_rate_records (
    id          BIGINT GENERATED ALWAYS AS IDENTITY,
    user_id     TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    bpm         INTEGER NOT NULL,
    received_at TIMESTAMPTZ NOT NULL
);

SELECT create_hypertable(
    'heart_rate_records',
    by_range('recorded_at', INTERVAL '1 day')
);

CREATE INDEX idx_hr_user_time ON heart_rate_records (user_id, recorded_at DESC);

-- Continuous aggregate backing the minute-stats endpoints.
CREATE MATERIALIZED VIEW heart_rate_1m
WITH (timescaledb.continuous) AS
SELECT
    user_id,
    time_bucket(INTERVAL '1 minute', recorded_at) AS bucket,
    avg(bpm) AS avg_bpm,
    min(bpm) AS min_bpm,
    max(bpm) AS max_bpm,
    count(*) AS sample_count
FROM heart_rate_records
GROUP BY user_id, bucket;

-- ---------------------------------------------------------------------------
-- Sharing groups
-- ---------------------------------------------------------------------------

CREATE TABLE groups (
    id            TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    name          TEXT,
    -- 'group':  members may invite. 'group+': owner only.
    invite_policy TEXT NOT NULL DEFAULT 'group'
        CHECK (invite_policy IN ('group', 'group+')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE group_members (
    id         TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    group_id   TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'member')),
    -- Whether this member's heart rate is visible to the rest of the group.
    sharing    BOOLEAN NOT NULL DEFAULT false,
    status     TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'left')),
    joined_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    left_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (group_id, user_id)
);

CREATE TABLE group_invites (
    id             TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    group_id       TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    -- SHA-256 hex of the raw invite token; the raw value is shown once and
    -- never stored.
    token_hash     TEXT NOT NULL UNIQUE,
    created_by     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at     TIMESTAMPTZ NOT NULL,
    revoked        BOOLEAN NOT NULL DEFAULT false,
    max_uses       INT,
    use_count      INT NOT NULL DEFAULT 0,
    -- Set for a personal invite aimed at one specific user.
    target_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (max_uses IS NULL OR max_uses > 0),
    CHECK (use_count >= 0),
    CHECK (max_uses IS NULL OR use_count <= max_uses)
);

-- ---------------------------------------------------------------------------
-- Pulsoid integration
-- ---------------------------------------------------------------------------

CREATE SEQUENCE pulsoid_revision_seq;

CREATE TABLE pulsoid_connections (
    id                TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    user_id           TEXT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    source            TEXT NOT NULL DEFAULT 'oauth' CHECK (source IN ('oauth', 'manual')),
    -- AES-256-GCM ciphertext (nonce || ciphertext || tag).
    access_token      BYTEA NOT NULL,
    refresh_token     BYTEA,
    key_version       INT NOT NULL DEFAULT 1,
    token_expires_at  TIMESTAMPTZ,
    last_connected_at TIMESTAMPTZ,
    last_error        TEXT,
    -- 'error' is sticky: only a 401 or invalid_grant is terminal.
    connection_state  TEXT NOT NULL DEFAULT 'pending'
        CHECK (connection_state IN ('pending', 'connected', 'error')),
    state_updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Bumped on every token change; pulsoid-ingest uses it to swap workers.
    revision          INTEGER NOT NULL DEFAULT nextval('pulsoid_revision_seq'),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Supports pulsoid-refresher's 60s scan for tokens nearing expiry.
CREATE INDEX idx_pulsoid_refresh_scanner
    ON pulsoid_connections (token_expires_at)
    INCLUDE (user_id, revision)
    WHERE source = 'oauth'
      AND connection_state != 'error'
      AND token_expires_at IS NOT NULL;

-- One-time CSRF state tickets for the Pulsoid OAuth flow.
--
-- Note: the *Discord* login flow keeps its equivalent state in Redis instead,
-- so that ordinary authentication never touches Postgres.
CREATE TABLE connect_requests (
    id         TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider   TEXT NOT NULL DEFAULT 'pulsoid',
    state      TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    return_to  TEXT NOT NULL DEFAULT '/settings',
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
