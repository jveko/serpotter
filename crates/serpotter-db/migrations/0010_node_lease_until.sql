-- Node multi-hold reclaim deadline (parity with api_keys.lease_until).
-- lease_until is hold expiry for abandoned inflight reclaim, not exclusive mutex.
ALTER TABLE nodes ADD COLUMN lease_until TEXT;

UPDATE schema_version SET version = 10 WHERE id = 1;
