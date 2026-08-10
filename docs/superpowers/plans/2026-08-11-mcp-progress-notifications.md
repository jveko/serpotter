# MCP Live Progress Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit live attempt/retry/fallback/phase progress from Serpotter's product layer, surfaced as MCP `notifications/progress` for clients that send `_meta.progressToken` — without touching the plain-JSON fast path.

**Architecture:** A `ProgressSink` observer hook lives on `ProductCtx` (`Option<Arc<dyn ProgressSink>>`, default None). Product attempt loops call `ctx.emit(ProgressEvent)`. The MCP layer wires an `McpProgressSink` (peer + progressToken) into a per-request cloned ctx; no token → sink no-ops → no SSE fallback. REST keeps `progress: None` and is untouched.

**Tech Stack:** Rust 1.97 (pinned), rmcp 3.1.2, axum 0.8, tokio, serpotter workspace crates.

**Spec:** `docs/superpowers/specs/2026-08-11-mcp-progress-notifications-design.md`

## Global Constraints

- Toolchain pinned to `rust-toolchain.toml` (1.97.0). Use `cargo +1.97.0` for all commands.
- Gates: `cargo +1.97.0 test --workspace --locked` and `cargo +1.97.0 clippy --workspace --locked -- -D warnings` must both pass.
- `serpotter-product` stays pure: **no** rmcp/axum/http deps. `ProgressEvent`/`ProgressSink`/`NoopSink` live there; `McpProgressSink` lives in `serpotter-api`.
- `ProductCtx` keeps `#[derive(Clone)]`; the new field is `pub progress: Option<Arc<dyn ProgressSink>>` — this is the one documented `dyn` exception (outbound observer, not hot-path dispatch). Do not convert to generics.
- Test providers point at `127.0.0.1:9` (connection refused → deterministic retryable failure). No live vendor calls.
- Wire names frozen: tools `search`/`extract_url`/`research`/`health`; REST camelCase; MCP snake_case args with camel aliases. `notifications/progress` is the only new wire surface and is already in the 2026-07-28 spec.
- No `--no-verify`; conventional commits (`feat(scope): ...`).

---
## File structure

- Modify: `crates/serpotter-product/src/meta.rs` — add `ProgressEvent`, `ProgressSink`, `NoopSink`, `VecSink` (test), `message()` tests.
- Modify: `crates/serpotter-product/src/lib.rs` — `ProductCtx.progress` field + `emit()` helper + re-export the new types.
- Modify: `crates/serpotter-product/src/search/run_provider.rs` — `Attempt`/`Retry` emissions.
- Modify: `crates/serpotter-product/src/search/execute.rs` — `Fallback` emissions (single chain + hybrid web leg).
- Modify: `crates/serpotter-product/src/extract/extract_url.rs` — `Attempt`/`Retry`/`Fallback` emissions.
- Modify: `crates/serpotter-product/src/extract/research.rs` — `Phase` emissions (web/scrape/social).
- Modify: `crates/serpotter-api/src/lib.rs` — `product_ctx()` sets `progress: None`.
- Modify: `crates/serpotter-api/src/mcp/progress.rs` — add `McpProgressSink`, remove `soft_progress`.
- Modify: `crates/serpotter-api/src/mcp/mod.rs` — per-request sink wiring in 3 handlers.
- Modify: `crates/serpotter-api/tests/mcp_stateless.rs` — token → SSE test; no-token → JSON regression test.
- Modify: `AGENTS.md`, `docs/ops/api.md` — MCP progress note.

---

### Task 1: Product event model + sink on ProductCtx

