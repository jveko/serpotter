-- Data-layer hygiene: drop the dead `api_keys.email` column, index the
-- maintenance-cron scans, and make the declared foreign keys load-bearing
-- rather than decorative.
--
-- 1. `email` (0003:7) has had zero readers and zero writers since it was
--    created, so it only widens every api_keys row. The embedded SQLite
--    (libsqlite3-sys) is >= 3.35, so `DROP COLUMN` applies directly — the
--    column is not a PRIMARY KEY / UNIQUE column, is not indexed, and is not
--    referenced by any view, trigger, generated column or index WHERE, each
--    of which would block the drop.
--
-- 2. Support indexes. The hot acquire/reclaim transaction after schema 19 is
--      DELETE FROM api_key_leases WHERE lease_until <= datetime('now')
--    (acquire_report.rs) plus its node twin, and 0019 ALREADY indexes both
--    child tables on `lease_until` and on their parent id, so the hot path
--    needs nothing new. The parent mirror columns `api_keys.lease_until` /
--    `nodes.lease_until` are deliberately NOT indexed: since the holder-set
--    cutover no query filters on them, and indexing a column that every
--    acquire and release rewrites buys no reader.
--
--    The remaining gap is `Db::reenable_stale_keys` (admin_crud.rs), a
--    full-table UPDATE:
--      WHERE active = 0
--        AND disabled_reason IS NOT 'vendor_suspended'
--        AND last_used_at IS NOT NULL
--        AND last_used_at < datetime('now', '-' || ? || ' hours')
--    `idx_api_keys_service_active` cannot serve it: with no service equality
--    the leading column is open, and only the disabled minority is wanted. So
--    the index is keyed on the RANGE column with the cheap equality in its
--    WHERE: `EXPLAIN QUERY PLAN` then reports a real range seek
--    `SEARCH api_keys USING INDEX idx_api_keys_reenable (last_used_at>? AND
--    last_used_at<?)` instead of the bare `SCAN api_keys` it falls back to.
--    Keying it on `active, disabled_reason, last_used_at` (the obvious
--    "match the WHERE in order" shape) instead degrades to
--    `SEARCH ... (active=?)`, which still walks every disabled row — the same
--    criticism this index exists to fix. The remaining predicates stay out of
--    the index on purpose: folding `IS NOT 'vendor_suspended'` or
--    `IS NOT NULL` in risks an implication the planner cannot prove (silent
--    fall-back to the full scan), and it would make every revival rewrite an
--    index entry keyed on the very columns the UPDATE mutates.
--    `tests/migrate.rs` pins the plan with EXPLAIN QUERY PLAN against the
--    bound-parameter statement the cron actually runs.
--
--    `reenable_stale_nodes` (nodes.rs) needs nothing: 0004's plain
--    `idx_nodes_enabled` already plans it as a SEARCH on `enabled = 0`, so a
--    second index there would never be chosen.
--
-- 3. Foreign keys. sqlx 0.9 already opens every connection with
--    `PRAGMA foreign_keys = ON` (sqlx-sqlite options/mod.rs:187), so the
--    REFERENCES in 0008/0019 have always been enforced on THIS stack; 0019's
--    header comment claiming otherwise is wrong, but an applied migration
--    must never be edited (sqlx checksums it, so boot would fail), hence the
--    correction here and in `connect_and_migrate`'s docs.
--    `connect_and_migrate` now PINS the pragma explicitly (and busy_timeout)
--    so a driver default change can never silently relax it. The DELETEs below
--    are a hygiene net, not a boot blocker: SQLite does not retro-validate
--    existing rows when the pragma is turned on, only subsequent DML, so a
--    database written by a non-sqlx client (the sqlite3 CLI, an operator
--    script, a restored backup) can still hold a dangling row. A clean
--    database deletes nothing.
--
-- 4. `admin_sessions.user_id` has no ON DELETE clause and nothing in the
--    codebase deletes an `admin_users` row, so enforcement is free today;
--    whoever adds a user-delete path must clear or cascade the sessions
--    first, or the delete hard-errors as an FK violation.
ALTER TABLE api_keys DROP COLUMN email;

CREATE INDEX idx_api_keys_reenable ON api_keys(last_used_at) WHERE active = 0;

DELETE FROM admin_sessions
 WHERE user_id NOT IN (SELECT id FROM admin_users);
DELETE FROM api_key_leases
 WHERE api_key_id NOT IN (SELECT id FROM api_keys);
DELETE FROM node_leases
 WHERE node_id NOT IN (SELECT id FROM nodes);

UPDATE schema_version SET version = 20 WHERE id = 1;
