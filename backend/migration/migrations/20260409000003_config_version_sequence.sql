CREATE SEQUENCE pulsoid_config_version_seq;

-- Seed the sequence past any existing config_version.
-- setval(seq, val, false) means the *next* nextval() returns exactly val.
-- Empty table → next nextval() returns 1; non-empty → returns MAX + 1.
SELECT setval(
    'pulsoid_config_version_seq',
    COALESCE((SELECT MAX(config_version) + 1 FROM pulsoid_connections), 1),
    false
);

ALTER TABLE pulsoid_connections
  ALTER COLUMN config_version SET DEFAULT nextval('pulsoid_config_version_seq');
