-- Request events wave: drop request_log (raw per-request events live only in
-- the JSON log stream); widen usage_daily with key/token dims so spend-per-key
-- survives without the raw table. PK change requires a table rebuild; old
-- aggregate rows migrate with sentinel key_id=0/token_name='' (they were
-- service-level aggregates, so per-key history is not recoverable).

DROP TABLE request_log;  -- its 6 indexes drop with the table (SQLite)

CREATE TABLE usage_daily_new (
    service TEXT NOT NULL,
    provider_used TEXT NOT NULL,
    date TEXT NOT NULL,
    key_id INTEGER NOT NULL DEFAULT 0,      -- sentinel: unknown key
    token_name TEXT NOT NULL DEFAULT '',    -- sentinel: unknown token
    requests INTEGER NOT NULL DEFAULT 0,
    successes INTEGER NOT NULL DEFAULT 0,
    errors INTEGER NOT NULL DEFAULT 0,
    tokens INTEGER NOT NULL DEFAULT 0,
    cost REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (service, provider_used, date, key_id, token_name)
);
INSERT INTO usage_daily_new (service, provider_used, date, requests, successes, errors, tokens, cost)
    SELECT service, provider_used, date, requests, successes, errors, tokens, cost
    FROM usage_daily;
DROP TABLE usage_daily;
ALTER TABLE usage_daily_new RENAME TO usage_daily;
CREATE INDEX IF NOT EXISTS idx_usage_daily_date ON usage_daily(date);

UPDATE schema_version SET version = 17 WHERE id = 1;
