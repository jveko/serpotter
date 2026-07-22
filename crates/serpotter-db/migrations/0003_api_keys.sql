-- Upstream provider API keys (plaintext at rest; personal-use).
CREATE TABLE IF NOT EXISTS api_keys (
    id INTEGER PRIMARY KEY,
    service TEXT NOT NULL DEFAULT 'tavily',
    key TEXT UNIQUE NOT NULL,
    key_fingerprint TEXT,
    email TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    consecutive_fails INTEGER NOT NULL DEFAULT 0,
    last_used_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    credits_remaining INTEGER,
    credits_limit INTEGER,
    usage_synced_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_api_keys_service ON api_keys(service);
CREATE INDEX IF NOT EXISTS idx_api_keys_service_active ON api_keys(service, active);

UPDATE schema_version SET version = 3 WHERE id = 1;
