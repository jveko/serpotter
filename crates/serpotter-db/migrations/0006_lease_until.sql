-- Hold reclaim deadline column (later multi-hold: lease_until = shared expiry, not exclusive mutex).
ALTER TABLE api_keys ADD COLUMN lease_until TEXT;

UPDATE schema_version SET version = 6 WHERE id = 1;
