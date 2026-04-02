-- Drop legacy column now that migration to pulsoid_connections is complete
ALTER TABLE users DROP COLUMN IF EXISTS pulsoid_access_token;
