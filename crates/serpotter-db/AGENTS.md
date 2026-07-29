# serpotter-db

**Updated:** 2026-07-30 · SQLite SoT (multi-module `Db`)

## OVERVIEW

sqlx pool + embedded migrations. One `Db` type; domain methods live in sibling modules via `impl Db`. `EXPECTED_SCHEMA_VERSION` must match last migration bump (currently **11**).

## STRUCTURE

```
migrations/
  0001_foundation.sql … 0011_node_protocol.sql   # schema_version row per bump
src/
├── lib.rs              # Db, connect_and_migrate, consts (KEY_/NODE_HOLD_TTL, MAX fails)
├── error.rs            # DbError
├── keys/               # acquire_report, admin_crud, rows
├── nodes.rs            # outbound node acquire/report/reclaim
├── tokens.rs
├── settings.rs
├── stats.rs
├── request_log.rs
└── admin_auth.rs       # admin_users + admin_sessions
tests/
└── migrate.rs          # memory DB integration (schema + SQL contracts)
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| New table | next `migrations/000N_*.sql` + bump version row + const |
| Token CRUD | `insert_token` / `get_token_by_value` |
| Key acquire (shared) | `acquire_api_key_shared(service, max_inflight, hold_ttl_secs, unknown_credit_weight)` — exhausted last, score `(C*1000)/(inflight+1)`; success soft-burns non-NULL credits −1 |
| Key reclaim / hygiene | `reclaim_expired_key_holds` / `zero_all_key_inflight` / `release_api_key_inflight` |
| Report multi-hold | success/fail/exhausted also multi-hold-safe inflight--; clear `lease_until` only when last hold ends |
| Fail disable | `report_api_key_failure` (inactive after 3 fails) |
| Credit fields | `update_api_key_usage` for admin sync |
| Outbound node pick | `acquire_outbound_node` / `acquire_outbound_node_with_ttl` (reclaim expired + least-inflight + stamp lease) + `NODE_HOLD_TTL_SECS=90` |
| Node health | `report_node_success` / `report_node_failure(id, max_fails, last_error)` / `set_node_enabled` (re-enable clears fails+last_error) / `reclaim_expired_node_holds` / `release_node_inflight` / `zero_all_node_inflight` (clears lease) |
| Request log | `insert_request_log` / `purge_request_log` / `count_request_logs` |
| Re-enable keys | `reenable_stale_keys(hours)` for inactive + stale last_used_at |
| Per-service stats | `stats_by_service` |
| Admin auth | `insert_admin_user` / `get_admin_user_by_username` / sessions |

## CONVENTIONS

- `connect_and_migrate`: `:memory:` → `max_connections=1` (shared empty DB trap).
- Raw `sqlx::query` + `?` binds; row types are plain structs (not FromRow macros).
- Personal-use: tokens/api_keys stored **plaintext**.
- Shared holds: `api_keys.inflight` + `lease_until` as hold expiry for reclaim (not exclusive mutex).

## ANTI-PATTERNS

- Do not bump schema const without migration SQL and `/ready` expectations.
- Do not use multi-connection pools against `sqlite::memory:` in tests.
- Do not put HTTP/routing logic in this crate.
