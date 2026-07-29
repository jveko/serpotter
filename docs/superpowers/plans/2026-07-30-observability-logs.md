# Observability Logs + request_log Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correlate process logs and durable `request_log` via `request_id`, enrich audit with Approach-2 `ExecMeta` (including last-success/last-attempt key/node).

**Architecture:** Schema v12 additive columns on `request_log`; product returns `ProductOutcome { result, meta: ExecMeta }` (non-wire); API shells pass TokenRow + request_id into extended `spawn_log`; fix TraceLayer/Propagate/SetRequestId order, custom MakeSpan, and `#[instrument]` on product shells + provider_attempt spans.

**Tech Stack:** Rust axum 0.8, tower-http 0.6 (trace, request-id), tracing, sqlx SQLite, admin Vite SPA.

**Spec:** `docs/superpowers/specs/2026-07-30-observability-logs-design.md`

## Global Constraints

- Schema **v12** only (`EXPECTED_SCHEMA_VERSION = 12`); current disk SoT is v11
- Id path **A**: non-wire `ExecMeta`; last **success** else last **attempt** for `key_id`/`node_id`
- Product return: **`ProductOutcome { result, meta }` only** (no thread-local, no out-param)
- `providers_consulted`: comma-separated, **no spaces**, **first-seen** order
- Path filter: **prefix** `path LIKE ?` with bound `prefix || '%'`
- MCP `token_name`: **always** `get_token_by_value` when tok- valid
- Wire REST/MCP response JSON **unchanged** (no ExecMeta serde)
- Never `git commit --no-verify`; conventional commits; `rtk cargo test` / `rtk cargo clippy` when available
- Clean cutover: update every callsite; no dual shims

## File map

| File | Responsibility |
| --- | --- |
| `crates/serpotter-db/migrations/0012_request_log_observability.sql` | Additive columns + index + version 12 |
| `crates/serpotter-db/src/lib.rs` | `EXPECTED_SCHEMA_VERSION = 12` |
| `crates/serpotter-db/src/request_log.rs` | `RequestLogRow` fields; insert; `list_request_logs` filters |
| `crates/serpotter-db/tests/migrate.rs` | schema 12 + insert/list/filter tests |
| `crates/serpotter-product/src/meta.rs` (new) | `ExecMeta`, `ProductOutcome`, helpers |
| `crates/serpotter-product/src/lib.rs` | re-export meta types |
| `crates/serpotter-product/src/hold.rs` | `ProxyHold::node_id()` |
| `crates/serpotter-product/src/search/*`, `extract/*` | accumulate ExecMeta; return ProductOutcome |
| `crates/serpotter-api/src/log_request.rs` | extended spawn_log; warn on insert err |
| `crates/serpotter-api/src/lib.rs` | `require_api_token` → TokenRow |
| `crates/serpotter-api/src/product/*` | use ProductOutcome + TokenRow + request_id |
| `crates/serpotter-api/src/admin/logs.rs` | filters + new LogOut fields |
| `crates/serpotter-api/src/main.rs` | Trace layer order + MakeSpan |
| `crates/serpotter-api/src/mcp/*` | token_name via get_token_by_value; meta logging |
| `apps/admin/src/features/logs/*` | columns + server filters |
| `docs/ops/env.md`, `docs/ops/api.md`, AGENTS.md, `.env.example` | SoT |
| `crates/serpotter-api/tests/*` | logs filters, schema asserts |

**Public shapes after cutover:**

```rust
// serpotter-product
pub struct ExecMeta {
    pub strategy: Option<String>,
    pub providers_consulted: Vec<String>,
    pub attempt_count: u32,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
}
impl ExecMeta {
    pub fn note_attempt(&mut self, service: &str, key_id: i64, node_id: Option<i64>, success: bool);
    pub fn providers_csv(&self) -> Option<String>; // None if empty
}
pub struct ProductOutcome<T> {
    pub result: T,
    pub meta: ExecMeta,
}

// serpotter-db
pub struct RequestLogFilter {
    pub limit: i64,
    pub status: Option<i64>,
    pub path_prefix: Option<String>,
    pub service: Option<String>,
    pub request_id: Option<String>,
}
// insert_request_log gains the seven new optional columns
// list_request_logs(filter: RequestLogFilter) -> Vec<RequestLogRow>

// serpotter-api
pub async fn require_api_token(...) -> Result<serpotter_db::TokenRow, Response>;
```

