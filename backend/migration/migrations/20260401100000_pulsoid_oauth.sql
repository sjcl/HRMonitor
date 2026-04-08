-- =============================================================================
-- Pulsoid OAuth: pulsoid_connections table + connect_requests + updated_at trigger
-- =============================================================================

-- updated_at auto-update trigger function (reusable)
CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- pulsoid_connections: separated from users table, tokens are AES-256-GCM encrypted BYTEA
-- source: 'oauth' (OAuth flow) or 'manual' (user-entered access token)
CREATE TABLE pulsoid_connections (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE UNIQUE,
    source TEXT NOT NULL DEFAULT 'oauth' CHECK (source IN ('oauth', 'manual')),
    access_token BYTEA NOT NULL,
    refresh_token BYTEA,
    key_version INT NOT NULL DEFAULT 1,
    token_expires_at TIMESTAMPTZ,
    last_connected_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER trg_pulsoid_connections_updated_at
    BEFORE UPDATE ON pulsoid_connections
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- Drop old metadata columns from users (moved to pulsoid_connections)
-- Keep pulsoid_access_token for Rust-side migration of existing tokens
ALTER TABLE users DROP COLUMN IF EXISTS pulsoid_last_connected_at;
ALTER TABLE users DROP COLUMN IF EXISTS pulsoid_last_error;

-- connect_requests: short-lived OAuth flow tickets
CREATE TABLE connect_requests (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL DEFAULT 'pulsoid',
    state TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    return_to TEXT NOT NULL DEFAULT '/settings',
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_connect_requests_expires_at
    ON connect_requests(expires_at);
CREATE INDEX idx_connect_requests_user_provider
    ON connect_requests(user_id, provider, used_at, expires_at);
