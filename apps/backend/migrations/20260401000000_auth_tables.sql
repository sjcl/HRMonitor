-- Enable pgcrypto for gen_random_uuid() in DEFAULT expressions
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- Add DEFAULT to users.id so Auth.js adapter can let DB generate UUIDs
ALTER TABLE users ALTER COLUMN id SET DEFAULT gen_random_uuid()::TEXT;

-- Rename name -> display_name (app identity, not provider profile)
ALTER TABLE users RENAME COLUMN name TO display_name;

-- Add auth-related columns to users
ALTER TABLE users ADD COLUMN IF NOT EXISTS primary_email TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS role TEXT NOT NULL DEFAULT 'user';

-- OAuth provider links (1 user : N accounts)
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    account_type TEXT NOT NULL,
    provider_email TEXT,
    provider_email_verified TIMESTAMPTZ,
    provider_name TEXT,
    provider_image TEXT,
    access_token TEXT,
    refresh_token TEXT,
    expires_at BIGINT,
    token_type TEXT,
    scope TEXT,
    id_token TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(provider, provider_account_id)
);

CREATE INDEX IF NOT EXISTS idx_accounts_user_id ON accounts(user_id);

-- Database sessions (1 user : N sessions)
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    session_token TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_sessions_token ON sessions(session_token);
CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
