CREATE SEQUENCE pulsoid_revision_seq;

-- Seed the sequence past any existing revision.
-- setval(seq, val, false) means the *next* nextval() returns exactly val.
-- Empty table → next nextval() returns 1; non-empty → returns MAX + 1.
SELECT setval(
    'pulsoid_revision_seq',
    COALESCE((SELECT MAX(revision) + 1 FROM pulsoid_connections), 1),
    false
);

ALTER TABLE pulsoid_connections
  ALTER COLUMN revision SET DEFAULT nextval('pulsoid_revision_seq');
