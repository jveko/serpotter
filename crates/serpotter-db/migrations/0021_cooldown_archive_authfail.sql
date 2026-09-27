-- Failure-class observability storage: the three durable facts a rate-limited,
-- auth-failing or vendor-banned key leaves behind. Purely additive — 0020
-- dropped `api_keys.email`, and the `api_keys` row shape otherwise stays put,
-- so every 0020-era binary that still runs against this schema is unaffected.
--
-- 1. `cooldown_until` — a vendor 429 is NOT a key failure: the credential is
--    fine, the account's rate budget is spent. So a 429 must never burn a fail
--    (that would disable a healthy key three rate-limit windows from now) and
--    must never revoke it. Instead the acquire ordering demotes a key whose
--    `cooldown_until > datetime('now')` — a demote TIER, not a filter: a
--    cooling key is still servable when nothing better exists, which is what
--    keeps a whole-service 429 from turning into a `NoHealthyKey` 503.
--
--    The column is written once per observed 429 (`Retry-After`, or a bounded
--    fallback) and is NEVER cleared afterwards — deliberately, not an
--    oversight. Nothing sweeps it: no cron, no re-enable path, no rotation.
--    A stale past timestamp is inert rather than wrong, because the only
--    reader is the `> datetime('now')` comparison — once now has passed the
--    mark the key simply stops demoting, and there is no state to reconcile.
--    A cleanup job would buy nothing but write amplification on a table that is
--    already touched by every acquire's ORDER BY.
--
--    The column is declared `TEXT`, matching every other timestamp in the
--    schema (`0003` `last_used_at`/`created_at`, `0006`/`0010`
--    `lease_until`, `0014` `disabled_at`, `0019` `lease_until`, and this
--    table's own `archived_at`). Deliberately NOT `DATETIME`: that spelling
--    carries NUMERIC affinity, so the `>` above would compare as a string only
--    because SQLite cannot losslessly coerce 'YYYY-MM-DD HH:MM:SS' to a
--    number — an accident the ONE predicate this column exists for would
--    depend on. TEXT makes that comparison text by declaration, not by luck.
--
-- 2. `api_keys_archive` — a dead key is a real operational event (rotation
--    cadence, spend recovery, "how many accounts did this vendor kill"), and
--    today the row is simply `active = 0` forever. The archive records the
--    tombstone at ban time so the admin surface can show WHAT happened to a
--    row that no longer earns its place in the pool.
--
--    It stores a FINGERPRINT (`key_fingerprint`), never `api_keys.key` text.
--    That is the whole point of the table: the ban record has to outlive the
--    row and stay safe to render in an admin list, a log line and a bug
--    report. A `key` column here would turn a vendor's "this account is dead"
--    response into a credential that survives deletion of the parent row, and
--    archival is exactly the path that deletes the parent. `reason` is a
--    vocabulary ('vendor_banned', 'auth_fail', …) — never free text from an
--    upstream error body, for the same reason.
--
-- 3. The `auth_fail` backfill. The failure arm now stamps
--    `disabled_reason = 'auth_fail'` in the SAME UPDATE that flips
--    `active = 0` (62c3ec6, at schema 20), so every fail@3 disable from
--    schema 20 onward explains itself. Three groups were left behind by that
--    cutover; (a) and (c) share the NULL signature, (b) does not:
--
--    a) Rows disabled while 0018-0020 were live, by code that had no
--       stamping. These are the residue this UPDATE targets:
--       `active = 0 AND disabled_reason IS NULL AND consecutive_fails >= 3`.
--    b) Rows 0018's own backfill labelled. It set `'manual'` on EVERY
--       pre-18 inactive row (`0018_key_disabled_reason.sql:35-38`), so a
--       genuine pre-18 fail@3 is already `'manual'` and can never present as
--       NULL. Those rows are OUT OF SCOPE here and stay `'manual'` — the
--       fail count still distinguishes them for triage, but guessing that
--       they were auth failures would be unfounded; a `'manual'` row could
--       equally be an operator toggle.
--    c) Rows a re-enable path cleared to NULL and a later fail@3 re-disabled
--       before the stamp existed. Same signature as (a), same fix.
--
--    Relabelling (a)+(c) makes the disposition column uniformly trustworthy,
--    so admin triage stops inferring a cause from a fail count.
--
--    The `disabled_reason IS NULL` guard is the load-bearing half: it can
--    only fill a gap, never overwrite a recorded cause. That keeps group (b)
--    `'manual'`, and — the reason the guard exists — keeps the
--    `'vendor_suspended'` markers 0018 wrote, whose revival the re-enable cron
--    must keep skipping, from being downgraded to a reason the cron does NOT
--    skip (a dead vendor account would return to rotation to be re-attempted
--    and re-401'd forever).
ALTER TABLE api_keys ADD COLUMN cooldown_until TEXT;

CREATE TABLE api_keys_archive (
    id INTEGER PRIMARY KEY,
    api_key_id INTEGER NOT NULL,
    service TEXT NOT NULL,
    key_fingerprint TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL,               -- 'vendor_banned' etc; NEVER the key text
    consecutive_fails INTEGER NOT NULL DEFAULT 0,
    credits_remaining INTEGER,
    archived_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Legacy fail@3 rows: inactive + NULL reason + fails >= 3 is the documented
-- fail@3 signature (serpotter-db/AGENTS.md disposition rule).
UPDATE api_keys SET disabled_reason = 'auth_fail'
 WHERE active = 0 AND disabled_reason IS NULL AND consecutive_fails >= 3;

-- Version stamp: EVERY migration ends with this (0002..0020 pattern) —
-- without it schema_version stays 20, /ready 503s and migrate.rs fails.
UPDATE schema_version SET version = 21 WHERE id = 1;
