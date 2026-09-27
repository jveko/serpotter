# serpotter-db

**Updated:** 2026-08-12 · SQLite SoT (multi-module `Db`)

## OVERVIEW

sqlx pool + embedded migrations. One `Db` type; domain methods live in sibling modules via `impl Db`. `EXPECTED_SCHEMA_VERSION` must match last migration bump (currently **20**).

## STRUCTURE

```
migrations/
  0001_foundation.sql … 0020_hygiene.sql      # schema_version row per bump
src/
├── lib.rs              # Db, connect_and_migrate, consts (KEY_/NODE_HOLD_TTL, MAX fails)
├── error.rs            # DbError
├── cache.rs            # B1 exact-query TTL cache (query_cache)
├── usage.rs            # B6 usage_daily rollup + spend aggregates
├── keys/               # acquire_report, admin_crud, rows
├── nodes.rs            # outbound node acquire/report/reclaim
├── tokens.rs
├── settings.rs
├── stats.rs
└── admin_auth.rs       # admin_users + admin_sessions
tests/
├── migrate.rs          # memory DB integration (schema + SQL contracts)
└── feature_wave.rs     # Wave 3A storage contracts (cache/usage/jobs/pagination/budgets)
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| New table | next `migrations/000N_*.sql` + bump version row + const |
| Token CRUD | `insert_token` / `get_token_by_value` |
| Key acquire (shared) | `acquire_api_key_shared(service, max_inflight, hold_ttl_secs, unknown_credit_weight)` — exhausted last, score `(C*1000)/(inflight+1)`; success soft-burns non-NULL credits −1 |
| Key reclaim / hygiene | `reclaim_expired_key_holds` / `zero_all_key_inflight` / `release_api_key_inflight` |
| Report multi-hold | success/fail/exhausted also multi-hold-safe inflight--; clear `lease_until` only when last hold ends |
| Fail disable | `report_api_key_failure_lease(token)` / `note_key_health_failure(id)` (inactive after 3 fails — same `acquire_report.rs:134`/`:284` SQL) — the `active = 0` flip and the `disabled_reason = 'auth_fail'` stamp share one UPDATE and one `max_fails` bound. The two predicates DIFFER by design: flip = `consecutive_fails + 1 >= ?`; stamp = `disabled_reason IS NULL AND consecutive_fails + 1 >= ?` (mirroring `set_api_key_active`). The `IS NULL` guard keeps a concurrent `finish_suspended` marker from being downgraded to the cron-eligible `auth_fail`; the stamp still implies the flip, so the reason can never fire without the disable |
| Credit fields | `update_api_key_usage` for admin sync |
| B1 response cache | `cache_put(service, key_hash, response_json, ttl_secs)` / `cache_get(service, key_hash)` (expiry checked in SQL) / `purge_expired_cache` |
| B6 usage rollup | `upsert_usage_daily` (additive per-request; fed at write time by `serpotter-api` `events.rs` usage writer) / `usage_summary(days)` / `spend_by_key(days)` / `spend_by_service(days)`. Every `days` goes through the one shared `clamp_usage_days` (1..=`USAGE_MAX_DAYS` = 180); the spend queries also cap grouped rows at `SPEND_MAX_ROWS` |
| Outbound node pick | `acquire_outbound_node` / `acquire_outbound_node_with_ttl` (reclaim expired + least-inflight + stamp lease) + `NODE_HOLD_TTL_SECS=90` |
| Node health | `report_node_success` / `report_node_failure(id, max_fails, last_error)` (disable at max_fails stamps `disabled_at`) / `set_node_enabled` (re-enable clears fails+last_error+disabled_at; disable stamps `disabled_at`) / `reenable_stale_nodes(hours)` (auto re-enable disabled nodes older than `hours`) / `reclaim_expired_node_holds` / `release_node_inflight` / `zero_all_node_inflight` (clears lease) |
| Request events | table dropped (0017); raw events live in `serpotter-api` `events.rs` (log line + in-memory ring + `usage_daily` upsert) |
| Re-enable keys | `reenable_stale_keys(hours)` for inactive + stale `last_used_at`, `hours` clamped to the shared `Db::REENABLE_MIN_HOURS` floor (1) so a `0`/negative `KEY_REENABLE_AFTER_HOURS` can neither disable fail@3 backoff nor form the `--N hours` modifier SQLite reads as NULL (a silent match-nothing no-op). Same floor on the node path (`reenable_stale_nodes`). Skips `disabled_reason = 'vendor_suspended'` **only** — an `'auth_fail'` row returns to rotation after `KEY_REENABLE_AFTER_HOURS`, and the UPDATE clears the reason to `NULL` as it re-activates |
| Per-service stats | `stats_by_service` |
| Admin auth | `insert_admin_user` / `get_admin_user_by_username` / sessions |

**`api_keys.disabled_reason` disposition (schema 18) — the one place to read it:**

| Value | Meaning | Recovered by |
|---|---|---|
| `'manual'` | **operator toggle only** — written by `set_api_key_active(id, false)`, and only when no reason is recorded yet | `reenable_stale_keys` cron + operator toggle |
| `'vendor_suspended'` | permanent vendor deactivation (`suspend_api_key_lease` / `note_key_health_suspended`, e.g. `401 "account … deactivated"`) | **operator only** — the cron skips this reason by design; `set_api_key_active` clears it |
| `'auth_fail'` | **a fail@3 auth hard-disable**: `report_api_key_failure_lease` / `note_key_health_failure` set `active = 0` at `MAX_CONSECUTIVE_FAILURES` and stamp this reason in the same UPDATE — but only when `disabled_reason IS NULL`, so a `vendor_suspended` marker set by a racing leg is never downgraded | `reenable_stale_keys` cron — for these rows the revival *is* the recovery path; the cron deliberately does NOT skip this reason |
| `NULL` | never disabled, or re-enabled (rotation, key swap, cron revival, operator enable — all clear the column) | n/a (row is active) |

Triage: every NEW fail@3 disable carries `'auth_fail'`, so an INACTIVE row with a `NULL` reason and `consecutive_fails >= 3` is a **pre-0021 legacy** fail@3 disable (a later migration backfills exactly those to `'auth_fail'`); an INACTIVE `NULL` row with fewer fails was never disabled by code (e.g. created inactive outside the app). Rows disabled BEFORE schema 18 carry `'manual'` from migration 0018's backfill (`0018_key_disabled_reason.sql:35-38`) regardless of cause — a pre-18 fail@3 row is `'manual'`, NOT NULL — so disambiguate those with `consecutive_fails >= 3` too. The disposition table above stays the source of truth.

**Migration 0018's header clause is SUPERSEDED by the code.** `0018_key_disabled_reason.sql:12-13` says `'manual'` = "operator toggle or a fail@3 auth hard-disable"; the code does not do that — the failure path stamps its own `'auth_fail'` (the `active = 0` flip and the stamp are the same UPDATE). The file is frozen — its checksum is pinned — so the correction lives here and in `keys/rows.rs` instead, the same way `0020_hygiene.sql`'s header notes supersede 0019's rationale. Trust this table and the code, not 0018's prose.

The migration's own body still names the pre-rename `report_api_key_failure`
(`0018_key_disabled_reason.sql:19`) for the same fail@3 bump. It cannot be
edited — `sqlx::migrate!` (`src/lib.rs:110`) checksums applied migrations, so
a byte change fails boot on every existing database — so read it as
`report_api_key_failure_lease` / `note_key_health_failure`, the two functions
that run the SQL it describes.

## CONVENTIONS

- `connect_and_migrate`: `:memory:` → `max_connections=1` (shared empty DB trap); every connection also sets `foreign_keys=ON` and `busy_timeout=SQLITE_BUSY_TIMEOUT_SECS` (5s) explicitly. Migration 0020 deletes orphan `admin_sessions` / lease rows first, so turning enforcement on cannot strand an existing database.
- Raw `sqlx::query` + `?` binds; row types are plain structs (not FromRow macros).
- Personal-use: tokens/api_keys stored **plaintext**.
- Shared holds: `api_keys.inflight` + `lease_until` as hold expiry for reclaim (not exclusive mutex).
- Row structs with `Option<f64>` fields (e.g. `UsageDailyRow.cost`, `SpendKeyRow.cost`) derive `PartialEq`, not `Eq` (f64 is not Eq).

## ANTI-PATTERNS

- Do not bump schema const without migration SQL and `/ready` expectations.
- Do not use multi-connection pools against `sqlite::memory:` in tests.
- Do not put HTTP/routing logic in this crate.