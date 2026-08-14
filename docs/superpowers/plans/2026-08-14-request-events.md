# Request Events Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retire the `request_log` SQLite table and replace it with a split model: structured JSON log events (durable audit), an in-memory ring buffer (admin browser), write-time `usage_daily` upserts (spend/usage), and an in-memory error window (alerting).

**Architecture:** `log_request.rs` becomes `events.rs` with one synchronous `emit(&RequestEvents, LogFields, Instant)` funnel — tracing log line, ring push, error-window update, metrics observation, and (task 2) a usage delta into a single SQLite writer task. The `request_log` table and its Db methods are deleted (migration 0017); `usage_daily` gains `key_id`/`token_name` dims and is upserted at write time, fixing the current latent bug where nothing ever populates it.

**Tech Stack:** Rust 1.97 (workspace), axum, tokio (mpsc/oneshot/Notify), tracing, sqlx/SQLite (WAL), prometheus, Vite+ admin SPA. No new dependencies.

## Global Constraints

- Repo gates (CI, branch protection): `cargo fmt --check`, `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`; admin SPA: `npm run typecheck`, `npm run check`, `npm run build` (Node 22.18+).
- Toolchain pinned to 1.97.0 via `rust-toolchain.toml`; never bypass git hooks (`--no-verify` forbidden); conventional commits (`type(scope): subject`).
- Wire contracts stay identical unless a task says otherwise: `/api/request-logs` JSON shape + filters, `/api/usage`, `/api/spend/*`, `/metrics`, alert webhook body. Only `/api/stats` field `requestLogs` → `recentRequests` changes.
- REST/admin JSON camelCase; MCP tool args snake_case. No new dependencies.
- Ring cap `RING_CAP = 2048`; alert constants stay `ALERT_WINDOW_MINUTES = 5`, `ALERT_MIN_TOTAL = 20`, `ALERT_ERROR_RATIO = 0.5`.
- `usage_daily` key/token sentinels: `key_id = 0`, `token_name = ''` (SQLite UNIQUE treats NULLs as distinct, so sentinels are REQUIRED for conflict-dedupe upsert).
- `EXPECTED_SCHEMA_VERSION = 17` after migration `0017_request_events.sql`.
- Every event counts toward usage: `service = fields.service.unwrap_or("unknown")`, `provider_used = fields.provider_used.unwrap_or("unknown")`, 2xx = success else error, `tokens = fields.total_tokens.unwrap_or(0)`, `cost = fields.cost_est.unwrap_or(0.0)` (matches the old rollup's COALESCE semantics).
- Spec: `docs/superpowers/specs/2026-08-14-request-events-design.md` (committed `1945802`).

---

### Task 1: Api-side cutover — events.rs (ring + error window + emit), no request_log reads/writes in the api crate

Replaces `log_request.rs` with `events.rs`; every product request now flows through `emit`. The `request_log` table and its Db methods still exist (removed in Task 2); the api crate stops reading and writing them entirely. Admin log browsing reads the ring; the alert reads the error window; SPA stats field renamed; integration tests that seeded the DB or polled it are reworked to the ring.

**Files:**
- Create: `crates/serpotter-api/src/events.rs`
- Delete: `crates/serpotter-api/src/log_request.rs`
- Modify: `crates/serpotter-api/src/lib.rs` (mod decl, `AppState.events`, `ApiTokenLogged` usage moves), `crates/serpotter-api/src/admin/logs.rs`, `crates/serpotter-api/src/admin/stats.rs`, `crates/serpotter-api/src/product/search.rs`, `crates/serpotter-api/src/product/extract.rs`, `crates/serpotter-api/src/mcp/mod.rs`, `crates/serpotter-api/src/cron.rs`, `crates/serpotter-api/src/main.rs`, `crates/serpotter-api/src/metrics.rs` (doc comments only)
- Test: `crates/serpotter-api/tests/common/mod.rs`, `crates/serpotter-api/tests/admin_logs_pagination.rs`, `crates/serpotter-api/tests/admin_nodes_logs.rs`, `crates/serpotter-api/tests/tracing.rs`, `crates/serpotter-api/src/cron.rs` (unit tests), `crates/serpotter-api/src/events.rs` (new unit tests)
- SPA: `apps/admin/src/features/stats/types.ts`, `apps/admin/src/features/stats/StatsPanel.tsx`, `apps/admin/src/features/logs/LogsPanel.tsx`

**Interfaces:**
- Consumes: `serpotter_product::ExecMeta` (unchanged), `serpotter_auth::extract_token`, `serpotter_db::{Db, TokenRow}`.
- Produces:
  - `events::LogFields` — same struct as today's (all 19 fields, `cache_hit` bool).
  - `events::emit(&RequestEvents, LogFields, Instant)` — sync, never fails.
  - `events::RequestEvents { ring: Arc<RequestRing>, error_window: Arc<ErrorWindow> }` (Task 2 adds usage fields) — `Clone`; `new()` in Task 1 takes no args.
  - `events::RequestRing::push/list/len`, `events::RingFilter`, `events::RingEntryView { id: i64, created_at: String, fields: LogFields }`.
  - `events::ErrorWindow` (`pub(crate)`): `record(i64)`, `record_at(i64, minute)`, `counts(window_minutes)`, `counts_at(now_minute, window_minutes)`; `events::now_minute() -> i64` (`pub(crate)`).
  - `events::ApiTokenLogged(pub serpotter_db::TokenRow)` — moved verbatim, `FromRequestParts<AppState>` emits on rejection.
  - `RequestEvents::test_push(LogFields)` — `#[doc(hidden)] pub`, ring push + window record for integration-test seeding.
  - `AppState.events: RequestEvents`; `cron::spawn_maintenance(db, providers, events: RequestEvents)`; `cron::check_error_rate(&ErrorWindow) -> Option<ErrorRateStats>` (no longer async, no Db).
  - `admin::stats` returns `recentRequests` (ring len); `/api/request-logs` reads the ring with identical filters/JSON.

- [ ] **Step 1: Write `events.rs` (Task 1 version — no usage writer)**

Create `crates/serpotter-api/src/events.rs`:

```rust
//! Request events: the single funnel for every product request (search /
//! extract / research / MCP tools / failed auth). One event emits:
//!   1. a structured tracing log line  → the durable audit (stdout JSON logs)
//!   2. a ring-buffer entry            → admin /api/request-logs browser
//!   3. an error-window update         → cron high-error-rate alert
//!   4. a metrics observation          → /metrics
//! (Task 2 adds: a usage delta → SQLite usage_daily via a single writer task.)
//!
//! The request_log table is gone; raw per-request events live only in the log
//! stream. Nothing here ever fails the request path.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::http::{request::Parts, HeaderMap};
use serpotter_auth::extract_token;
use serpotter_db::{Db, TokenRow};
use serpotter_product::ExecMeta;

use crate::AppState;

/// One request event (service = vendor family; provider_used = dial label).
#[derive(Clone, Debug)]
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
    /// B2: input tokens from the successful provider call (NULL when unknown).
    pub input_tokens: Option<i64>,
    /// B2: output tokens from the successful provider call (NULL when unknown).
    pub output_tokens: Option<i64>,
    /// B2: total tokens (reported, else input+output sum).
    pub total_tokens: Option<i64>,
    /// B2: cost estimate (exact for Exa `costDollars`, credit estimates for
    /// Tavily/Firecrawl; NULL when unknown).
    pub cost_est: Option<f64>,
    /// B5: true when the response was served from the exact-query TTL cache
    /// (zero provider calls) — feeds `serpotter_cache_requests_total{hit}`.
    pub cache_hit: bool,
}

/// Truncate query/url preview to 120 chars for the log line + ring.
pub fn query_preview(s: &str) -> String {
    let mut out: String = s.chars().take(120).collect();
    if s.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// Read `x-request-id` (SetRequestId already set it on the request before handlers).
pub fn request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Multi-leg dial labels — never stored in `service`.
fn is_dial_label(s: &str) -> bool {
    matches!(s, "hybrid" | "blend" | "blend-verify" | "verify")
}

/// Vendor family for `service`: never hybrid/blend; first consulted on dial labels.
/// On bare meta (errors): last attempted vendor when `attempt_count > 0`.
pub fn service_from_meta(provider_used: Option<&str>, meta: &ExecMeta) -> Option<String> {
    if let Some(pu) = provider_used {
        if is_dial_label(pu) {
            return meta.providers_consulted.first().cloned();
        }
        return Some(pu.to_string());
    }
    if meta.attempt_count > 0 {
        meta.providers_consulted
            .last()
            .cloned()
            .or_else(|| meta.providers_consulted.first().cloned())
    } else {
        None
    }
}

/// Dial / route label for research rows. With F16 the `strategy` column stores
/// the RAW routed strategy (fast/balanced/verify/deep), so the research dial
/// label must be derived from it, matching search `provider_used`:
/// `verify` → `blend-verify` (3-leg), `balanced` → `blend` (2-leg),
/// anything else (fast/deep — single chains) → first consulted vendor.
pub fn research_dial_label(meta: &ExecMeta) -> Option<String> {
    match meta.strategy.as_deref() {
        Some("verify") => Some("blend-verify".into()),
        Some("balanced") => Some("blend".into()),
        _ => meta.providers_consulted.first().cloned(),
    }
}

/// Build log fields from product ExecMeta + dial label + auth/correlation.
#[allow(clippy::too_many_arguments)]
pub fn fields_from_meta(
    path: &'static str,
    status: i64,
    error_kind: Option<&'static str>,
    query_preview: Option<String>,
    request_id: Option<String>,
    token_name: Option<String>,
    provider_used: Option<String>,
    meta: &ExecMeta,
) -> LogFields {
    let service = service_from_meta(provider_used.as_deref(), meta);
    LogFields {
        path,
        status,
        service,
        provider_used,
        error_kind,
        query_preview,
        request_id,
        token_name,
        strategy: meta.strategy.clone(),
        providers_consulted: meta.providers_csv(),
        attempt_count: Some(i64::from(meta.attempt_count)),
        key_id: meta.key_id,
        node_id: meta.node_id,
        input_tokens: meta.input_tokens.map(|v| v as i64),
        output_tokens: meta.output_tokens.map(|v| v as i64),
        total_tokens: meta.total_tokens.map(|v| v as i64),
        cost_est: meta.cost,
        cache_hit: meta.cache_hit,
    }
}

/// Resolve MCP token_name + request_id from HTTP Parts (extensions + headers).
///
/// Prefers `TokenRow` stashed by mcp_auth_middleware; falls back to
/// `get_token_by_value` so valid tok- never leaves token_name NULL.
pub async fn resolve_mcp_log_ctx(db: &Db, parts: &Parts) -> (Option<String>, Option<String>) {
    let request_id = request_id_from_headers(&parts.headers);
    if let Some(row) = parts.extensions.get::<TokenRow>() {
        return (Some(row.name.clone()), request_id);
    }
    if let Some(tok) = extract_token(&parts.headers) {
        if let Ok(Some(row)) = db.get_token_by_value(&tok).await {
            return (Some(row.name), request_id);
        }
    }
    (None, request_id)
}

// --- Request ring: the admin "what just happened" browser ------------------

/// Bounded in-memory window of recent request events. Oldest entries are
/// evicted past [`RING_CAP`]. Lost on restart — the JSON log stream is the
/// durable record.
pub const RING_CAP: usize = 2048;

struct RingEntry {
    seq: u64,
    created_at: String,
    fields: LogFields,
}

/// One ring row as served by the admin endpoint (newest-first ordering by seq).
#[derive(Clone, Debug)]
pub struct RingEntryView {
    pub id: i64,
    pub created_at: String,
    pub fields: LogFields,
}

/// Admin list filters for the ring (mirrors the old `RequestLogFilter`).
#[derive(Clone, Debug, Default)]
pub struct RingFilter {
    pub limit: usize,
    pub offset: usize,
    pub status: Option<i64>,
    pub path_prefix: Option<String>,
    pub service: Option<String>,
    pub request_id: Option<String>,
    pub token_name: Option<String>,
}

/// Bounded FIFO of recent events, newest last; `list` returns newest first.
pub struct RequestRing {
    inner: Mutex<RingInner>,
}

struct RingInner {
    entries: VecDeque<RingEntry>,
    next_seq: u64,
}

impl RequestRing {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RingInner {
                entries: VecDeque::new(),
                next_seq: 1,
            }),
        }
    }

    pub fn push(&self, fields: LogFields) {
        let mut inner = self.inner.lock().expect("ring mutex poisoned");
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.entries.push_back(RingEntry {
            seq,
            created_at: utc_now_str(),
            fields,
        });
        if inner.entries.len() > RING_CAP {
            inner.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("ring mutex poisoned").entries.len()
    }

    /// Newest-first rows matching the filter, paged by offset/limit.
    pub fn list(&self, filter: &RingFilter) -> Vec<RingEntryView> {
        let inner = self.inner.lock().expect("ring mutex poisoned");
        inner
            .entries
            .iter()
            .rev()
            .filter(|e| {
                filter.status.is_none_or(|s| e.fields.status == s)
                    && filter
                        .path_prefix
                        .as_deref()
                        .is_none_or(|p| e.fields.path.starts_with(p))
                    && filter
                        .service
                        .as_deref()
                        .is_none_or(|s| e.fields.service.as_deref() == Some(s))
                    && filter
                        .request_id
                        .as_deref()
                        .is_none_or(|r| e.fields.request_id.as_deref() == Some(r))
                    && filter
                        .token_name
                        .as_deref()
                        .is_none_or(|t| e.fields.token_name.as_deref() == Some(t))
            })
            .skip(filter.offset)
            .take(filter.limit)
            .map(|e| RingEntryView {
                id: e.seq as i64,
                created_at: e.created_at.clone(),
                fields: e.fields.clone(),
            })
            .collect()
    }
}

impl Default for RequestRing {
    fn default() -> Self {
        Self::new()
    }
}

// --- Error window: the high-error-rate alert source ------------------------

/// Sliding per-minute error-rate window for the high-error-rate alert.
/// In-memory only: an empty window after restart never fires a false alert
/// (it needs ALERT_MIN_TOTAL requests to accumulate first).
pub(crate) struct ErrorWindow {
    /// (epoch_minute, total, errors) buckets, oldest first.
    inner: Mutex<VecDeque<(i64, i64, i64)>>,
}

impl ErrorWindow {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
        }
    }

    /// Bucket one finished request into the current minute.
    pub(crate) fn record(&self, status: i64) {
        self.record_at(status, now_minute());
    }

    /// Test-visible core: bucket by an explicit epoch minute.
    pub(crate) fn record_at(&self, status: i64, minute: i64) {
        let mut inner = self.inner.lock().expect("error window mutex poisoned");
        match inner.back_mut() {
            Some((m, total, errors)) if *m == minute => {
                *total += 1;
                if !(200..300).contains(&status) {
                    *errors += 1;
                }
            }
            _ => {
                let errors = i64::from(!(200..300).contains(&status));
                inner.push_back((minute, 1, errors));
            }
        }
    }

    /// (total, errors) over the last `window_minutes`, pruning old buckets.
    pub(crate) fn counts(&self, window_minutes: i64) -> (i64, i64) {
        self.counts_at(now_minute(), window_minutes)
    }

    /// Test-visible core: counts for an explicit now-minute.
    pub(crate) fn counts_at(&self, now_minute: i64, window_minutes: i64) -> (i64, i64) {
        let mut inner = self.inner.lock().expect("error window mutex poisoned");
        let cutoff = now_minute - window_minutes;
        while inner.front().is_some_and(|(m, _, _)| *m < cutoff) {
            inner.pop_front();
        }
        let (mut total, mut errors) = (0, 0);
        for (_, t, e) in inner.iter() {
            total += t;
            errors += e;
        }
        (total, errors)
    }
}

/// Current epoch minute (UTC).
pub(crate) fn now_minute() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 60)
        .unwrap_or(0)
}

/// "YYYY-MM-DD HH:MM:SS" in UTC (same shape the old SQLite `datetime('now')`
/// produced), computed without an external date crate.
fn utc_now_str() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant): days since 1970-01-01 → Y-M-D.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

// --- The funnel ------------------------------------------------------------

/// Everything the request funnel touches that must outlive one request:
/// the ring (admin browse) and the error window (alerting). Task 2 adds the
/// usage-writer channel.
#[derive(Clone)]
pub struct RequestEvents {
    pub(crate) ring: Arc<RequestRing>,
    pub(crate) error_window: Arc<ErrorWindow>,
}

impl RequestEvents {
    pub fn new() -> Self {
        Self {
            ring: Arc::new(RequestRing::new()),
            error_window: Arc::new(ErrorWindow::new()),
        }
    }

    /// Integration-test seeding: push an event through the ring + window
    /// without a real HTTP request (deterministic pagination/filter tests).
    #[doc(hidden)]
    pub fn test_push(&self, fields: LogFields) {
        self.ring.push(fields.clone());
        self.error_window.record(fields.status);
    }
}

impl Default for RequestEvents {
    fn default() -> Self {
        Self::new()
    }
}

/// Record one finished product request. Synchronous and non-blocking — never
/// fails the request path. Side effects: structured log line, ring entry,
/// error-window update, metrics observation.
pub fn emit(events: &RequestEvents, fields: LogFields, started: Instant) {
    let duration = started.elapsed();
    let duration_ms = duration.as_millis() as i64;
    // 1. The durable audit line (stdout JSON logs; retention is owned by the
    //    container log pipeline, not the app).
    tracing::info!(
        target: "request",
        path = fields.path,
        method = "POST",
        status = fields.status,
        duration_ms,
        service = fields.service.as_deref(),
        provider_used = fields.provider_used.as_deref(),
        error_kind = fields.error_kind,
        query_preview = fields.query_preview.as_deref(),
        request_id = fields.request_id.as_deref(),
        token_name = fields.token_name.as_deref(),
        strategy = fields.strategy.as_deref(),
        providers_consulted = fields.providers_consulted.as_deref(),
        attempt_count = fields.attempt_count,
        key_id = fields.key_id,
        node_id = fields.node_id,
        input_tokens = fields.input_tokens,
        output_tokens = fields.output_tokens,
        total_tokens = fields.total_tokens,
        cost_est = fields.cost_est,
        cache_hit = fields.cache_hit,
        "request",
    );
    // 2. Admin browser (bounded in-memory window).
    events.ring.push(fields.clone());
    // 3. Error-rate alert window.
    events.error_window.record(fields.status);
    // 4. Live metrics.
    crate::metrics::observe(
        fields.status,
        fields.service.as_deref(),
        duration,
        fields.input_tokens,
        fields.output_tokens,
        fields.cache_hit,
    );
}

// --- F08: failed-auth logging ----------------------------------------------

/// API-token extractor that LOGS failed authentication (F08).
///
/// Identical semantics to `crate::ApiToken` (parts-level `FromRequestParts`,
/// so auth still wins over body parsing, F01) but on a rejected token it
/// emits a 401 event before returning the 401 — otherwise failed auth
/// attempts (missing/invalid token) are invisible in the admin surface.
/// `token_name` stays `None`: the token either does not exist or is not
/// present, so there is no name to attribute.
pub struct ApiTokenLogged(pub serpotter_db::TokenRow);

/// Map a request URI to the static path label stored in the event.
fn static_product_path(uri_path: &str) -> &'static str {
    match uri_path {
        "/api/search" => "/api/search",
        "/api/extract" => "/api/extract",
        "/api/research" => "/api/research",
        _ => "/api",
    }
}

/// Build the F08 auth-failure event (401; body never parsed so no preview,
/// no token name, no usage/cost — the request never reached a provider).
fn auth_failure_fields(parts: &Parts) -> LogFields {
    LogFields {
        path: static_product_path(parts.uri.path()),
        status: 401,
        service: None,
        provider_used: None,
        error_kind: Some("Unauthorized"),
        query_preview: None,
        request_id: request_id_from_headers(&parts.headers),
        token_name: None,
        strategy: None,
        providers_consulted: None,
        attempt_count: None,
        key_id: None,
        node_id: None,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cost_est: None,
        cache_hit: false,
    }
}

#[allow(clippy::result_large_err)]
impl axum::extract::FromRequestParts<AppState> for ApiTokenLogged {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match crate::require_api_token(state, &parts.headers).await {
            Ok(row) => Ok(ApiTokenLogged(row)),
            Err(rejection) => {
                // F08: failed auth emits an event — status 401, request_id
                // from the inbound header (post SetRequestId), path from the
                // URI; the body was never parsed so no preview.
                emit(&state.events, auth_failure_fields(parts), Instant::now());
                Err(rejection)
            }
        }
    }
}
```

- [ ] **Step 2: Add `events.rs` unit tests**

Append to `crates/serpotter-api/src/events.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fields(status: i64, request_id: &str, token_name: &str, path: &'static str) -> LogFields {
        LogFields {
            path,
            status,
            service: Some("tavily".into()),
            provider_used: Some("tavily".into()),
            error_kind: None,
            query_preview: Some("q".into()),
            request_id: Some(request_id.into()),
            token_name: Some(token_name.into()),
            strategy: Some("fast".into()),
            providers_consulted: Some("tavily".into()),
            attempt_count: Some(1),
            key_id: Some(1),
            node_id: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost_est: None,
            cache_hit: false,
        }
    }

    #[test]
    fn ring_cap_evicts_oldest() {
        let ring = RequestRing::new();
        for i in 0..(RING_CAP + 10) as i64 {
            ring.push(fields(200, &format!("req-{i}"), "t", "/api/search"));
        }
        assert_eq!(ring.len(), RING_CAP);
        // Newest first: the evicted page-0 rows are gone.
        let views = ring.list(&RingFilter {
            limit: 5,
            ..Default::default()
        });
        assert_eq!(views.len(), 5);
        assert_eq!(views[0].id, (RING_CAP + 10) as i64, "newest seq first");
    }

    #[test]
    fn ring_list_filters_and_pages() {
        let ring = RequestRing::new();
        for i in 0..5 {
            ring.push(fields(200, &format!("page-{i}"), "tok-a", "/api/search"));
        }
        for i in 0..3 {
            ring.push(fields(502, &format!("err-{i}"), "tok-b", "/api/extract"));
        }
        // Newest-first page of 2 → err-2, err-1.
        let v = ring.list(&RingFilter {
            limit: 2,
            ..Default::default()
        });
        assert_eq!(v[0].fields.request_id.as_deref(), Some("err-2"));
        assert_eq!(v[1].fields.request_id.as_deref(), Some("err-1"));
        // Offset pages into the older half.
        let v = ring.list(&RingFilter {
            limit: 2,
            offset: 2,
            ..Default::default()
        });
        assert_eq!(v[0].fields.request_id.as_deref(), Some("err-0"));
        assert_eq!(v[1].fields.request_id.as_deref(), Some("page-4"));
        // token_name exact.
        let v = ring.list(&RingFilter {
            limit: 10,
            token_name: Some("tok-a".into()),
            ..Default::default()
        });
        assert_eq!(v.len(), 5);
        assert!(v.iter().all(|r| r.fields.token_name.as_deref() == Some("tok-a")));
        // path prefix.
        let v = ring.list(&RingFilter {
            limit: 10,
            path_prefix: Some("/api/extract".into()),
            ..Default::default()
        });
        assert_eq!(v.len(), 3);
        // status exact.
        let v = ring.list(&RingFilter {
            limit: 10,
            status: Some(502),
            ..Default::default()
        });
        assert_eq!(v.len(), 3);
        // request_id exact.
        let v = ring.list(&RingFilter {
            limit: 10,
            request_id: Some("page-2".into()),
            ..Default::default()
        });
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn error_window_buckets_prunes_and_counts() {
        let w = ErrorWindow::new();
        // 20 errors at minute 100, 10 ok at minute 101.
        for _ in 0..20 {
            w.record_at(500, 100);
        }
        for _ in 0..10 {
            w.record_at(200, 101);
        }
        // Window 5 from minute 103: all buckets (100..=101 >= 98) count.
        let (total, errors) = w.counts_at(103, 5);
        assert_eq!(total, 30);
        assert_eq!(errors, 20);
        // Window 1 from minute 103: only minute 101 survives.
        let (total, errors) = w.counts_at(103, 1);
        assert_eq!(total, 10);
        assert_eq!(errors, 0);
        // Old buckets pruned when counted past the cutoff.
        let (total, _) = w.counts_at(200, 5);
        assert_eq!(total, 0);
    }

    #[test]
    fn emit_feeds_ring_and_error_window() {
        let events = RequestEvents::new();
        emit(&events, fields(502, "req-1", "t", "/api/search"), Instant::now());
        assert_eq!(events.ring.len(), 1);
        let (total, errors) = events.error_window.counts(5);
        assert_eq!((total, errors), (1, 1));
    }

    #[test]
    fn utc_now_str_shape() {
        let s = utc_now_str();
        assert_eq!(s.len(), 19, "YYYY-MM-DD HH:MM:SS: {s}");
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[10], b' ');
    }
}
```

- [ ] **Step 3: Wire `AppState.events` and swap the module**

In `crates/serpotter-api/src/lib.rs`:
- Replace `mod log_request;` with `pub mod events;`.
- Add `pub events: events::RequestEvents,` to `AppState` (after `admin_secret`).
- `ApiTokenLogged` was re-exported to product handlers via `crate::log_request::...`; keep `events` module private items accessible — product handlers import `crate::events::{...}` (Step 5).
- Update `app_with_spa` if it references `log_request` (it does not — only the metrics doc comment in `metrics.rs` mentions it).

- [ ] **Step 4: Rewrite `admin/logs.rs` against the ring**

Replace the body of `list_request_logs` and its imports in `crates/serpotter-api/src/admin/logs.rs`:

```rust
use super::require_admin;
use crate::events::{RingFilter, RingEntryView};
use crate::AppState;

// ListLogsQuery and LogOut stay byte-for-byte identical (same query params,
// same serialized shape — only the backing store changes).
```

In `list_request_logs`, replace the filter construction and the DB call:

```rust
    let limit = q.limit.unwrap_or(50).clamp(1, 200) as usize;
    // Lenient status filter: non-numeric values (e.g. "2xx") are treated as
    // absent rather than a 400 so dashboards can pass through raw inputs.
    let status = q.status.and_then(|s| s.parse::<i64>().ok());
    let offset = q.offset.unwrap_or(0).max(0) as usize;
    let filter = RingFilter {
        limit,
        offset,
        status,
        path_prefix: q.path,
        service: q.service,
        request_id: q.request_id,
        token_name: q.token_name,
    };
    let views = state.events.ring.list(&filter);
    let out: Vec<LogOut> = views
        .into_iter()
        .map(|v| log_out_from_view(v))
        .collect();
    (StatusCode::OK, Json(out)).into_response()
```

Add the mapping fn (same shape as the old row→LogOut map):

```rust
fn log_out_from_view(v: RingEntryView) -> LogOut {
    let f = v.fields;
    LogOut {
        id: v.id,
        created_at: v.created_at,
        path: f.path.to_string(),
        method: "POST".to_string(),
        status: f.status,
        service: f.service,
        provider_used: f.provider_used,
        duration_ms: None,
        error_kind: f.error_kind.map(str::to_string),
        query_preview: f.query_preview,
        request_id: f.request_id,
        token_name: f.token_name,
        strategy: f.strategy,
        providers_consulted: f.providers_consulted,
        attempt_count: f.attempt_count,
        key_id: f.key_id,
        node_id: f.node_id,
    }
}
```

> Note: the old SQLite row stored `duration_ms`; `LogFields` carries no duration, so the ring row leaves `duration_ms` null (`skip_serializing_if` drops it from JSON, so the wire shape is unchanged for the fields tests assert). The duration lives in the log line and metrics histogram.

- [ ] **Step 5: Update product + MCP call sites to `events`**

`crates/serpotter-api/src/product/search.rs` and `extract.rs`:
- Change `use crate::log_request::{self, fields_from_meta, request_id_from_headers, ApiTokenLogged};` → `use crate::events::{self, fields_from_meta, request_id_from_headers, ApiTokenLogged};` (extract.rs also imports `research_dial_label`).
- Replace every `log_request::spawn_log(&state, fields, started)` → `events::emit(&state.events, fields, started)`; every `log_request::query_preview(...)` → `events::query_preview(...)`.

`crates/serpotter-api/src/mcp/mod.rs`:
- All `crate::log_request::` paths → `crate::events::` (`resolve_mcp_log_ctx`, `research_dial_label`, `query_preview`, `fields_from_meta`).
- `SerpotterMcp` struct gains `events: Arc<crate::events::RequestEvents>`; `SerpotterMcp::new(product, expected_schema_version, events)` sets it.
- `pub fn service(state: AppState)` → pass `Arc::new(state.events)` into `SerpotterMcp::new`.
- `run_tool` signature: `db: &serpotter_db::Db` → `events: &crate::events::RequestEvents`; the five `crate::log_request::spawn_log_db(db.clone(), fields, started)` calls → `crate::events::emit(events, fields, started)`.
- The three handler call sites pass `&self.events` instead of `&self.product.db`; `resolve_mcp_log_ctx(&self.product.db, &parts)` stays as-is.

- [ ] **Step 6: Admin stats → ring length**

`crates/serpotter-api/src/admin/stats.rs`:
- `StatsOut.request_logs: i64` → `recent_requests: i64`.
- Remove the `count_request_logs` DB branch; set `recent_requests: state.events.ring.len() as i64` (fail-closed DB checks for the other counts stay).

- [ ] **Step 7: Cron alert reads the error window**

`crates/serpotter-api/src/cron.rs`:
- `spawn_maintenance(db, providers)` → `spawn_maintenance(db, providers, events: crate::events::RequestEvents)`; `spawn_maintenance_with_period` likewise; pass `events` into `maintenance_loop`/`run_maintenance_once`.
- `run_maintenance_once(db, providers)` → `run_maintenance_once(db, providers, events: &crate::events::RequestEvents)`; keep the `purge_request_log` call (the table still exists until Task 2; it is now write-dead and purges nothing).
- `alert_if_high_error_rate(db)` → `alert_if_high_error_rate(window: &crate::events::ErrorWindow)`; `check_error_rate(db)` → `check_error_rate(window: &crate::events::ErrorWindow) -> Option<ErrorRateStats>` — replace the DB query with:

```rust
fn check_error_rate(window: &crate::events::ErrorWindow) -> Option<ErrorRateStats> {
    let (total, errors) = window.counts(ALERT_WINDOW_MINUTES);
    let stats = ErrorRateStats { total, errors };
    (stats.total >= ALERT_MIN_TOTAL && stats.error_rate() > ALERT_ERROR_RATIO).then_some(stats)
}
```

- Delete `error_rate_counts` and its `sqlx::Row` import if now unused. In `run_maintenance_once`, call `alert_if_high_error_rate(&events.error_window).await`.

Unit tests in `cron.rs`: replace the `seed_request_log(db, status, n)` DB seeder with:

```rust
/// Seed the error window with `n` requests of one status, all inside the
/// alert window (a fixed minute one minute back).
fn seed_error_window(window: &crate::events::ErrorWindow, status: i64, n: usize) {
    let minute = crate::events::now_minute() - 1;
    for _ in 0..n {
        window.record_at(status, minute);
    }
}
```

Update every `check_error_rate(&db)` call → `check_error_rate(&window)` with `let window = crate::events::ErrorWindow::new();` + `seed_error_window(&window, …)`; tests that called `run_maintenance_once(&db, &providers)` now pass `&crate::events::RequestEvents::new()`.

- [ ] **Step 8: `main.rs` — construct events and wire maintenance**

`crates/serpotter-api/src/main.rs`:
- `let events = serpotter_api::events::RequestEvents::new();` — note: Task 2 changes this to `let (events, usage_writer) = RequestEvents::new(db.clone());` (the Task 2 constructor takes a `Db` and returns the writer handle).
- `spawn_maintenance(db.clone(), providers.clone(), events.clone())`.
- `AppState { db, keys, outbound, providers, admin_secret, events }`.

- [ ] **Step 9: Update `tests/common/mod.rs`**

`state_with(db)` (and the `*_key_pool*`/`require_proxy` variants) build `AppState` — add `events: serpotter_api::events::RequestEvents::new()`. `RequestEvents` is `pub` in the api crate's `pub mod events`, so integration tests can reach it.

- [ ] **Step 10: Rework `admin_logs_pagination.rs`**

Replace the `seed_logs` helper (which called `db.insert_request_log_full`) with ring seeding on the shared state:

```rust
use serpotter_api::events::LogFields;

fn page_fields(i: i64, token_name: &str) -> LogFields {
    LogFields {
        path: "/api/search",
        status: 200,
        service: Some("tavily".into()),
        provider_used: Some("tavily".into()),
        error_kind: None,
        query_preview: Some("page query".into()),
        request_id: Some(format!("page-{i}")),
        token_name: Some(token_name.into()),
        strategy: Some("hybrid".into()),
        providers_consulted: Some("tavily".into()),
        attempt_count: Some(1),
        key_id: None,
        node_id: None,
        input_tokens: Some(10),
        output_tokens: Some(5),
        total_tokens: Some(15),
        cost_est: Some(0.1),
        cache_hit: false,
    }
}

async fn seed_logs(state: &AppState, n: i64, token_name: &str) {
    for i in 0..n {
        state.events.test_push(page_fields(i, token_name));
    }
}
```

Each test changes `let db = test_db().await; seed_logs(&db, …).await; let app = app(state_with(db));` → `let db = test_db().await; let state = state_with(db); seed_logs(&state, …).await; let app = app(state);`. All pagination/offset/filter assertions stay identical (seq ordering reproduces `page-4` newest).

- [ ] **Step 11: Rework `admin_nodes_logs.rs` request-log tests**

Three tests seed `db.insert_request_log` directly:
- `list_request_logs_empty_then_after_insert` — seed after the empty check: `let state = state_with(db); let app = app(state.clone());` … assert empty … `state.events.test_push(…fields…)` … assert one row. Build a small `fn log_fields(status: i64, request_id: &str) -> LogFields` helper (status/path/service/requestId/tokenName as asserted by each test — mirror the current `insert_request_log` argument values at lines 163-233 and 347-360).
- `list_request_logs_observability_fields_and_filters` — seed both rows via `state.events.test_push` (status 200 `/api/search` requestId `req-obs-1`; status 502 `/api/extract` service `firecrawl`) before building the app; the endpoint filter assertions (lines 257-330) stay unchanged.
- `list_request_logs_status_lenient_string` — seed one 200 row via `test_push`; the `status=2xx` lenient assertion stays.
- `list_request_logs_requires_admin` — unchanged (no seeding).

- [ ] **Step 12: Rework `tracing.rs` to poll the ring instead of the DB**

Replace the `wait_for_search_log_row` DB poll with an endpoint poll:

```rust
/// Poll `/api/request-logs` until a row for `/api/search` appears (the ring
/// is fed synchronously by emit in the handler, so this is belt-and-braces).
async fn wait_for_search_ring_row(app: axum::Router) -> (Option<String>, i64) {
    for _ in 0..50 {
        let res = app.clone().oneshot(
            Request::builder()
                .uri("/api/request-logs?path=%2Fapi%2Fsearch&limit=20")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        ).await.unwrap();
        if res.status() == StatusCode::OK {
            let v = body_json(res).await;
            if let Some(row) = v.as_array().and_then(|a| a.first()) {
                return (
                    row["requestId"].as_str().map(String::from),
                    row["status"].as_i64().unwrap_or(0),
                );
            }
        }
    }
    panic!("no /api/search ring row after poll window");
}
```

Both tests change `wait_for_search_log_row(&db).await` → `wait_for_search_ring_row(app.clone()).await` (drop the `db` param where unused; keep `db` for token/key seeding).

- [ ] **Step 13: SPA — stats field rename + logs note**

`apps/admin/src/features/stats/types.ts`: `requestLogs: number;` → `recentRequests: number;`
`apps/admin/src/features/stats/StatsPanel.tsx` (lines ~91-94): label `request logs` → `recent requests`; value `{data.requestLogs ?? 0}` → `{data.recentRequests ?? 0}`.
`apps/admin/src/features/logs/LogsPanel.tsx` (the `block__note` at lines ~124-127): append to the existing note: ` Recent 2,048 requests are kept in memory — full history lives in the server JSON logs (LOG_FORMAT=json).`

- [ ] **Step 14: Update `metrics.rs` doc comments**

`crates/serpotter-api/src/metrics.rs` lines 5 and 129: replace "called by `log_request`" / "for every row written to `request_log`" with "called by `events::emit` for every product request".

- [ ] **Step 15: Gate + commit**

```bash
cargo fmt --check && cargo clippy -p serpotter-api --all-targets -- -D warnings && cargo test -p serpotter-api --locked
cd apps/admin && npm run typecheck
git add -A && git commit -m "refactor(api): replace request_log with in-memory request events"
```

Expected: fmt clean; clippy clean (watch for the removed `sqlx::Row` import in cron.rs); `cargo test -p serpotter-api` passes — including reworked pagination/nodes/tracing/cron suites, and the extract_research/mcp/search_auth suites that poll `/api/request-logs` (they now read the ring, fed by real handlers). SPA typecheck passes.

---

### Task 2: DB migration 0017 + usage writer

Removes `request_log` for real (table, indexes, Db methods, module) and makes `usage_daily` the write-time spend source via a single writer task drained on shutdown. Adds the loud-drop metric. Fixes the latent bug where `/api/usage` had no producer.

**Files:**
- Create: `crates/serpotter-db/migrations/0017_request_events.sql`
- Modify: `crates/serpotter-db/src/lib.rs`, `crates/serpotter-db/src/usage.rs`, `crates/serpotter-api/src/events.rs`, `crates/serpotter-api/src/metrics.rs`, `crates/serpotter-api/src/main.rs`, `crates/serpotter-api/src/cron.rs` (drop purge), `crates/serpotter-api/tests/common/mod.rs`
- Delete: `crates/serpotter-db/src/request_log.rs`
- Test: `crates/serpotter-db/src/usage.rs` (unit tests), `crates/serpotter-api/tests/admin_usage.rs`, `crates/serpotter-api/src/events.rs` (usage writer tests)

**Interfaces:**
- Consumes: `events::RequestEvents` from Task 1; `Db::upsert_usage_daily` new signature.
- Produces:
  - `RequestEvents::new(db: Db) -> (RequestEvents, tokio::task::JoinHandle<()>)`; `RequestEvents::shutdown(&self)`; internal `UsageDelta`; `events::USAGE_CHANNEL_CAP = 1024`.
  - `Db::upsert_usage_daily(&self, service, provider_used, key_id: i64, token_name: &str, requests, successes, errors, tokens, cost)` — date is `date('now')` in SQL (UTC), key/token sentinels `0`/`''`.
  - `Db::usage_summary(days)` — aggregated `SUM(...) GROUP BY service, provider_used, date`.
  - `Db::spend_by_key()`, `Db::spend_by_service()` — read `usage_daily`; sentinel mapped to `None` in `SpendKeyRow`.
  - `metrics::record_drop(reason: &'static str)` + `serpotter_events_dropped_total{reason}`.
  - Deleted from `Db`: `insert_request_log`, `insert_request_log_full`, `purge_request_log`, `count_request_logs`, `list_request_logs`, `rollup_usage_from_request_log`, `RequestLogRow`, `RequestLogFilter`. Deleted envs: `REQUEST_LOG_RETENTION_DAYS`, `REQUEST_LOG_MAX_ROWS`.

- [ ] **Step 1: Write migration `0017_request_events.sql`**

Create `crates/serpotter-db/migrations/0017_request_events.sql`:

```sql
-- Request events wave: drop request_log (raw per-request events live only in
-- the JSON log stream); widen usage_daily with key/token dims so spend-per-key
-- survives without the raw table. PK change requires a table rebuild; old
-- aggregate rows migrate with sentinel key_id=0/token_name='' (they were
-- service-level aggregates, so per-key history is not recoverable).

DROP TABLE request_log;  -- its 6 indexes drop with the table (SQLite)

CREATE TABLE usage_daily_new (
    service TEXT NOT NULL,
    provider_used TEXT NOT NULL,
    date TEXT NOT NULL,
    key_id INTEGER NOT NULL DEFAULT 0,      -- sentinel: unknown key
    token_name TEXT NOT NULL DEFAULT '',    -- sentinel: unknown token
    requests INTEGER NOT NULL DEFAULT 0,
    successes INTEGER NOT NULL DEFAULT 0,
    errors INTEGER NOT NULL DEFAULT 0,
    tokens INTEGER NOT NULL DEFAULT 0,
    cost REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (service, provider_used, date, key_id, token_name)
);
INSERT INTO usage_daily_new (service, provider_used, date, requests, successes, errors, tokens, cost)
    SELECT service, provider_used, date, requests, successes, errors, tokens, cost
    FROM usage_daily;
DROP TABLE usage_daily;
ALTER TABLE usage_daily_new RENAME TO usage_daily;
CREATE INDEX IF NOT EXISTS idx_usage_daily_date ON usage_daily(date);

UPDATE schema_version SET version = 17 WHERE id = 1;
```

- [ ] **Step 2: db crate — bump version, drop the request_log module**

`crates/serpotter-db/src/lib.rs`:
- `pub const EXPECTED_SCHEMA_VERSION: i64 = 17;`
- Remove `mod request_log;` and `pub use request_log::{RequestLogFilter, RequestLogRow};`.
- Delete `crates/serpotter-db/src/request_log.rs`.

- [ ] **Step 3: Rewrite `usage.rs`**

Replace the `impl Db` usage/spend methods in `crates/serpotter-db/src/usage.rs` (row structs `UsageDailyRow`, `SpendKeyRow`, `SpendServiceRow` unchanged; `SpendKeyRow.key_id`/`token_name` stay `Option` — sentinel maps to `None`):

```rust
impl Db {
    /// Accumulate one request's usage into `usage_daily` for TODAY (UTC —
    /// `date('now')` in SQL). `key_id`/`token_name` use the sentinel `0`/`''`
    /// when the request never resolved a key/token (SQLite UNIQUE treats
    /// NULLs as distinct, so sentinels keep the conflict-dedupe honest).
    /// Additive — call once per completed request with per-request deltas.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_usage_daily(
        &self,
        service: &str,
        provider_used: &str,
        key_id: i64,
        token_name: &str,
        requests: i64,
        successes: i64,
        errors: i64,
        tokens: i64,
        cost: f64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO usage_daily (service, provider_used, date, key_id, token_name, requests, successes, errors, tokens, cost) \
             VALUES (?, ?, date('now'), ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(service, provider_used, date, key_id, token_name) DO UPDATE SET \
               requests = usage_daily.requests + excluded.requests, \
               successes = usage_daily.successes + excluded.successes, \
               errors = usage_daily.errors + excluded.errors, \
               tokens = usage_daily.tokens + excluded.tokens, \
               cost = usage_daily.cost + excluded.cost",
        )
        .bind(service)
        .bind(provider_used)
        .bind(key_id)
        .bind(token_name)
        .bind(requests)
        .bind(successes)
        .bind(errors)
        .bind(tokens)
        .bind(cost)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `usage_daily` rows for the last `days` days aggregated across
    /// key/token dims (one row per service+provider+date), newest first
    /// (`days` clamped 1..=90).
    pub async fn usage_summary(&self, days: i64) -> Result<Vec<UsageDailyRow>, DbError> {
        let days = days.clamp(1, 90);
        let rows = sqlx::query(
            "SELECT service, provider_used, date, \
                    SUM(requests) AS requests, SUM(successes) AS successes, \
                    SUM(errors) AS errors, SUM(tokens) AS tokens, SUM(cost) AS cost \
             FROM usage_daily \
             WHERE date >= date('now', '-' || ? || ' days') \
             GROUP BY service, provider_used, date \
             ORDER BY date DESC, service ASC, provider_used ASC",
        )
        .bind(days)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(UsageDailyRow {
                service: r.try_get("service")?,
                provider_used: r.try_get("provider_used")?,
                date: r.try_get("date")?,
                requests: r.try_get("requests")?,
                successes: r.try_get("successes")?,
                errors: r.try_get("errors")?,
                tokens: r.try_get("tokens")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }

    /// Aggregated spend per key/token from `usage_daily`, cost DESC. Sentinel
    /// `key_id=0`/`token_name=''` rows map to `None` (never-resolved keys).
    /// Used by `/api/spend/keys`.
    pub async fn spend_by_key(&self) -> Result<Vec<SpendKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT ud.key_id, ud.token_name, COALESCE(MAX(k.service), 'unknown') AS service, \
                    SUM(ud.requests) AS requests, SUM(ud.cost) AS cost \
             FROM usage_daily ud LEFT JOIN api_keys k ON k.id = ud.key_id \
             GROUP BY ud.key_id, ud.token_name \
             ORDER BY cost DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let key_id: i64 = r.try_get("key_id")?;
            let token_name: String = r.try_get("token_name")?;
            out.push(SpendKeyRow {
                key_id: (key_id != 0).then_some(key_id),
                token_name: (!token_name.is_empty()).then_some(token_name),
                service: r.try_get("service")?,
                requests: r.try_get("requests")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }

    /// Aggregated spend per service from `usage_daily`, cost DESC.
    /// Used by `/api/spend/services`.
    pub async fn spend_by_service(&self) -> Result<Vec<SpendServiceRow>, DbError> {
        let rows = sqlx::query(
            "SELECT service, SUM(requests) AS requests, SUM(cost) AS cost \
             FROM usage_daily \
             GROUP BY service \
             ORDER BY cost DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(SpendServiceRow {
                service: r.try_get("service")?,
                requests: r.try_get("requests")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Rewrite the `usage.rs` unit tests**

Replace the `#[cfg(test)] mod tests` block in `crates/serpotter-db/src/usage.rs` (drop `RequestLogRow`/`RequestLogFilter` imports and the `row_shapes` fn; rollup tests are deleted — there is no rollup anymore):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        Db::connect_for_test().await
    }

    #[tokio::test]
    async fn upsert_usage_daily_accumulates_same_key() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 120, 2.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 2, 1, 1, 40, 0.5)
            .await
            .unwrap();
        let rows = db.usage_summary(7).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 3);
        assert_eq!(rows[0].successes, 2);
        assert_eq!(rows[0].errors, 1);
        assert_eq!(rows[0].tokens, 160);
        assert!((rows[0].cost - 2.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn upsert_usage_daily_key_dim_is_distinct() {
        let db = db().await;
        let k1 = db.insert_api_key("tavily", "tvly-1").await.unwrap();
        let k2 = db.insert_api_key("tavily", "tvly-2").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k1.id, "tok-1", 1, 1, 0, 0, 1.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", k2.id, "tok-2", 1, 1, 0, 0, 2.0)
            .await
            .unwrap();
        // Aggregated summary: one service/provider/date row, both keys summed.
        let rows = db.usage_summary(7).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 2);
        assert!((rows[0].cost - 3.0).abs() < 1e-9);
        // Per-key spend keeps them separate.
        let by_key = db.spend_by_key().await.unwrap();
        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key[0].token_name.as_deref(), Some("tok-2"));
        assert!((by_key[0].cost - 2.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn usage_summary_filters_by_day_window() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 0, 0.0)
            .await
            .unwrap();
        // Backdate the row to 5 days ago (relative: no UTC-midnight flake).
        sqlx::query("UPDATE usage_daily SET date = date('now', '-5 days')")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.usage_summary(2).await.unwrap().is_empty());
        let wide = db.usage_summary(90).await.unwrap();
        assert_eq!(wide.len(), 1);
        assert_eq!(wide[0].service, "tavily");
    }

    #[tokio::test]
    async fn spend_aggregations_group_and_order() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 0, 3.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 0, 1, 0, 2.0)
            .await
            .unwrap();
        // Unknown-key row (sentinel) — cost with no resolved key.
        db.upsert_usage_daily("firecrawl", "firecrawl", 0, "tok-b", 1, 0, 1, 0, 1.0)
            .await
            .unwrap();

        let by_key = db.spend_by_key().await.unwrap();
        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key[0].token_name.as_deref(), Some("tok-a"));
        assert!(by_key[0].key_id.is_some());
        assert_eq!(by_key[0].service, "tavily");
        assert_eq!(by_key[0].requests, 2);
        assert!((by_key[0].cost - 5.0).abs() < 1e-9);
        assert_eq!(by_key[1].token_name.as_deref(), Some("tok-b"));
        assert!(by_key[1].key_id.is_none(), "sentinel 0 maps to null");
        assert_eq!(by_key[1].service, "unknown", "no api_keys row for key_id 0");
        assert!((by_key[1].cost - 1.0).abs() < 1e-9);

        let by_service = db.spend_by_service().await.unwrap();
        assert_eq!(by_service.len(), 2);
        assert_eq!(by_service[0].service, "tavily");
        assert_eq!(by_service[0].requests, 2);
        assert!((by_service[0].cost - 5.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 5: `events.rs` — usage channel + writer task**

In `crates/serpotter-api/src/events.rs`:

Add to the imports: `use serpotter_db::Db;` is already there; add `use tokio::sync::mpsc;` and `use tokio::sync::Notify;` — the file already imports `Instant`, add `use std::time::Duration;` for the drain timeout helper if needed (not needed here).

Replace the `RequestEvents` struct + `new()`:

```rust
/// Bound on the usage-writer channel. A full channel logs error! + counts a
/// drop (the audit line already landed in the log stream; only a rollup cell
/// undercounts — loudly).
const USAGE_CHANNEL_CAP: usize = 1024;

/// One usage_daily accumulation unit (built from LogFields in `emit`).
struct UsageDelta {
    service: String,
    provider_used: String,
    key_id: i64,
    token_name: String,
    success: bool,
    tokens: i64,
    cost: f64,
}

/// Everything the request funnel touches that must outlive one request: the
/// ring (admin browse), the error window (alerting), and the usage-writer
/// channel (write-time usage_daily rollup).
#[derive(Clone)]
pub struct RequestEvents {
    pub(crate) ring: Arc<RequestRing>,
    pub(crate) error_window: Arc<ErrorWindow>,
    usage_tx: mpsc::Sender<UsageDelta>,
    writer_stop: Arc<Notify>,
}

impl RequestEvents {
    /// Spawn the single usage writer. The returned handle must be awaited
    /// (with a timeout) after [`RequestEvents::shutdown`] during graceful
    /// shutdown so pending rollup deltas flush before process exit.
    pub fn new(db: Db) -> (RequestEvents, tokio::task::JoinHandle<()>) {
        let (usage_tx, mut usage_rx) = mpsc::channel::<UsageDelta>(USAGE_CHANNEL_CAP);
        let writer_stop = Arc::new(Notify::new());
        let stop = writer_stop.clone();
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    // Graceful shutdown: drain whatever is queued, then stop.
                    _ = stop.notified() => {
                        while let Ok(delta) = usage_rx.try_recv() {
                            upsert_usage(&db, delta).await;
                        }
                        break;
                    }
                    delta = usage_rx.recv() => {
                        let Some(delta) = delta else { break };
                        upsert_usage(&db, delta).await;
                    }
                }
            }
        });
        (
            Self {
                ring: Arc::new(RequestRing::new()),
                error_window: Arc::new(ErrorWindow::new()),
                usage_tx,
                writer_stop,
            },
            writer,
        )
    }

    /// Stop the usage writer after draining queued deltas (graceful shutdown).
    pub fn shutdown(&self) {
        self.writer_stop.notify_one();
    }

    /// Best-effort send; a full channel logs + counts (never blocks the
    /// request path).
    fn send_usage(&self, delta: UsageDelta) {
        match self.usage_tx.try_send(delta) {
            Ok(()) => {}
            Err(_) => {
                tracing::error!("usage channel full; usage_daily delta dropped");
                crate::metrics::record_drop("channel_full");
            }
        }
    }

    /// Integration-test seeding: push an event through the ring + window
    /// without a real HTTP request (deterministic pagination/filter tests).
    #[doc(hidden)]
    pub fn test_push(&self, fields: LogFields) {
        self.ring.push(fields.clone());
        self.error_window.record(fields.status);
    }
}

/// One writer-side upsert with loud failure accounting.
async fn upsert_usage(db: &Db, delta: UsageDelta) {
    let (successes, errors) = if delta.success { (1, 0) } else { (0, 1) };
    if let Err(e) = db
        .upsert_usage_daily(
            &delta.service,
            &delta.provider_used,
            delta.key_id,
            &delta.token_name,
            1,
            successes,
            errors,
            delta.tokens,
            delta.cost,
        )
        .await
    {
        tracing::error!(error = %e, "usage_daily upsert failed");
        crate::metrics::record_drop("upsert_failed");
    }
}

/// Build the usage_daily delta for an event. Every event counts (the old
/// rollup counted 401/validation rows as unknown-service errors too): service
/// falls back to "unknown", provider to "unknown", key/token to the sentinels,
/// tokens/cost to 0.
fn usage_delta(fields: &LogFields) -> UsageDelta {
    UsageDelta {
        service: fields.service.clone().unwrap_or_else(|| "unknown".into()),
        provider_used: fields
            .provider_used
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        key_id: fields.key_id.unwrap_or(0),
        token_name: fields.token_name.clone().unwrap_or_default(),
        success: (200..300).contains(&fields.status),
        tokens: fields.total_tokens.unwrap_or(0),
        cost: fields.cost_est.unwrap_or(0.0),
    }
}
```

In `emit`, after the metrics observation, add:

```rust
    // 5. Write-time usage rollup (best-effort; the audit line above already
    //    landed in the log stream — a dropped delta only undercounts a cell).
    events.send_usage(usage_delta(&fields));
```

Update the `emit_feeds_ring_and_error_window` unit test: `RequestEvents::new()` now needs a `Db` — change to `let db = serpotter_db::Db::connect_for_test().await;` — this helper is `#[cfg(test)]` inside the db crate and NOT public. Instead build a tiny in-memory Db in the api test via `serpotter_db::connect_and_migrate("sqlite::memory:")` (public). Replace the test with:

```rust
    #[tokio::test]
    async fn emit_feeds_ring_error_window_and_usage_channel() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let (events, _writer) = RequestEvents::new(db);
        emit(&events, fields(502, "req-1", "t", "/api/search"), Instant::now());
        assert_eq!(events.ring.len(), 1);
        let (total, errors) = events.error_window.counts(5);
        assert_eq!((total, errors), (1, 1));
        // The usage delta is queued (writer picks it up asynchronously).
        events.shutdown();
        // The writer handle was detached in the test; give it a beat to flush.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
```

Also add a deterministic writer test using the channel directly:

```rust
    #[tokio::test]
    async fn usage_writer_flushes_queued_deltas_on_shutdown() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let (events, writer) = RequestEvents::new(db.clone());
        let k = db.insert_api_key("tavily", "tvly-w").await.unwrap();
        events.send_usage(UsageDelta {
            service: "tavily".into(),
            provider_used: "tavily".into(),
            key_id: k.id,
            token_name: "tok-w".into(),
            success: true,
            tokens: 100,
            cost: 1.5,
        });
        events.shutdown();
        let flushed = tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .expect("writer must stop after shutdown")
            .expect("writer task must not panic");
        assert!(flushed);
        let rows = db.usage_summary(7).await.expect("usage summary");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens, 100);
        assert!((rows[0].cost - 1.5).abs() < 1e-9);
    }
```

Note: `events.send_usage` is private — the test lives in the same module, so it can call it; `RequestEvents::new` + `db` are needed, and the writer handle must be awaited (not detached) in this test.

- [ ] **Step 6: `metrics.rs` — loud-drop counter**

Add to the `Metrics` struct + registration in `crates/serpotter-api/src/metrics.rs`:

```rust
    events_dropped_total: IntCounterVec,
```

In the `LazyLock` initializer, after `cache_requests_total`:

```rust
    let events_dropped_total = IntCounterVec::new(
        Opts::new(
            "serpotter_events_dropped_total",
            "Request events dropped by the usage writer, by reason (channel_full|upsert_failed).",
        ),
        &["reason"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(events_dropped_total.clone()))
        .expect("register");
```

Add the field to the struct literal and this fn:

```rust
/// Count one dropped usage delta (loud accounting: the audit line already
/// landed in the log stream, so a drop only undercounts a rollup cell).
pub fn record_drop(reason: &'static str) {
    METRICS.events_dropped_total.with_label_values(&[reason]).inc();
}
```

Add a unit test mirroring the existing `observe_increments_counters_and_histogram` pattern (lock `METRICS_LOCK`, reset `events_dropped_total`, call `record_drop("channel_full")` twice + `record_drop("upsert_failed")` once, assert `with_label_values(&["channel_full"]).get() == 2`).

- [ ] **Step 7: `main.rs` — drain the usage writer on shutdown**

In `crates/serpotter-api/src/main.rs`:

```rust
let (events, usage_writer) = serpotter_api::events::RequestEvents::new(db.clone());
```

Pass `events.clone()` to `spawn_maintenance` and into `AppState` as before. After the serve loop and `maint.abort(); let _ = maint.await;`, add:

```rust
// Flush pending usage deltas before exit (bounded 5s; a hard kill loses at
// most the in-channel buffer — the audit line survives in the JSON logs).
events.shutdown();
match tokio::time::timeout(Duration::from_secs(5), usage_writer).await {
    Ok(Ok(())) => {}
    Ok(Err(e)) => tracing::warn!(error = %e, "usage writer panicked during drain"),
    Err(_) => tracing::warn!("usage writer drain timed out after 5s"),
}
```

- [ ] **Step 8: `cron.rs` — drop the purge**

In `run_maintenance_once`, delete the `days`/`max_rows` env reads and the `db.purge_request_log(days, max_rows)` block. `REQUEST_LOG_RETENTION_DAYS` / `REQUEST_LOG_MAX_ROWS` disappear from code (docs in Task 3).

- [ ] **Step 9: `tests/common/mod.rs` — new constructor**

`state_with` and friends: `events: serpotter_api::events::RequestEvents::new(db.clone()).0` (discard the writer handle — it exits when the state drops and the channel closes).

- [ ] **Step 10: Rework `admin_usage.rs` — seed the rollup directly**

Replace `seed_and_rollup` in `crates/serpotter-api/tests/admin_usage.rs` (no more `insert_request_log_full` / `rollup_usage_from_request_log`):

```rust
/// Seed usage_daily via the write-time upsert path (the same call the
/// events writer makes), then the admin endpoints read it directly.
async fn seed_usage(db: &serpotter_db::Db) -> (i64, i64) {
    let tavily_key = db
        .insert_api_key("tavily", "tvly-usage-test-key")
        .await
        .unwrap();
    let firecrawl_key = db
        .insert_api_key("firecrawl", "fc-usage-test-key")
        .await
        .unwrap();
    // tavily success with tokens/cost.
    db.upsert_usage_daily("tavily", "tavily", tavily_key.id, "tok-usage", 1, 1, 0, 100, 1.5)
        .await
        .unwrap();
    // Same service/provider, failed request: counted as an error with tokens.
    db.upsert_usage_daily("tavily", "tavily", tavily_key.id, "tok-usage", 1, 0, 1, 40, 0.5)
        .await
        .unwrap();
    // firecrawl success (no tokens/cost).
    db.upsert_usage_daily("firecrawl", "firecrawl", firecrawl_key.id, "tok-fc", 1, 1, 0, 0, 0.0)
        .await
        .unwrap();
    (tavily_key.id, firecrawl_key.id)
}
```

Update the four tests: `seed_and_rollup(&db).await` → `seed_usage(&db).await`. The usage endpoint assertions (rows grouped per service+provider, requests/successes/errors/tokens/cost) and spend assertions stay — `spend_by_keys_groups_cost_per_key_with_service` should still find a `tok-usage` row with the key's service; `spend_by_services_groups_cost_per_service` unchanged. The doc comment at the top of the file changes to describe write-time seeding.

- [ ] **Step 11: Gate + commit**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --locked
git add -A && git commit -m "refactor(db): drop request_log, write-time usage_daily rollup"
```

Expected: workspace fmt/clippy clean; full workspace test pass (db usage tests, api events/usage tests, admin_usage reworked, cron purge removal compiles — verify no lingering `purge_request_log`/`insert_request_log`/`rollup_usage_from_request_log` references via `cargo test`).

---

### Task 3: Docs

Aligns AGENTS.md and ops docs with the retired table.

**Files:**
- Modify: `AGENTS.md`, `docs/ops/env.md`, `docs/ops/api.md`, `.env.example` (if present)

- [ ] **Step 1: Grep for stale references**

```bash
grep -rn "request_log\|REQUEST_LOG_\|requestLogs" AGENTS.md docs/ .env.example 2>/dev/null
```

- [ ] **Step 2: Update `AGENTS.md`**

- Schema notes: `EXPECTED_SCHEMA_VERSION=16` → `17`; the NOTES bullet about v9–v16 gains: "v17 drops `request_log` (raw events live only in the JSON log stream; admin browsing reads an in-memory 2,048-entry ring) and widens `usage_daily` with `key_id`/`token_name` (sentinels `0`/`''`) upserted at write time."
- "Request log (schema v12)" bullet → describe the events model: `events::emit` (log line `target: "request"`, ring, error window, metrics, usage writer), ring cap 2048, admin endpoint reads the ring, usage_daily write-time, alert window in memory, drop accounting via `serpotter_events_dropped_total`.
- "Maintenance cron" bullet: remove `purge request_log`; keep re-enable/purge sessions/purge cache/credit sync/alert.
- Ops knobs: remove `REQUEST_LOG_RETENTION_DAYS` / `REQUEST_LOG_MAX_ROWS`.
- Admin stats: `requestLogs` → `recentRequests`.

- [ ] **Step 3: Update `docs/ops/env.md` + `.env.example`**

Remove `REQUEST_LOG_RETENTION_DAYS` and `REQUEST_LOG_MAX_ROWS`. Add a note under observability: request events are written to stdout (`LOG_FORMAT=json` recommended for structured lines); the admin request-log page shows the recent in-memory window; `/api/usage` + `/api/spend/*` are populated at write time.

- [ ] **Step 4: Update `docs/ops/api.md`**

- `GET /api/request-logs`: change "SQLite `request_log` table" wording to the in-memory ring (same query params/JSON; newest-first; lost on restart; full history in JSON logs).
- `GET /api/stats`: `requestLogs` → `recentRequests` (ring length).
- `GET /api/usage` / `GET /api/spend/*`: "populated by the request_log rollup" → "populated at write time by the events usage writer".

- [ ] **Step 5: Gate + commit**

```bash
grep -rn "request_log\|REQUEST_LOG_" AGENTS.md docs/ apps/ crates/ --include="*.md" --include="*.rs" --include="*.ts" --include="*.tsx" --include="*.env*" | grep -v "0017_request_events" || true
cargo fmt --check && cargo test --workspace --locked
git add -A && git commit -m "docs: retire request_log, document the events model"
```

Expected: the grep only shows the migration filename (and any intentional `request_log` mentions in the migration itself — `0017` contains `DROP TABLE request_log`, which is intentional); workspace tests still pass.

---

### Task 4: Full-gate verification

- [ ] **Step 1: Run every repo gate**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cd apps/admin && npm ci && npm run typecheck && npm run check && npm run build
```

Expected: all green.

- [ ] **Step 2: Manual smoke — live binary against a scratch DB**

```bash
set -a; source .env 2>/dev/null; set +a
export ADMIN_SECRET=dev-admin
export LOG_FORMAT=json
cargo run -p serpotter-api -- seed-token --name smoke   # capture the tok-
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY" 2>/dev/null || true
cargo run -p serpotter-api &
```

- [ ] **Step 3: Exercise the funnel**

```bash
TOK=<the seeded token>
curl -s -o /dev/null -w "%{http_code}\n" -X POST localhost:8080/api/search -H "Authorization: Bearer $TOK" -H 'Content-Type: application/json' -d '{"query":"rust async"}'
# JSON log line with target "request" appears in stdout
curl -s localhost:8080/api/request-logs -H "Authorization: Bearer $ADMIN_SECRET" | jq '.[0] | {id, path, status, requestId, tokenName}'
curl -s "localhost:8080/api/usage?days=1" -H "Authorization: Bearer $ADMIN_SECRET" | jq .
curl -s localhost:8080/metrics -H "Authorization: Bearer $ADMIN_SECRET" | grep -E "serpotter_requests_total|serpotter_events_dropped_total"
kill %1   # graceful SIGINT: usage writer drains, no warnings about dropped deltas
```

Expected: the request-log row appears in the ring (endpoint) AND as a `"target":"request"` JSON log line; `/api/usage` shows the request (fixing the previously-empty producer); `/metrics` shows counters; graceful shutdown drains cleanly.

- [ ] **Step 4: Final commit if smoke revealed fixes**

If Step 3 uncovered anything, fix + `cargo test --workspace --locked` + amend or add a follow-up commit. Otherwise no commit needed.

