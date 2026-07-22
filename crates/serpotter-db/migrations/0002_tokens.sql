-- API bearer tokens (plaintext at rest; personal-use threat model).
CREATE TABLE IF NOT EXISTS tokens (
    id INTEGER PRIMARY KEY,
    token TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

UPDATE schema_version SET version = 2 WHERE id = 1;