---

### Task 1: Schema v12 + request_log API (TDD)

**Files:**
- Create: `crates/serpotter-db/migrations/0012_request_log_observability.sql`
- Modify: `crates/serpotter-db/src/lib.rs` (`EXPECTED_SCHEMA_VERSION`)
- Modify: `crates/serpotter-db/src/request_log.rs`
- Modify: `crates/serpotter-db/tests/migrate.rs`
- Modify: any test hardcoding schema version **11** → **12** (grep `EXPECTED_SCHEMA_VERSION` / `schemaVersion` / `== 11`)

**Interfaces:**
- Consumes: existing `request_log` table v7+
- Produces: v12 columns; `RequestLogFilter`; extended insert/list

- [ ] **Step 1: Write failing migrate + filter tests**

In `crates/serpotter-db/tests/migrate.rs`, change `migrate_sets_schema_version_11` to `_12` asserting `12`, and add:

```rust
#[tokio::test]
async fn request_log_v12_columns_and_path_prefix_filter() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    assert_eq!(db.schema_version().await.unwrap(), 12);

    db.insert_request_log(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(12),
        None,
        Some("hello"),
        Some("req-1"),
        Some("local"),
        Some("single"),
        Some("tavily"),
        Some(1),
        Some(7),
        Some(3),
    )
    .await
    .unwrap();
    db.insert_request_log(
        "/api/extract",
        "POST",
        502,
        Some("firecrawl"),
        Some("firecrawl"),
        Some(99),
        Some("Upstream"),
        Some("https://x"),
        Some("req-2"),
        Some("ci"),
        None,
        Some("firecrawl"),
        Some(2),
        Some(8),
        None,
    )
    .await
    .unwrap();

    let rows = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 50,
            status: None,
            path_prefix: Some("/api/se".into()),
            service: None,
            request_id: None,
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_id.as_deref(), Some("req-1"));
    assert_eq!(rows[0].token_name.as_deref(), Some("local"));
    assert_eq!(rows[0].key_id, Some(7));
    assert_eq!(rows[0].node_id, Some(3));

    let by_id = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 10,
            status: None,
            path_prefix: None,
            service: None,
            request_id: Some("req-2".into()),
        })
        .await
        .unwrap();
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].status, 502);
}
```

- [ ] **Step 2: Run test — expect fail**

```bash
rtk cargo test -p serpotter-db --locked request_log_v12 -- --nocapture
```

Expected: FAIL (migration/types missing) or schema still 11.

- [ ] **Step 3: Migration + types + SQL**

Create `crates/serpotter-db/migrations/0012_request_log_observability.sql`:

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

Set `EXPECTED_SCHEMA_VERSION: i64 = 12`.

Extend `RequestLogRow` with the seven fields. Replace insert signature to accept them (all `Option` except existing required). Implement `RequestLogFilter` and `list_request_logs` building dynamic WHERE (only bind provided filters); path uses `path LIKE (prefix || '%')` — in SQLite bind the full pattern `format!("{prefix}%")` as one `?` to avoid concat quirks.

Update existing `request_log_insert_and_purge` and any insert callsites with trailing `None`s for new args **or** introduce a params struct `InsertRequestLog` if argument count is unbearable — prefer a single struct if clippy `too_many_arguments` fires beyond allow.

- [ ] **Step 4: Run db tests**

```bash
rtk cargo test -p serpotter-db --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add crates/serpotter-db
rtk git commit -m "feat(db): request_log observability columns schema v12"
```

---

### Task 2: ExecMeta + ProductOutcome + hold node_id

**Files:**
- Create: `crates/serpotter-product/src/meta.rs`
- Modify: `crates/serpotter-product/src/lib.rs`
- Modify: `crates/serpotter-product/src/hold.rs`
- Modify: `crates/serpotter-product/src/search/mod.rs`, `search/execute.rs`, `search/run_provider.rs`, `extract/extract_url.rs`, `extract/research.rs` (and any re-exports)
- Modify: all API/MCP callers of `search_inner` / `extract_url` / `research_inner` in the **same** commit wave as needed to compile (Task 3 can finish logging; this task must leave workspace compiling — either temporary `.meta` discard at call sites or complete Task 3 immediately after)

**Interfaces:**
- Consumes: `KeyHold::key_id`, `ProxyHold::node_id` (new)
- Produces: `ExecMeta`, `ProductOutcome<T>`, `note_attempt`, `providers_csv`