**Files:**
- Modify: `crates/serpotter-product/src/meta.rs`
- Modify: `crates/serpotter-product/src/lib.rs`
- Test: `crates/serpotter-product/src/meta.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub enum ProgressEvent { Attempt { service: String, attempt: u32, max: u32 }, Retry { service: String, attempt: u32, reason: String }, Fallback { from: String, to: String, reason: String }, Phase { name: String, done: u32, total: u32 } }` with `pub fn message(&self) -> String`
  - `pub trait ProgressSink: Send + Sync { fn emit(&self, event: &ProgressEvent); }`
  - `pub struct NoopSink;` implementing `ProgressSink` (no-op)
  - `ProductCtx { ..., pub progress: Option<Arc<dyn ProgressSink>> }` + `pub fn emit(&self, event: &ProgressEvent)`
  - Re-exports: `pub use meta::{ExecMeta, ProductOutcome, NoopSink, ProgressEvent, ProgressSink};`

- [ ] **Step 1: Write the failing tests** (append to `crates/serpotter-product/src/meta.rs`):

```rust
#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn event_messages_render_one_liners() {
        assert_eq!(
            ProgressEvent::Attempt { service: "tavily".into(), attempt: 2, max: 3 }.message(),
            "tavily attempt 2/3"
        );
        assert_eq!(
            ProgressEvent::Retry { service: "tavily".into(), attempt: 1, reason: "upstream 429".into() }.message(),
            "tavily attempt 1 failed, retrying: upstream 429"
        );
        assert_eq!(
            ProgressEvent::Fallback { from: "tavily".into(), to: "firecrawl".into(), reason: "exhausted".into() }.message(),
            "tavily failed → firecrawl"
        );
        assert_eq!(
            ProgressEvent::Phase { name: "scrape".into(), done: 2, total: 5 }.message(),
            "research: scrape 2/5"
        );
    }

    #[test]
    fn noop_sink_discards() {
        NoopSink.emit(&ProgressEvent::Attempt { service: "x".into(), attempt: 1, max: 1 });
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo +1.97.0 test -p serpotter-product progress_tests`
Expected: compile error — `ProgressEvent`, `NoopSink` not found.

- [ ] **Step 3: Implement the event model + sink** (add to `crates/serpotter-product/src/meta.rs`, after `ExecMeta`):

```rust
/// One observable step of a provider attempt / research phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// About to attempt a provider call.
    Attempt { service: String, attempt: u32, max: u32 },
    /// A retryable failure; about to retry the same provider.
    Retry { service: String, attempt: u32, reason: String },
    /// Moving to the next provider in a fallback chain.
    Fallback { from: String, to: String, reason: String },
    /// Research phase boundary (web / scrape / social).
    Phase { name: String, done: u32, total: u32 },
}

impl ProgressEvent {
    /// Human-readable one-liner used as the MCP progress message.
    pub fn message(&self) -> String {
        match self {
            Self::Attempt { service, attempt, max } => {
                format!("{service} attempt {attempt}/{max}")
            }
            Self::Retry { service, attempt, reason } => {
                format!("{service} attempt {attempt} failed, retrying: {reason}")
            }
            Self::Fallback { from, to, .. } => format!("{from} failed → {to}"),
            Self::Phase { name, done, total } => {
                format!("research: {name} {done}/{total}")
            }
        }
    }
}

/// Outbound observer hook. Product emits; the API layer decides what to do.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: &ProgressEvent);
}

/// Default sink: discards. Used when `ProductCtx.progress` is `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn emit(&self, _event: &ProgressEvent) {}
}
```

- [ ] **Step 4: Add the field + helper to `ProductCtx`** (in `crates/serpotter-product/src/lib.rs`):

```rust
#[derive(Clone)]
pub struct ProductCtx {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub outbound: Arc<ProxyPool>,
    pub providers: ProviderRegistry,
    /// Outbound progress observer (MCP sets it; REST leaves `None`).
    pub progress: Option<Arc<dyn ProgressSink>>,
}

