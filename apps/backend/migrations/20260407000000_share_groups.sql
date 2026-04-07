-- =============================================================================
-- Share Groups: groups, group_members, group_invites
-- =============================================================================

CREATE TABLE groups (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    name TEXT,
    invite_policy TEXT NOT NULL DEFAULT 'group'
        CHECK (invite_policy IN ('group', 'group+')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER trg_groups_updated_at
    BEFORE UPDATE ON groups
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TABLE group_members (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member'
        CHECK (role IN ('owner', 'member')),
    sharing BOOLEAN NOT NULL DEFAULT false,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'left')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    left_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(group_id, user_id)
);

CREATE INDEX idx_group_members_user_active
    ON group_members(user_id) WHERE status = 'active';
CREATE INDEX idx_group_members_group_active
    ON group_members(group_id) WHERE status = 'active';

CREATE TRIGGER trg_group_members_updated_at
    BEFORE UPDATE ON group_members
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TABLE group_invites (
    id TEXT PRIMARY KEY DEFAULT gen_random_uuid()::TEXT,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_by TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked BOOLEAN NOT NULL DEFAULT false,
    max_uses INT,
    use_count INT NOT NULL DEFAULT 0,
    target_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (max_uses IS NULL OR max_uses > 0),
    CHECK (use_count >= 0),
    CHECK (max_uses IS NULL OR use_count <= max_uses)
);

CREATE INDEX idx_group_invites_token_hash ON group_invites(token_hash);
CREATE INDEX idx_group_invites_group_id ON group_invites(group_id);
