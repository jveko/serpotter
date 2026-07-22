CREATE TABLE IF NOT EXISTS request_log (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    path TEXT NOT NULL,
    method TEXT NOT NULL DEFAULT 'POST',
    status INTEGER NOT NULL,
    service TEXT,
    provider_used TEXT,
    duration_ms INTEGER,
    error_kind TEXT,
    query_preview TEXT
);
CREATE INDEX IF NOT EXISTS idx_request_log_created ON request_log(created_at);
CREATE INDEX IF NOT EXISTS idx_request_log_service ON request_log(service);

UPDATE schema_version SET version = 7 WHERE id = 1;
