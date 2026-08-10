# MCP Live Progress Notifications — Design

**Date:** 2026-08-11
**Status:** Design (pending review)
**Branch:** main (working tree)

## Problem

Serpotter's MCP tools (`search`, `extract_url`, `research`) are long-running:
multi-provider fallback chains, per-provider retry loops (max 3 attempts), and
research's web+scrape+social legs can take tens of seconds. Today only
`research` emits progress — and only 3 coarse handler-level stages
(`starting` → `running` → `complete`), via `soft_progress` with a hardcoded
fraction. `search` and `extract_url` emit none.

`ExecMeta` records attempts **post-hoc** — the attempt loops live inside
`serpotter-product` (`run_provider`, `try_extract_provider`, research phases),
so the MCP handler can only see the final outcome. Live attempt-level progress
requires a seam inside the product crate.

## Goals

1. All three MCP tools emit live progress: attempt start (`tavily attempt 2/3`),
   retry (`attempt failed, retrying`), provider fallback (`tavily exhausted →
   firecrawl`), research phases (`research: scrape 2/5`), and a terminal
   outcome.
2. **Opt-in by the client**: a request carrying `_meta.progressToken` gets
   progress notifications (SSE stream); a request without one stays on the
   plain-JSON fast path (no wire regression).
3. Product stays pure — no rmcp/axum in `serpotter-product`; the seam is a
   small trait + event enum. REST handlers are untouched.
4. Best-effort: sink failures never fail or slow the request.

## Non-goals

- No schema change, no MCP wire-format change (progress notifications are
  already in the 2026-07-28 spec).
- No changes to cancellation semantics (client disconnect → `ct.cancelled()` →
  `499/Cancelled` log row).
- No progress for REST handlers (no notification channel; they keep plain
  JSON). The sink field defaults to `None` there.
- No attempt detail in the result payload — attempt detail stays in
  `request_log` / `ExecMeta` as today.

## Design

### 1. `serpotter-product` — event model and sink

New types in `crates/serpotter-product/src/meta.rs` (alongside `ExecMeta`):

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
    pub fn message(&self) -> String;
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

`ProductCtx` gains one field:

```rust
pub struct ProductCtx {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub outbound: Arc<ProxyPool>,
    pub providers: ProviderRegistry,
    /// Outbound progress observer (MCP sets it; REST leaves `None`).
    pub progress: Option<Arc<dyn ProgressSink>>,
}
```

plus a convenience helper:

```rust
impl ProductCtx {
    pub fn emit(&self, event: &ProgressEvent) {
        if let Some(sink) = &self.progress {
            sink.emit(event);
        }
    }
}
```

**Convention note (documented exception):** the project rule "no `dyn` trait
objects in product path" targets hot-path service dispatch (routing, provider
registry — explicit match). `progress` is an *outbound observer hook* on the
cold path, behind an `Option` defaulting to `None`; REST is unaffected. The
alternative (generic `S: ProgressSink` param threaded through 8 signatures)
was rejected as churn with no benefit at this call volume.

### 2. Emission points (product)

All emissions go through `ctx.emit(...)`. Because research nests
`search_inner`/`extract_url` with the same shared ctx, nested legs inherit the
sink automatically.

| File | Where | Event(s) |
| --- | --- | --- |
| `search/run_provider.rs` | top of the `for (attempt_idx, _) in (0..MAX_ATTEMPTS)` loop, before the HTTP call | `Attempt { service: provider, attempt: idx+1, max: MAX_ATTEMPTS }` |
| `search/run_provider.rs` | each retryable-failure branch that `continue`s (exhausted, banned, upstream, request-failed) | `Retry { service, attempt: idx+1, reason }` |
| `search/execute.rs` | fallback-chain loop when moving to the next provider | `Fallback { from: prev, to: next, reason }` |
| `extract/extract_url.rs` | `try_extract_provider` attempt loop + chain fallback | `Attempt` / `Retry` / `Fallback` (same shape) |
| `extract/research.rs` | web leg start; per-page scrape (`i/M`); social leg start | `Phase { name, done, total }` |

`MAX_ATTEMPTS` is already a module const in both `run_provider.rs` and
`extract_url.rs`; reuse it as the `max` value.

### 3. `serpotter-api` — MCP sink wiring

New file `crates/serpotter-api/src/mcp/progress.rs` additions (same module as
the existing `soft_progress`):

```rust
pub(crate) struct McpProgressSink {
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
    n: std::sync::atomic::AtomicU64,
}

impl McpProgressSink {
    pub fn new(peer: Peer<RoleServer>, meta: &RequestMetaObject) -> Self {
        Self { peer, token: meta.get_progress_token(), n: AtomicU64::new(0) }
    }
}

impl ProgressSink for McpProgressSink {
    fn emit(&self, event: &ProgressEvent) {
        let Some(token) = &self.token else { return }; // opt-in gate
        let message = event.message();
        let n = self.n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = self.peer.notify_progress(
            ProgressNotificationParam::new(token.clone(), n as f64)
                .with_message(message),
        );
    }
}
```

