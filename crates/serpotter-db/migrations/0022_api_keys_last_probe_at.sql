-- Daily key health probe: when was this key last probed?
-- NULL = never probed = due on the first pass. TEXT, not DATETIME: the only
-- reader is `last_probe_at < date('now')`, which needs the date string's
-- TEXT affinity to compare losslessly (DATETIME is NUMERIC — see 0021).
ALTER TABLE api_keys ADD COLUMN last_probe_at TEXT;

-- Manual schema_version bump (house pattern; a missed bump 503s /ready).
UPDATE schema_version SET version = 22 WHERE id = 1;
