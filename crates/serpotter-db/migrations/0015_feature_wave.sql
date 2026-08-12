-- Wave 3A feature-wave storage (additive only): B1 exact-query TTL cache,
-- B2 request_log token+cost columns, B6 usage_daily rollup table,
-- B16 async provider_jobs, B23 per-key budget caps (columns land now, gates next wave).

-- B1: exact-query TTL response cache. key_hash is the service-aware content
-- hash minted by the product layer; PRIMARY KEY is the hash per the DDL contract.
CREATE TABLE IF NOT EXISTS query_cache (
    service TEXT NOT NULL,
    key_hash TEXT NOT NULL PRIMARY KEY,
    response_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_query_cache_expires_at ON query_cache(expires_at);

-- B2: request_log token/cost observability (all nullable; request_mode
-- 'oneshot'|'stream'|NULL=unknown; ttft_ms filled by the TTFT wave).
ALTER TABLE request_log ADD COLUMN input_tokens INTEGER;
ALTER TABLE request_log ADD COLUMN output_tokens INTEGER;
ALTER TABLE request_log ADD COLUMN total_tokens INTEGER;
ALTER TABLE request_log ADD COLUMN cost_est REAL;
ALTER TABLE request_log ADD COLUMN ttft_ms REAL;
ALTER TABLE request_log ADD COLUMN request_mode TEXT;

-- B13: token_name is now a first-class list filter.
CREATE INDEX IF NOT EXISTS idx_request_log_token_name ON request_log(token_name);

-- B6: daily usage rollup source for the admin usage dashboard.
CREATE TABLE IF NOT EXISTS usage_daily (
    service TEXT NOT NULL,
    provider_used TEXT NOT NULL,
    date TEXT NOT NULL,
    requests INTEGER NOT NULL DEFAULT 0,
    successes INTEGER NOT NULL DEFAULT 0,
    errors INTEGER NOT NULL DEFAULT 0,
    tokens INTEGER NOT NULL DEFAULT 0,
    cost REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (service, provider_used, date)
);
CREATE INDEX IF NOT EXISTS idx_usage_daily_date ON usage_daily(date);

-- B16: async job rows (id minted by the API layer; status running|done|failed).
CREATE TABLE IF NOT EXISTS provider_jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    service TEXT NOT NULL,
    params_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    result_json TEXT,
    error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_provider_jobs_expires_at ON provider_jobs(expires_at);

-- B23: per-key budget caps (NULL = unlimited). Storage lands now; gating lands
-- in the next wave (J4).
ALTER TABLE api_keys ADD COLUMN budget_daily REAL;
ALTER TABLE api_keys ADD COLUMN budget_monthly REAL;

UPDATE schema_version SET version = 15 WHERE id = 1;