impl ProductCtx {
    pub fn emit(&self, event: &ProgressEvent) {
        if let Some(sink) = &self.progress {
            sink.emit(event);
        }
    }
}
```

Update the re-export line:
```rust
pub use meta::{ExecMeta, NoopSink, ProductOutcome, ProgressEvent, ProgressSink};
```

- [ ] **Step 5: Fix the one construction site** (`crates/serpotter-api/src/lib.rs` `product_ctx()`):

```rust
        ProductCtx {
            db: self.db.clone(),
            keys: self.keys.clone(),
            outbound: self.outbound.clone(),
            providers: self.providers.clone(),
            progress: None,
        }
```

- [ ] **Step 6: Run the product tests**

Run: `cargo +1.97.0 test -p serpotter-product`
Expected: PASS (2 new tests + existing).

- [ ] **Step 7: Commit**

```bash
git add crates/serpotter-product/src/meta.rs crates/serpotter-product/src/lib.rs crates/serpotter-api/src/lib.rs
git commit -m "feat(product): add progress event model and sink on ProductCtx"
```

---

### Task 2: Search emissions (Attempt / Retry / Fallback)

**Files:**
- Modify: `crates/serpotter-product/src/search/run_provider.rs`
- Modify: `crates/serpotter-product/src/search/execute.rs`
- Test: new `crates/serpotter-product/src/search/progress_tests.rs` (module included from `search/mod.rs` under `#[cfg(test)]`)

**Interfaces:**
- Consumes: `ProgressEvent`, `ProgressSink` + `ProductCtx::emit` from Task 1 (exact signatures above).
- Produces: no new public surface — emissions only. `run_provider` and `execute_*` signatures unchanged.

- [ ] **Step 1: Register the test module** (in `crates/serpotter-product/src/search/mod.rs`, bottom):

```rust
#[cfg(test)]
mod progress_tests;
```

- [ ] **Step 2: Write the failing test** (`crates/serpotter-product/src/search/progress_tests.rs`):

```rust
//! Recording-sink tests for search attempt/fallback emissions.
//! Providers point at 127.0.0.1:9 (connection refused → retryable failure).

use std::sync::{Arc, Mutex};

use serpotter_db::Db;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::{
    ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
};
use serpotter_core::SearchQuery;

use crate::meta::{ProgressEvent, ProgressSink};
use crate::{ProductCtx, search_inner};

/// Collects events in order for assertions.
#[derive(Clone, Default)]
struct VecSink(Arc<Mutex<Vec<ProgressEvent>>>);

impl ProgressSink for VecSink {
    fn emit(&self, event: &ProgressEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

async fn test_db() -> Db {
    serpotter_db::connect_and_migrate("sqlite::memory:").await.expect("migrate")
}

fn test_ctx(db: Db, sink: VecSink) -> ProductCtx {
    let keys = Arc::new(KeyPool::new(db.clone()));
    let outbound = Arc::new(ProxyPool::new(db.clone()));
    let registry = ProviderRegistry::with_clients(
        TavilyClient::new("http://127.0.0.1:9".into()),
        FirecrawlClient::new("http://127.0.0.1:9".into()),
        ExaClient::new("http://127.0.0.1:9".into()),
        XaiClient::new("http://127.0.0.1:9".into()),
    );
    ProductCtx {
        db,
        keys,
        outbound,
        providers: registry,
        progress: Some(Arc::new(sink)),
    }
}

#[tokio::test]
async fn search_emits_attempt_and_retry_in_order() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-progress-test").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx(db, sink.clone());
    let body = SearchQuery {
        query: "hello".into(),
        max_results: Some(1),
        ..Default::default()
    };
    let _ = search_inner(&ctx, body).await; // fails: connection refused after 3 attempts

    let events = sink.0.lock().unwrap().clone();
    let attempts: Vec<&ProgressEvent> = events.iter().filter(|e| matches!(e, ProgressEvent::Attempt { .. })).collect();
    assert_eq!(attempts.len(), 3, "one Attempt per MAX_ATTEMPTS: {events:?}");
    assert_eq!(
        attempts[0],
        &ProgressEvent::Attempt { service: "tavily".into(), attempt: 1, max: 3 }
    );
    assert_eq!(
        attempts[2],
        &ProgressEvent::Attempt { service: "tavily".into(), attempt: 3, max: 3 }
    );
    let retries: Vec<&ProgressEvent> = events.iter().filter(|e| matches!(e, ProgressEvent::Retry { .. })).collect();
    assert_eq!(retries.len(), 2, "two retries after two failures: {events:?}");
    assert!(
        matches!(retries[0], ProgressEvent::Retry { service, attempt: 1, .. } if service == "tavily"),
        "retry names service and attempt: {events:?}"
    );
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo +1.97.0 test -p serpotter-product search_emits_attempt_and_retry_in_order`
Expected: FAIL — no `Attempt` events emitted (len 0).

