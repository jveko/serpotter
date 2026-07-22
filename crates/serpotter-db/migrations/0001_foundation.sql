-- Foundation schema marker for Serpotter readiness checks.
CREATE TABLE IF NOT EXISTS schema_version (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL
);

INSERT INTO schema_version (id, version) VALUES (1, 1)
ON CONFLICT(id) DO UPDATE SET version = excluded.version;
