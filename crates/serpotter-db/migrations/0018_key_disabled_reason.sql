-- Distinguish WHY a key row is inactive. Before this, `suspend_api_key`
-- (vendor ban, soft tier) and a manual/admin disable both landed as a bare
-- `active = 0`, so the 24h maintenance cron (`reenable_stale_keys`) revived
-- vendor-deactivated accounts indiscriminately: Tavily answers
-- `401 "The account associated with this API key has been deactivated"` with
-- disposition `suspended`, the row comes back after KEY_REENABLE_AFTER_HOURS,
-- gets attempted again, 401s again, and the cycle repeats forever — a
-- permanent attempt tax on every request that reaches the key. Firecrawl
-- escapes this only because its disposition is `deleted` (row removed).
--
-- `disabled_reason`: NULL = never disabled (or re-enabled); 'vendor_suspended'
-- = the vendor told us the account is dead; 'manual' = operator toggle or a
-- fail@3 auth hard-disable. `reenable_stale_keys` skips 'vendor_suspended'
-- only; an operator can still force any row back with `set_api_key_active`,
-- which clears the reason.
ALTER TABLE api_keys ADD COLUMN disabled_reason TEXT;

-- Backfill the rows this migration is about to stop reviving. The discriminator
-- is `consecutive_fails`: `report_api_key_failure` bumps it and disables at the
-- max, so a fail@3 auth-disabled row is `active = 0 AND consecutive_fails >= 3`
-- and its revival IS the recovery path — it must not be marked. `suspend_api_key`
-- deliberately bumps no fails ("the disposition is the suspension itself"), so
-- an inactive non-firecrawl row with fails below the max is a vendor
-- deactivation. Firecrawl keeps its own semantics (bans are deletes), so its
-- inactive rows are left alone.
UPDATE api_keys
   SET disabled_reason = 'vendor_suspended'
 WHERE active = 0
   AND consecutive_fails < 3
   AND service IN ('tavily', 'exa', 'xai');

-- Everything else that is already inactive keeps behaving exactly as before
-- (the cron still revives it): label it 'manual' rather than leaving a NULL
-- that would be indistinguishable from "never disabled".
UPDATE api_keys
   SET disabled_reason = 'manual'
 WHERE active = 0
   AND disabled_reason IS NULL;

UPDATE schema_version SET version = 18 WHERE id = 1;
