-- Soft lease: exclude in-flight keys until lease_until or report clears.
ALTER TABLE api_keys ADD COLUMN lease_until TEXT;

UPDATE schema_version SET version = 6 WHERE id = 1;