- [ ] **Step 4: Emit Attempt + Retry in `run_provider.rs`**

At the top of the attempt loop (right after `for (attempt_idx, _) in (0..MAX_ATTEMPTS).enumerate() {`), emit:

```rust
        ctx.emit(&ProgressEvent::Attempt {
            service: provider.to_string(),
            attempt: attempt_idx as u32 + 1,
            max: MAX_ATTEMPTS as u32,
        });
```

In each retryable `continue` branch (the `Err(...)` arms for exhausted / banned / upstream / request-failed that set `last_err = ...; continue;`), emit immediately before `continue`:

```rust
                ctx.emit(&ProgressEvent::Retry {
                    service: provider.to_string(),
                    attempt: attempt_idx as u32 + 1,
                    reason: format!("{provider} exhausted status {status}: {b}"),
                });
```

Use a reason matching the branch (exhausted status, banned status, upstream status, request failed) — copy the message text already assigned to `last_err`.

Add the import at the top of the file:
```rust
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
```

- [ ] **Step 5: Emit Fallback in `execute.rs` single chain + hybrid web leg**

In `execute_single_chain`, convert the loop to track the previous provider:

```rust
    let chain = fallback_chain(&decision.provider);
    let mut meta = ExecMeta::default();
    let mut last_err = SearchExecError::NoHealthyKey("No healthy provider key".into());

    for (i, provider) in chain.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: chain[i - 1].to_string(),
                to: provider.to_string(),
                reason: last_err.to_string(),
            });
        }
        match run_provider(
            ctx,
            provider,
            ...
        )
        .await
        { ... unchanged ... }
    }
```

`run_provider` takes `provider: &str` — pass `provider` (already `&&str` from `.iter()`; `provider` derefs automatically in the existing call since the original `for provider in chain` gave `&str`; adjust by using `provider` as-is if the call site compiled with `&str`, else `*provider`).

Same pattern in `execute_hybrid`'s `web_fut` loop over `fallback_chain("tavily")` (this one already uses `for provider in fallback_chain("tavily")` — convert to indexed iteration with the same `Fallback` emission using `chain[i - 1]`).

Add the import:
```rust
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
```

- [ ] **Step 6: Run the search tests**

Run: `cargo +1.97.0 test -p serpotter-product`
Expected: PASS — new test plus existing suite (fallback-chain behavior unchanged; events are additive).

- [ ] **Step 7: Commit**

```bash
git add crates/serpotter-product/src/search/
git commit -m "feat(product): emit search attempt, retry, and fallback progress"
```

---

### Task 3: Extract + research emissions

**Files:**
- Modify: `crates/serpotter-product/src/extract/extract_url.rs`
- Modify: `crates/serpotter-product/src/extract/research.rs`
- Test: append to `crates/serpotter-product/src/search/progress_tests.rs` (reuse `VecSink`/`test_ctx`) — or a new `crates/serpotter-product/src/extract/progress_tests.rs` registered from `extract/mod.rs`.

**Interfaces:**
- Consumes: Task 1 types + helpers; Task 2's `VecSink`/`test_ctx` if shared (move them to a `#[cfg(test)]` common location if both modules need them — recommended: keep `VecSink`/`test_ctx` in `search/progress_tests.rs` and have extract tests construct their own copy).
- Produces: no new public surface.