- [ ] **Step 1: Unit tests for ExecMeta rules**

In `meta.rs` `#[cfg(test)]`:

```rust
#[test]
fn note_attempt_last_success_wins_else_last() {
    let mut m = ExecMeta::default();
    m.note_attempt("tavily", 1, Some(10), false);
    assert_eq!(m.key_id, Some(1));
    assert_eq!(m.attempt_count, 1);
    m.note_attempt("firecrawl", 2, Some(11), true);
    assert_eq!(m.key_id, Some(2));
    m.note_attempt("exa", 3, None, false);
    // last was failure but prior success keeps success ids? Spec: last success else last attempt.
    // After a success, a later failure must NOT wipe success ids — only update if we define
    // "last success wins and is sticky until newer success".
    // LOCKED rule for implementers:
    // - on every attempt: attempt_count++, first-seen provider push
    // - always set key_id/node_id on attempt (last attempt)
    // - ALSO track success_key_id/success_node_id; providers_csv/final key_id prefer success_* if set
    assert_eq!(m.key_id, Some(2)); // sticky last success
    assert_eq!(m.node_id, Some(11));
    assert_eq!(m.providers_csv().as_deref(), Some("tavily,firecrawl,exa"));
}

#[test]
fn all_failures_keep_last_attempt() {
    let mut m = ExecMeta::default();
    m.note_attempt("tavily", 1, None, false);
    m.note_attempt("firecrawl", 2, Some(9), false);
    assert_eq!(m.key_id, Some(2));
    assert_eq!(m.node_id, Some(9));
}
```

Implement sticky last-success as above (internal `success_key_id` or only overwrite key_id on success, and on failure only set if no success yet):

```rust
pub fn note_attempt(&mut self, service: &str, key_id: i64, node_id: Option<i64>, success: bool) {
    self.attempt_count = self.attempt_count.saturating_add(1);
    if !self.providers_consulted.iter().any(|s| s == service) {
        self.providers_consulted.push(service.to_string());
    }
    if success {
        self.key_id = Some(key_id);
        self.node_id = node_id;
        self.had_success = true;
    } else if !self.had_success {
        self.key_id = Some(key_id);
        self.node_id = node_id;
    }
}
```

- [ ] **Step 2: Run product tests for meta**

```bash
rtk cargo test -p serpotter-product --locked note_attempt -- --nocapture
```

- [ ] **Step 3: Wire ProductOutcome through search/extract/research**

Change signatures to return `Result<ProductOutcome<T>, E>` **or** on error types that cannot carry meta, use `Result<ProductOutcome<T>, (E, ExecMeta)>` — prefer:

```rust
pub async fn search_inner(...) -> Result<ProductOutcome<SearchResponse>, SearchExecError>
```

and on error paths before return, if meta was accumulated, API still needs it — so **errors must carry meta**. Cleanest:

```rust
pub struct SearchExecFailure {
    pub error: SearchExecError,
    pub meta: ExecMeta,
}
// OR change SearchExecError to not hold meta; return Result<ProductOutcome<T>, ProductOutcome<SearchExecError>>
```

**Locked for this plan:** use

```rust
Result<ProductOutcome<T>, ProductOutcome<E>>
```

where `E` is the existing thiserror type. On early failures with empty meta, `meta` is `ExecMeta::default()`.

Set `meta.strategy` from routing decision (`single`/`hybrid`/`blend`/`verify`). Call `note_attempt` in `run_provider` (and extract equivalent) with service, key_id, node_id from holds, success bool.

Add:

```rust
impl ProxyHold {
    pub fn node_id(&self) -> i64 {
        self.lease.node_id
    }
}
```

- [ ] **Step 4: Fix all compile callsites** (API product handlers + MCP + tests) to match `ProductOutcome` — handlers may still ignore meta until Task 3, but must compile:

```rust
match search_inner(...).await {
    Ok(ProductOutcome { result, meta }) => { let _ = &meta; /* Task 3 logs */ Json(result) }
    Err(ProductOutcome { result: e, meta }) => { let _ = &meta; map_err(e) }
}
```

- [ ] **Step 5: Test + commit**

```bash
rtk cargo test -p serpotter-product --locked
rtk cargo test -p serpotter-api --locked --no-run   # compile
rtk git add crates/serpotter-product crates/serpotter-api
rtk git commit -m "feat(product): ExecMeta ProductOutcome and proxy node_id"
```

