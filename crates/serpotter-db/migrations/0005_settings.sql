-- Durable process settings (KV). Seed social_enabled for admin/research.
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT OR IGNORE INTO settings (key, value) VALUES ('social_enabled', 'true');

UPDATE schema_version SET version = 5 WHERE id = 1;