- [ ] **Step 1: Write the failing test** (append to `crates/serpotter-product/src/search/progress_tests.rs`):

```rust
#[tokio::test]
async fn extract_emits_attempt_retry_and_phase_order() {
    let db = test_db().await;
    db.insert_api_key("firecrawl", "fc-progress-test").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx(db, sink.clone());
    let _ = crate::extract_url(&ctx, "https://example.com", Some("firecrawl")).await;

    let events = sink.0.lock().unwrap().clone();
    let attempts: Vec<&ProgressEvent> = events.iter().filter(|e| matches!(e, ProgressEvent::Attempt { .. })).collect();
    assert_eq!(attempts.len(), 3, "firecrawl attempts: {events:?}");
    let retries: Vec<&ProgressEvent> = events.iter().filter(|e| matches!(e, ProgressEvent::Retry { .. })).collect();
    assert_eq!(retries.len(), 2, "retries after failures: {events:?}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo +1.97.0 test -p serpotter-product extract_emits_attempt_retry_and_phase_order`
Expected: FAIL — no events.

- [ ] **Step 3: Emit Attempt/Retry in `try_extract_provider`** (`extract_url.rs`)

Top of the attempt loop (after `for (attempt_idx, _) in (0..MAX_ATTEMPTS).enumerate() {`):

```rust
        ctx.emit(&ProgressEvent::Attempt {
            service: provider.to_string(),
            attempt: attempt_idx as u32 + 1,
            max: MAX_ATTEMPTS as u32,
        });
```

In each retryable `continue` branch, emit `Retry` before `continue` with a branch-specific reason (same shape as Task 2).

Add import:
```rust
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
```

- [ ] **Step 4: Emit Fallback in the extract provider chain** (`extract_url.rs`, the `for provider in chain` loop in `extract_url`):

```rust
    let mut meta = ExecMeta::default();
    let mut last = ExtractError::NoHealthyKey("No healthy extract key".into());
    for (i, provider) in chain.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: chain[i - 1].to_string(),
                to: provider.to_string(),
                reason: last.to_string(),
            });
        }
        match try_extract_provider(ctx, provider, url).await { ... unchanged ... }
    }
```

- [ ] **Step 5: Emit Phase in `research_inner`** (`research.rs`)

Before the web search leg (`let search_out = match search_inner(ctx, q).await`):

```rust
    ctx.emit(&ProgressEvent::Phase {
        name: "web".into(),
        done: 1,
        total: 3,
    });
```

Inside `scrape_fut` (before the `join_all`), emit the scrape phase with per-page count. Convert the scrape iterator to enumerate (it currently maps `scrape_targets.into_iter().map(...)`):

```rust
    let scrape_total = scrape_targets.len() as u32;
    let scrape_fut = async {
        let pairs = futures_util::future::join_all(scrape_targets.into_iter().enumerate().map(
            |(i, (url, title))| async move {
                ctx.emit(&ProgressEvent::Phase {
                    name: "scrape".into(),
                    done: i as u32 + 1,
                    total: scrape_total,
                });
                match extract_url(ctx, &url, None).await { ... unchanged ... }
            },
        ))
        .await;
        ...
    };
```

Before the social leg (guarded by `if run_social`), emit:

```rust
    if run_social {
        ctx.emit(&ProgressEvent::Phase {
            name: "social".into(),
            done: 3,
            total: 3,
        });
    }
```

Add import:
```rust
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
```

- [ ] **Step 6: Run product tests**

Run: `cargo +1.97.0 test -p serpotter-product`
Expected: PASS — new extract test + existing suite.

- [ ] **Step 7: Commit**

```bash
git add crates/serpotter-product/src/extract/
git commit -m "feat(product): emit extract and research phase progress"
```

---

### Task 4: MCP sink wiring + integration tests

