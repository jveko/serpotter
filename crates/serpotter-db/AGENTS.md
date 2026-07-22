# serpotter-db

**Generated:** 2026-07-22 · SQLite SoT

## OVERVIEW

sqlx pool + embedded migrations. `EXPECTED_SCHEMA_VERSION` must match last migration bump (currently **4**).

## STRUCTURE

```
migrations/
  0001_foundation.sql   # schema_version
  0002_tokens.sql       # API bearer tokens (plaintext)
  0003_api_keys.sql     # upstream provider keys
  0004_nodes.sql        # optional outbound proxy nodes
src/lib.rs              # Db methods + connect_and_migrate
tests/migrate.rs        # memory DB integration
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| New table | next `migrations/000N_*.sql` + bump version row + const |
| Token CRUD | `insert_token` / `get_token_by_value` |
| Key acquire | `acquire_api_key` / `acquire_api_keys_batch` |
| Fail disable | `report_api_key_failure` (inactive after 3 fails) |
| Outbound node pick | `select_outbound_node` (least inflight) |

## CONVENTIONS

- `connect_and_migrate`: `:memory:` → `max_connections=1` (shared empty DB trap).
- Raw `sqlx::query` + `?` binds; row types are plain structs (not FromRow macros).
- Personal-use: tokens/api_keys stored **plaintext**.

## ANTI-PATTERNS

- Do not bump schema const without migration SQL and `/ready` expectations.
- Do not use multi-connection pools against `sqlite::memory:` in tests.
- Do not put HTTP/routing logic in this crate.
