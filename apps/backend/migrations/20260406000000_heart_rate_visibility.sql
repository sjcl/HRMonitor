-- Controls heart rate data visibility.
-- 'group_default' = follow the group's visibility settings (once groups
-- are implemented; until then, only self can view).
-- 'private' = always hidden regardless of group settings (only self can view).
ALTER TABLE users
  ADD COLUMN heart_rate_visibility TEXT NOT NULL DEFAULT 'group_default'
  CHECK (heart_rate_visibility IN ('group_default', 'private'));
