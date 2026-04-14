-- Add connection_state and state_updated_at columns (nullable initially for backfill)
ALTER TABLE pulsoid_connections ADD COLUMN connection_state TEXT;
ALTER TABLE pulsoid_connections ADD COLUMN state_updated_at TIMESTAMPTZ;

-- Backfill existing rows. last_error is NOT terminal in practice (the worker
-- overwrites it on every transient disconnect), so we can't use it to detect
-- past terminal failures. Any row that was in a "blocked" state in a dev DB
-- will re-surface naturally the next time the worker attempts a refresh.
UPDATE pulsoid_connections SET
  connection_state = CASE
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