**Files:**
- Modify: `crates/serpotter-api/src/mcp/progress.rs`
- Modify: `crates/serpotter-api/src/mcp/mod.rs`
- Test: `crates/serpotter-api/tests/mcp_stateless.rs`

**Interfaces:**
- Consumes: `ProgressEvent`, `ProgressSink`, `ProductCtx.progress` from Task 1.
- Produces: `pub(crate) struct McpProgressSink { ... }` in `progress.rs`; handlers call product fns with a cloned ctx carrying the sink.

- [ ] **Step 1: Write the failing integration tests** (append to `crates/serpotter-api/tests/mcp_stateless.rs`):

```rust
/// A stateless tools/call whose _meta carries a progressToken must stream
/// notifications/progress (SSE) and end with the terminal result.
#[tokio::test]
async fn mcp_stateless_search_with_progress_token_streams_sse() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-progress").await.unwrap();
    let app = app(state_with(db));

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "progressToken": "tok-abc-123"
            },
            "name": "search",
            "arguments": { "query": "hello", "max_results": 1 }
        }
    });
    let res = app
        .oneshot(stateless_request("tools/call", Some("search"), serde_json::to_string(&body).unwrap()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "with progressToken the response must be SSE, got content-type={ct}"
    );
    let text = String::from_utf8(body_bytes(res).await.to_vec()).unwrap();
    assert!(
        text.contains("notifications/progress"),
        "SSE must carry notifications/progress frames: {text}"
    );
    assert!(
        text.contains("progressToken") || text.contains("\"token\":\"tok-abc-123\""),
        "progress frames must echo the client token: {text}"
    );
}

/// Without a progressToken the fast path stays plain JSON.
#[tokio::test]
async fn mcp_stateless_search_without_token_stays_json() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            stateless_body("tools/call", 31, serde_json::json!({"name": "search", "arguments": {"query": "hello", "max_results": 1}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    assert!(
        ct.starts_with("application/json"),
        "without progressToken response must stay plain JSON, got content-type={ct}"
    );
    let v = body_json(res).await;
    assert!(v.get("result").is_some(), "terminal result present: {v}");
}
```

Note: `body_bytes` is already exported by `tests/common`. `state_with` uses `max_inflight=3`, so a single tavily key at `:9` fails attempts deterministically.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo +1.97.0 test -p serpotter-api --test mcp_stateless mcp_stateless_search_with`
Expected: FAIL — no SSE with token (still JSON), no progress frames.

- [ ] **Step 3: Add `McpProgressSink` to `progress.rs` and remove `soft_progress`**

Replace the `soft_progress` function (and its doc comment) in `crates/serpotter-api/src/mcp/progress.rs`:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken, RequestMetaObject};
use rmcp::service::{Peer, RoleServer};
use serpotter_product::{ProgressEvent, ProgressSink};

use super::errors::tool_error;

/// MCP progress sink: emits each product event as a `notifications/progress`
/// frame. Opt-in — without a client `_meta.progressToken` it no-ops, so the
/// request keeps the plain-JSON fast path.
pub(crate) struct McpProgressSink {
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
    n: AtomicU64,
}

impl McpProgressSink {
    pub fn new(peer: Peer<RoleServer>, meta: &RequestMetaObject) -> Self {
        Self {
            peer,
            token: meta.get_progress_token(),
            n: AtomicU64::new(0),
        }
    }
}

impl ProgressSink for McpProgressSink {
    fn emit(&self, event: &ProgressEvent) {
        let Some(token) = &self.token else {
            return;
        };
        let message = event.message();
        let n = self.n.fetch_add(1, Ordering::Relaxed);
        let _ = self.peer.notify_progress(
            ProgressNotificationParam::new(token.clone(), n as f64).with_message(message),
        );
    }
}

/// Serialize a tool result as a single pretty JSON text block. The only error
/// path (serde serialization failure) goes through the same structured
/// [`tool_error`] envelope as every other tool failure, so clients never see a
/// bare, kind-less error text.
pub(crate) fn text_ok<T: serde::Serialize>(
    value: T,
    request_id: Option<String>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    match serde_json::to_string_pretty(&value) {
        Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
        Err(e) => Ok(tool_error(
            "InternalError",
            format!("serialize failed: {e}"),
            request_id,
        )),
    }
}
```