Each tool handler builds a per-request ctx with the sink and calls the product fn with it:

```rust
let sink = McpProgressSink::new(context.peer.clone(), &context.meta);
let product = ProductCtx { progress: Some(Arc::new(sink)), ..self.product.clone() };
let outcome = tokio::select! {
    r = serpotter_product::search_inner(&product, body) => r,
    _ = ct.cancelled() => { /* unchanged 499 path */ }
};
```

**Handler nuance:** `search` and `extract_url` use `context.peer` /
`context.meta` directly (both fields are `pub` on `RequestContext`).
`research` already extracts `meta: RequestMetaObject` and
`peer: Peer<RoleServer>` as explicit parameters — and rmcp's `FromContextPart`
for `RequestMetaObject` **swaps it out of the context**, so `research` must
build the sink from those params (`McpProgressSink::new(peer, &meta)`), not
from `context.meta` (which is empty there).

The existing 3-stage `soft_progress` calls in `research` are **replaced** by
the event-driven sink (the research `Phase` events carry the stage info). The
handler's terminal `text_ok`/`tool_error` result stays the final response.

**Progress number semantics:** the atomic counter starts at 0 and increments
per event — an ordinal, not a fraction. `total` is omitted (indeterminate
progress), which the spec allows. Clients show "search running…" with a live
message feed.

### 4. Behavior matrix

| Request carries `_meta.progressToken`? | Transport | Client sees |
| --- | --- | --- |
| yes | SSE (`json_response` falls back automatically when the first notification precedes the terminal response) | live `notifications/progress` frames: attempt/retry/fallback/phase lines, then the final result |
| no | plain JSON (unchanged) | terminal result only |

This is exactly why `json_response(true)` + stateless works: rmcp only falls
back to SSE when a notification is actually emitted before the terminal
message — a token-less request emits nothing and stays JSON.

### 5. Error handling

- `emit` returns `()`; the MCP impl swallows `notify_progress` errors
  (`let _ = ...`) — mirrors `soft_progress` best-effort.
- Product `emit` never panics: `Option<Arc<dyn>>` deref is infallible; `NoopSink`
  is a no-op.
- No new error enum variants; no request-log changes (attempt detail already
  lands there via `ExecMeta`).

## Testing

### Product unit tests (`serpotter-product`)

1. `ProgressEvent::message()` — each variant renders the expected one-liner
   (`tavily attempt 2/3`, `attempt failed, retrying: 429`, `tavily exhausted →
   firecrawl`, `research: scrape 2/5`).
2. `ctx.emit` with `progress: None` is a no-op (no panic).
3. Recording-sink integration: a `#[cfg(test)]` `VecSink` capturing events;
   `run_provider` against providers at `127.0.0.1:9` with an exhausted-status
   stub emits `Attempt` then `Retry` in order; `extract_url` chain emits
   `Attempt`/`Fallback` in order.

### API integration tests (`serpotter-api/tests/mcp_stateless.rs`)

4. **With token:** `tools/call` search with `_meta.progressToken` returns
   `text/event-stream` and the parsed SSE contains at least one
   `notifications/progress` data frame whose `params` carry the token and a
   message.
5. **Without token:** the same call returns `application/json` with the
   terminal result — regression guard that the plain-JSON fast path survives.
6. Existing suites (`mcp_session`, `mcp_tools`, `extract_research`, search
   REST) must pass unchanged — `progress: None` everywhere except the MCP
   handlers.

## Files touched

| File | Change |
| --- | --- |
| `crates/serpotter-product/src/meta.rs` | `ProgressEvent`, `ProgressSink`, `NoopSink` + tests |
| `crates/serpotter-product/src/lib.rs` | `ProductCtx.progress` field, `emit` helper, re-exports |
| `crates/serpotter-product/src/search/run_provider.rs` | `Attempt`/`Retry` emissions + tests |
| `crates/serpotter-product/src/search/execute.rs` | `Fallback` emissions |
| `crates/serpotter-product/src/extract/extract_url.rs` | `Attempt`/`Retry`/`Fallback` emissions |
| `crates/serpotter-product/src/extract/research.rs` | `Phase` emissions |
| `crates/serpotter-api/src/mcp/progress.rs` | `McpProgressSink`; **remove** `soft_progress` (its only callers are research's 3-stage lines, replaced by the sink) |
| `crates/serpotter-api/src/mcp/mod.rs` | per-request sink wiring in 3 handlers; drop old 3-stage `soft_progress` calls in research |
| `crates/serpotter-api/tests/mcp_stateless.rs` | tests 4–5 |
| docs (`AGENTS.md`, `docs/ops/api.md`) | MCP progress note |

## Out of scope (future, not in this plan)

- Attempt detail in the result payload / `structuredContent`.
- Progress on the REST `/api/research` path (needs SSE on REST — separate).
- Per-provider ETA / fraction (unknown total until the chain resolves).
