-- Add connection_state and state_updated_at columns (nullable initially for backfill)
ALTER TABLE pulsoid_connections ADD COLUMN connection_state TEXT;
ALTER TABLE pulsoid_connections ADD COLUMN state_updated_at TIMESTAMPTZ;

-- Backfill existing rows.
-- Use refresh_blocked (added in 20260409000001) as the terminal signal —
-- last_error is NOT terminal in the old schema (the worker overwrites it
-- on every transient disconnect/reconnect), so mapping last_error IS NOT
-- NULL → 'error' would promote recoverable rows to the terminal state and
-- the ingest would never spawn a worker for them again.
UPDATE pulsoid_connections SET
  connection_state = CASE
    WHEN refresh_blocked THEN 'error'
    WHEN last_connected_at IS NOT NULL THEN 'connected'
    ELSE 'pending'
  END,
  state_updated_at = COALESCE(last_connected_at, now());

-- Apply NOT NULL, defaults, and CHECK constraint
ALTER TABLE pulsoid_connections ALTER COLUMN connection_state SET NOT NULL;
ALTER TABLE pulsoid_connections ALTER COLUMN connection_state SET DEFAULT 'pending';
ALTER TABLE pulsoid_connections ALTER COLUMN state_updated_at SET NOT NULL;
ALTER TABLE pulsoid_connections ALTER COLUMN state_updated_at SET DEFAULT now();
ALTER TABLE pulsoid_connections ADD CONSTRAINT chk_connection_state
  CHECK (connection_state IN ('pending', 'connected', 'error'));
