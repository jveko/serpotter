# Observability: Logs + request_log Design

**Date:** 2026-07-30  
**Status:** Approved for implementation planning  
**Scope:** Process logs (TraceLayer + request_id correlation), durable `request_log` enrichment (schema v12)  
**Approach:** 2 — deep product instrumentation  
**Research:** librarian pass 2026-07-30 (TraceLayer/request-id)

## Problem

Serpotter already has pieces of observability but they do not form a coherent ops story:

1. **Process logs are weak.** `TraceLayer::new_for_http()` defaults log full URI (secret-prone), DEBUG-level events (hidden under default `RUST_LOG=info`), and no `request_id` span field. Layer order has Propagate outside Trace so response-side Trace hooks miss `x-request-id`.
2. **request_log is thin.** Columns are path/method/status/service/provider_used/duration/error_kind/query_preview only. No `request_id` correlation with JSON logs, no `token_name`, no strategy/attempts, no key/node forensics. Admin list is `limit` only (client substring filter). Insert failures are silent.
## Goals

1. **Correlate** one client call across JSON logs and SQLite audit via `x-request-id` / `request_id`.
2. **Deepen process logs** with safe MakeSpan fields, fixed layer order, `#[instrument]` on product hot paths, per-attempt `key_id`/`node_id` on spans.
3. **Enrich request_log** (schema **v12**) with correlation + Approach-2 audit columns including last-success/last-attempt `key_id`/`node_id` via non-wire product `ExecMeta`.
4. Keep wire REST/MCP response shapes unchanged (no serde exposure of ExecMeta).

## Non-goals (v1)

- Encrypting request_log or full body capture
- Inventing a single “true winner” for every hybrid leg beyond last-success/last-attempt rule
- New env knobs beyond existing `ADMIN_SECRET` / `LOG_FORMAT` / `RUST_LOG`

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Scope | Two layers: logs + request_log |
| Approach | **2** — deep product instrumentation |
| Id path on audit | **A** — non-wire product `ExecMeta`; last **success** else last **attempt** `key_id`/`node_id` |
| Product return shape | **`ProductOutcome { result, meta: ExecMeta }`** only (explicit; no thread-local, no out-param) |
| `providers_consulted` storage | Comma-separated, **no spaces**, **first-seen** order (dedupe) |
| Path list filter | **Prefix** — SQL `path LIKE ?` with bound `path || '%'` |
| MCP `token_name` | **Always** `get_token_by_value` on the tok- (same as REST); never leave NULL when token valid |
| Schema | Additive request_log columns; **`EXPECTED_SCHEMA_VERSION = 12`** |
| Wire DTOs | Unchanged (ExecMeta never serde) |

## Architecture

```text
HTTP
  SetRequestId (outer)
    → custom TraceLayer (MakeSpan: method, path, request_id)
      → PropagateRequestId (inner)
        → handlers

API shell
  require_api_token → TokenRow { id, name }
  product free-fn → (Response | Error) + ExecMeta
  spawn_log(request_id, token_name, ExecMeta fields, …)

Product / KeyPool / ProxyPool / providers
  #[instrument] per attempt: key_id, node_id, service, attempt, outcome
  ProxyHold::node_id() accessor
  ExecMeta updated each attempt (last success wins; else last attempt)

Readouts
  stdout JSON (LOG_FORMAT=json)     — why
  request_log + admin SPA           — which
```

### Complementary roles

| Layer | Question | Cardinality |
| --- | --- | --- |
| request_log | Which product calls happened? | One row per client call |
| tracing JSON / spans | Why did this fail / which attempt? | Unbounded attrs OK |

## §2 Process logs

### Layer order (fix)

Axum applies last `.layer()` as outermost. Required request path: Set → Trace → …; response path must run Propagate **before** Trace sees the response.

```rust
router
  .layer(PropagateRequestIdLayer::x_request_id()) // inner
  .layer(custom_trace_layer)
  .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid)); // outer
```

Today’s order has Propagate outside Trace (response-side bug vs tower-http docs).

### MakeSpan / OnResponse

- Fields: `method`, **`path` only** (not full URI), `request_id` from `RequestId` extension (fallback header).
- **Never** mint a second UUID in MakeSpan.
- **Never** `include_headers(true)` or default full-URI MakeSpan (Authorization / query secrets).
- Raise OnRequest/OnResponse to **INFO** so default EnvFilter shows them.
- OnResponse records `status` + latency; 5xx still hit on_response under `new_for_http`.

### Instrumentation

- API product handlers: `#[instrument(skip_all)]` short names (`search`, `extract`, `research`); inherit HTTP parent span.
- Provider attempt loops: child spans/events with `key_id`, `node_id`, `service`, attempt index, outcome.
- `tokio::spawn` log insert: log failures with `tracing::warn`; use `.in_current_span()` only if useful (insert task need not inherit for correctness).
- Add `ProxyHold::node_id(&self) -> i64` delegating to `lease.node_id`.

## §3 request_log (schema v12 + API)

### Migration `0012_request_log_observability.sql`