---

### Task 3: API logging — TokenRow, request_id, filters, spawn_log

**Files:**
- Modify: `crates/serpotter-api/src/lib.rs` (`require_api_token`)
- Modify: `crates/serpotter-api/src/log_request.rs`
- Modify: `crates/serpotter-api/src/product/search.rs`, `extract.rs`
- Modify: `crates/serpotter-api/src/admin/logs.rs`
- Modify: `crates/serpotter-api/src/mcp/mod.rs` (+ auth if needed)
- Modify: `crates/serpotter-api/tests/admin_nodes_logs.rs` and related

**Interfaces:**
- Consumes: `ProductOutcome`, `TokenRow`, `RequestId` / headers
- Produces: enriched request_log rows; filtered admin list

- [ ] **Step 1: Failing API test for log fields + filter**

Extend `admin_nodes_logs.rs` (or new test) to insert via handler path or db with new fields and `GET /api/request-logs?path=/api/se&requestId=...` asserting camelCase JSON.

- [ ] **Step 2: Implement**

`require_api_token` → `Result<TokenRow, Response>`:

```rust
Ok(Some(row)) => Ok(row),
```

Helper:

```rust
fn request_id_from(headers: &HeaderMap, req_ext: Option<&RequestId>) -> Option<String> { ... }
```

For handlers without full Request, read `x-request-id` header (SetRequestId already set it).

`spawn_log` gains params (or `LogFields` struct):

```rust
pub struct LogFields {
    pub path: &'static str,
    pub status: i64,
    pub service: Option<String>,
    pub provider_used: Option<String>,
    pub error_kind: Option<&'static str>,
    pub query_preview: Option<String>,
    pub request_id: Option<String>,
    pub token_name: Option<String>,
    pub strategy: Option<String>,
    pub providers_consulted: Option<String>,
    pub attempt_count: Option<i64>,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
}
```

