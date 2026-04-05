ALTER TABLE users
  ADD COLUMN heart_rate_visibility TEXT NOT NULL DEFAULT 'group'
  CHECK (heart_rate_visibility IN ('public', 'group', 'private'));
