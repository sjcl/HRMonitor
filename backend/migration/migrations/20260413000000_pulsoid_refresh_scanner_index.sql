-- no-transaction
CREATE INDEX CONCURRENTLY idx_pulsoid_connections_refresh_scan
    ON pulsoid_connections (token_expires_at ASC)
    INCLUDE (user_id, revision)
    WHERE source = 'oauth'
      AND connection_state != 'error'
      AND token_expires_at IS NOT NULL;