```sql
ALTER TABLE request_log ADD COLUMN request_id TEXT;
ALTER TABLE request_log ADD COLUMN token_name TEXT;
ALTER TABLE request_log ADD COLUMN strategy TEXT;
ALTER TABLE request_log ADD COLUMN providers_consulted TEXT;
ALTER TABLE request_log ADD COLUMN attempt_count INTEGER;
ALTER TABLE request_log ADD COLUMN key_id INTEGER;
ALTER TABLE request_log ADD COLUMN node_id INTEGER;

CREATE INDEX IF NOT EXISTS idx_request_log_request_id ON request_log(request_id);

UPDATE schema_version SET version = 12 WHERE id = 1;
```

- `EXPECTED_SCHEMA_VERSION = 12` in `serpotter-db`.
- Existing rows keep NULL new columns.
- Bump all schema hardcodes in tests/docs/AGENTS (root AGENTS still claims v10 in places — fix as part of this work).

### ExecMeta (product, non-wire) — path A

```text
ProductOutcome<T> { result: T, meta: ExecMeta }   // Ok path
// Err path: map error + best-effort ExecMeta accumulated so far

ExecMeta {
  strategy: Option<String>,           // single|hybrid|blend|verify
  providers_consulted: Vec<String>,   // first-seen order; SQL join = comma-separated, no spaces
  attempt_count: u32,
  key_id: Option<i64>,                // last success else last attempt
  node_id: Option<i64>,               // same; None if direct
}
```

- Updated on each provider attempt inside product orchestration.
- **Last success** overwrites key/node; if no success, **last attempt** remains.
- `providers_consulted`: append service name on first sight only; persist as `tavily,firecrawl` (no spaces).
- API shells consume **`ProductOutcome { result, meta }`** only — no thread-local, no out-param, no serde on wire DTOs.
- Wire `SearchResponse` / extract / research JSON **unchanged**.

### API shell

- `require_api_token` → `Result<TokenRow, Response>` (stop discarding row).
- Read request id from `RequestId` extension or `x-request-id`.
- Extend `spawn_log` / `spawn_log_db` / `Db::insert_request_log` / `RequestLogRow` / admin `LogOut` with new fields (camelCase on wire: `requestId`, `tokenName`, `strategy`, `providersConsulted`, `attemptCount`, `keyId`, `nodeId`).
- Insert failure: `tracing::warn!(error = %e, "insert_request_log failed")`; never fail the client path.

### Admin list filters

`GET /api/request-logs` query (camelCase):

| Param | Semantics |
| --- | --- |
| `limit` | 1..=200 (default 50) — existing |
| `status` | optional exact status |
| `path` | optional **prefix** — `path LIKE :prefix || '%'` |
| `service` | optional exact service |
| `requestId` | optional exact |

Server-side filter; SPA uses query params instead of only client substring (client filter may remain secondary).

### MCP

- **Always** resolve `token_name` via `get_token_by_value` (same lookup as REST `require_api_token`); valid tok- ⇒ non-NULL name (empty string only if token row has empty name).
- Set `request_id` from the inbound HTTP request when present.
- Existing `/mcp/...` path labels in request_log stay.

## §5 SPA, tests, docs

### Admin SPA

- Request logs panel: server query params for filters; display new columns (`requestId`, `tokenName`, `strategy`, `keyId`, `nodeId`, `attemptCount`, `providersConsulted`) with tabular nums where appropriate.

### Tests

- migrate → schema version **12**
- insert/list request_log with new columns; filter query params
- Search/extract still fire-and-forget log; wire JSON has no ExecMeta fields
- Response includes `x-request-id` (smoke)
- No live vendor network in CI

### Docs / SoT

- `docs/ops/api.md` — request-logs filters, new log fields
- Crate + root `AGENTS.md` — schema **12**, observability summary

### Error handling

- request_log insert best-effort + warn
- ExecMeta absence on early validation errors → NULL key/node/strategy as appropriate; still log request_id + token_name + status

## Implementation sketch (for planning, not code)

1. Schema v12 + db methods + tests
2. ExecMeta + ProxyHold::node_id + product attempt updates
3. API: TokenRow auth, spawn_log fields, filters
4. Trace layer order + MakeSpan
5. Admin SPA log columns/filters
6. Docs + AGENTS SoT
7. Workspace `cargo test` + clippy gate

## Spec self-review

| Check | Result |
| --- | --- |
| Placeholders / TBD | **Pinned:** path = prefix; `providers_consulted` = CSV no spaces first-seen; `ProductOutcome { result, meta }`; MCP always token lookup |
| Internal consistency | A ExecMeta ↔ v12 columns ↔ no Prom id labels aligned; Approach 2 depth without cardinality blowup |
| Scope | One implementation plan; three layers but one ship unit |
| Ambiguity | None left for plan on the four soft spots above |
| Schema | **12** (not 11); current SoT on disk is v11 |
| Scrape auth cost | Documented: static secret file, sessions unsupported, non-200 → up=0 |

## Next

After user review of this file: **writing-plans** → `docs/superpowers/plans/2026-07-30-observability-logs.md`, then implement.
