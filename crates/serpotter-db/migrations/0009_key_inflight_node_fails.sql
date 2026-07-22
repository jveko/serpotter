-- Shared multi-hold key inflight + node consecutive failure tracking.
ALTER TABLE api_keys ADD COLUMN inflight INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nodes ADD COLUMN consecutive_fails INTEGER NOT NULL DEFAULT 0;
UPDATE schema_version SET version = 9 WHERE id = 1;
