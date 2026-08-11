-- Track when a node was last disabled so the maintenance cron can auto
-- re-enable stale disabled nodes after a recovery window (keys parity via
-- reenable_stale_keys). NULL = enabled or never disabled.
ALTER TABLE nodes ADD COLUMN disabled_at TEXT;

UPDATE schema_version SET version = 14 WHERE id = 1;