- [ ] **Step 4: Wire the sink in the three handlers** (`crates/serpotter-api/src/mcp/mod.rs`)

Update the import (`use progress::{soft_progress, text_ok};` → `use progress::{text_ok, McpProgressSink};`) and add `use std::sync::Arc;` is already present. In `search` (before the `tokio::select!`):

```rust
        let sink = McpProgressSink::new(context.peer.clone(), &context.meta);
        let product = ProductCtx {
            progress: Some(Arc::new(sink)),
            ..self.product.clone()
        };
        let ct = context.ct.clone();
        let outcome = tokio::select! {
            r = serpotter_product::search_inner(&product, body) => r,
            ...unchanged cancel arm...
        };
```

Same in `extract_url` (same shape, `serpotter_product::extract_url(&product, ...)`).

In `research`, build the sink from the explicit `peer`/`meta` params (the `RequestMetaObject` param is swapped out of the context by rmcp's extractor — do NOT read `context.meta` there):

```rust
        let sink = McpProgressSink::new(peer.clone(), &meta);
        let product = ProductCtx {
            progress: Some(Arc::new(sink)),
            ..self.product.clone()
        };
```

Then delete the four `soft_progress(...)` calls in `research` (starting / running / complete / failed). The final terminal result path (`text_ok` / `tool_error`) is unchanged.

Add `use serpotter_product::ProductCtx;` to the imports if not already present.

- [ ] **Step 5: Run the API tests**

Run: `cargo +1.97.0 test -p serpotter-api --test mcp_stateless`
Expected: PASS — both new tests + existing 9.

- [ ] **Step 6: Run full gates**

Run: `cargo +1.97.0 test --workspace --locked && cargo +1.97.0 clippy --workspace --locked -- -D warnings`
Expected: all suites pass, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/serpotter-api/src/mcp/ crates/serpotter-api/tests/mcp_stateless.rs
git commit -m "feat(mcp): live progress notifications via product progress sink"
```

---

### Task 5: Docs

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/ops/api.md`

- [ ] **Step 1: Update `AGENTS.md` MCP bullet** (the dual-era bullet): append a sentence:

```
Progress: MCP tools emit live `notifications/progress` (attempt/retry/fallback/phase)
when the client sends `_meta.progressToken`; without a token responses stay plain JSON.
```

- [ ] **Step 2: Update `docs/ops/api.md` MCP table** — add a row:

```
| Progress | `notifications/progress` on SSE when the client sends `_meta.progressToken` (attempt/retry/fallback/phase lines); no token → plain JSON |
```

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md docs/ops/api.md
git commit -m "docs: MCP progress notifications contract"
```

---

## Self-review

- **Spec coverage:** event enum + sink (T1) ✓; Attempt/Retry in `run_provider` + `try_extract_provider` (T2/T3) ✓; Fallback in `execute.rs` single+hybrid chains and extract chain (T2/T3) ✓; Phase in research web/scrape/social (T3) ✓; `McpProgressSink` opt-in + handler wiring + `soft_progress` removal (T4) ✓; token → SSE / no-token → JSON tests (T4) ✓; docs (T5) ✓.
- **Placeholders:** none — every step has concrete code or an exact command.
- **Type consistency:** `ProgressEvent` variants and `message()` output match the spec examples (`tavily attempt 2/3`, `tavily failed → firecrawl`, `research: scrape 2/5`). `ProductCtx.emit` signature stable across tasks. `McpProgressSink::new(peer, meta)` matches handler shapes; research uses the extracted params (context-swap nuance documented).