**Field semantics (fix today's double-write bug):**

| Field | Meaning | Examples |
| --- | --- | --- |
| `service` | **Vendor family** only | `tavily`, `firecrawl`, `exa`, `xai` — **never** `hybrid` / `blend` / `blend-verify` |
| `provider_used` | **Dial / route label** | same as service for single-provider; `hybrid`, `blend`, `blend-verify` for multi-leg strategies |
| `strategy` | Routing strategy when known | `single`, `hybrid`, `blend`, `verify` (may align with dial label) |

On success single-provider: `service = Some("tavily")`, `provider_used = Some("tavily")`.  
On hybrid success: `service = Some(<primary or first consulted vendor>)` or first of `providers_consulted`; `provider_used = Some("hybrid")` — **do not** set both columns to `"hybrid"`.  
On error before any vendor: both `None` unless a vendor was attempted (then last attempted vendor family in `service`).

On insert `Err(e)`: `tracing::warn!(error = %e, "insert_request_log failed")`.

Admin `ListLogsQuery`: add `status`, `path`, `service`, `request_id` (serde camelCase `requestId`). Map to `RequestLogFilter`.

MCP: when logging tools, call `db.get_token_by_value` for the extracted tok (pass name into spawn_log_db). Prefer extracting token once in middleware and storing in request extensions if easy; otherwise lookup at tool log site — **must not skip**.

- [ ] **Step 3: Tests**

```bash
rtk cargo test -p serpotter-api --locked admin_nodes_logs
rtk cargo test --workspace --locked
```

- [ ] **Step 4: Commit**

```bash
rtk git commit -m "feat(api): enrich request_log spawn and admin filters"
```

---

### Task 4: TraceLayer order + MakeSpan + `#[instrument]` hot paths

**Files:**
- Modify: `crates/serpotter-api/src/main.rs` (layers today live on serve router)
- Optionally extract `fn make_trace_layer()` in `crates/serpotter-api/src/trace_layer.rs`
- Modify: `crates/serpotter-api/src/product/search.rs`, `extract.rs` — handler `#[instrument]`
- Modify: `crates/serpotter-product/src/search/run_provider.rs` (and extract attempt loop) — per-attempt spans
- Modify: `crates/serpotter-product/src/search/mod.rs` / `extract/*` free-fns with light `#[instrument(skip_all)]` where useful

- [ ] **Step 1: Reorder layers + custom MakeSpan**

```rust
.layer(PropagateRequestIdLayer::x_request_id()) // inner
.layer(make_trace_layer())
.layer(SetRequestIdLayer::x_request_id(MakeRequestUuid)); // outer
```

Custom MakeSpan fields: `method`, **`path` only** (not full URI), `request_id` from `RequestId` extension (fallback `x-request-id` header). **Never** mint a second UUID. **Never** `include_headers(true)`. OnRequest/OnResponse level **INFO**.

- [ ] **Step 2: `#[instrument]` on API product shells**

On `search`, `extract_handler`, `research` handlers:

```rust
#[tracing::instrument(skip_all, name = "search")]
pub async fn search(...) -> impl IntoResponse { ... }

#[tracing::instrument(skip_all, name = "extract")]
pub async fn extract_handler(...) -> impl IntoResponse { ... }

#[tracing::instrument(skip_all, name = "research")]
pub async fn research(...) -> impl IntoResponse { ... }
```

Inherit HTTP parent span (default contextual parent). Do **not** `parent = None`. Skip large State/Json bodies via `skip_all`.

- [ ] **Step 3: Per-attempt spans in `run_provider` (and extract equivalent)**

Around each provider attempt:

```rust
let span = tracing::info_span!(
    "provider_attempt",
    service = provider,
    key_id = key_hold.key_id(),
    node_id = proxy_hold.as_ref().map(|p| p.node_id()),
    attempt = attempt_idx,
    outcome = tracing::field::Empty,
);
let _guard = span.enter();
// ... call upstream ...
span.record("outcome", "ok"); // or "error" / "exhausted" / "timeout"
```

Note: plan samples use `service = provider` (no `%` format sugar) so markdown/tooling does not mangle the line; implementers MAY use `service = %provider` or `tracing::field::display(&provider)` in real Rust.

Fields: `service`, `key_id`, `node_id`, `attempt`, `outcome` — span fields only.

- [ ] **Step 4: Smoke test**

Oneshot any authenticated or health route: response header `x-request-id` present. Optional: with `LOG_FORMAT` not required in test; compile-check instrument attributes.

- [ ] **Step 5: Commit**

```bash
rtk git commit -m "fix(api): TraceLayer request-id MakeSpan and instrument hot paths"
```

---

### Task 5: Admin SPA logs panel

**Files:**
- Modify: `apps/admin/src/features/logs/types.ts`, `queries.ts`, `LogsPanel.tsx`

- [ ] **Step 1: Types** add camelCase fields matching API.

- [ ] **Step 2: queries** pass `path`, `status`, `service`, `requestId`, `limit` as search params.

- [ ] **Step 3: UI** columns for requestId, tokenName, strategy, keyId, nodeId, attemptCount; filter inputs that set query key.

- [ ] **Step 4: `npm run typecheck` in apps/admin**

- [ ] **Step 5: Commit**

```bash
rtk git commit -m "feat(admin): request log filters and observability columns"
```

---

### Task 6: Docs + AGENTS SoT + workspace gate

**Files:**
- `docs/ops/env.md`, `docs/ops/api.md`
- root `AGENTS.md`, `crates/serpotter-db/AGENTS.md`, `crates/serpotter-api/AGENTS.md`
- `.env.example`

Document schema 12; layer/request_id behavior.

```bash
rtk cargo test --workspace --locked
rtk cargo clippy --workspace --locked -- -D warnings
```

Commit: `docs(ops): observability request_log v12`

---

## Plan self-review

| Spec item | Task |
| --- | --- |
| Schema v12 columns + index | T1 |
| ExecMeta + ProductOutcome + sticky last success | T2 |
| ProxyHold::node_id | T2 |
| TokenRow auth + spawn_log + warn | T3 |
| Admin filters prefix path | T1+T3 |
| MCP always token lookup | T3 |
| Trace order + MakeSpan + `#[instrument]` shells + provider_attempt spans | T4 |
| SPA columns/filters | T5 |
| Docs/AGENTS | T6 |
| Wire DTO unchanged | T2–T3 |

Placeholders: none material; sticky-success rule spelled in T2 tests.

---

## Execution

Plan complete when this file is on disk and committed.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`superpowers:subagent-driven-development`)
2. **Parallel Independent Domains** — only where tasks do not share files (limited here: T1 can start alone; T2–T3 are serial; T4 after AppState stable; T6 can parallel T5 after T4 recorder exists; T7 after T3 API)

**Which approach?**
