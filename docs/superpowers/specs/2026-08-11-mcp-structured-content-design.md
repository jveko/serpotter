# MCP StructuredContent Results — Design

**Date:** 2026-08-11
**Status:** Design (approved)
**Branch:** feat/mcp-progress worktree (`.worktrees/feat-mcp-progress`)

## Problem

Serpotter's MCP tools (`search`, `extract_url`, `research`) return results as
**pretty-JSON text blocks** inside `content` (`text_ok` →
`CallToolResult::success`). MCP clients that want machine-readable data must
string-extract the block and re-parse it — fragile, and the spec offers a
first-class field for exactly this: `structuredContent` on `CallToolResult`
(2026-07-28; SEP-2106 relaxes it to any JSON value).

## Goals

1. Every `tools/call` result for the three result-bearing tools carries the
   typed camelCase response object in `structuredContent`, alongside the
   existing human-readable text block in `content` (both present, identical
   data — no consumer breakage).
2. Tool errors (`{kind,message,requestId}` envelope) also become structured
   (`structuredError`-style), so failures are machine-readable too.
3. `tools/list` advertises `outputSchema` for the three tools, generated from
   the real Rust types via schemars (no hand-maintained schemas).
4. Additive only: wire names, arg shapes, error envelope, and the text
   content all unchanged.

## Non-goals

- **Cursor pagination is explicitly out of scope.** Verified: no provider
  exposes page/offset/cursor (Tavily sends only `max_results`; Exa only
  `numResults`). Real result continuation does not exist upstream. Re-running
  with bumped `max_results` and slicing would be fake pagination (results
  shift between calls, wasted provider calls) — rejected. The spec's
  `nextCursor` applies to list endpoints; our `tools/list` is single-shot.
  Documented with evidence in the spec.
- No changes to REST, CLI, health tool (stays text-only — trivial body,
  YAGNI), or cancellation/progress behavior.

## Design

### 1. rmcp surface (verified in 3.1.2 source)

```rust
// rmcp::model::CallToolResult
pub fn structured(value: Value) -> Self;         // content=[text block], structuredContent=Some(value), isError=false
pub fn structured_error(value: Value) -> Self;   // same, isError=true
```

Both keep a human-readable text block in `content` — machine + human in one
result, zero client breakage.

### 2. `serpotter-api/src/mcp/` — structured helpers

**`progress.rs`** — replace `text_ok` with `structured_ok` (rename +
body change; `text_ok` has no other callers):

```rust
/// Serialize a tool result as structured content, keeping a human-readable
/// pretty-JSON text block in `content`. The only error path (serde failure)
/// goes through the same structured error envelope as every other failure.
pub(crate) fn structured_ok<T: serde::Serialize>(
    value: T,
    request_id: Option<String>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    match serde_json::to_value(&value) {
        Ok(v) => Ok(CallToolResult::structured(v)),
        Err(e) => Ok(tool_error(
            "InternalError",
            format!("serialize failed: {e}"),
            request_id,
        )),
    }
}
```

**`errors.rs`** — `tool_error` gains a structured twin (the envelope object
rides in `structuredContent`, text block stays):

```rust
/// Structured variant: the error envelope `{kind,message,requestId}` also
/// lands in `structuredContent` so failures are machine-readable.
pub fn tool_error_structured(
    kind: &str,
    message: String,
    request_id: Option<String>,
) -> CallToolResult {
    let envelope = ToolErrorEnvelope {
        kind,
        message: &message,
        request_id: request_id.as_deref(),
    };
    let value = serde_json::to_value(&envelope).unwrap_or_else(|_| {
        serde_json::json!({ "kind": "InternalError", "message": "failed to serialize tool error", "requestId": null })
    });
    let body = serde_json::to_string(&value)
        .unwrap_or_else(|_| r#"{"kind":"InternalError","message":"failed to serialize tool error","requestId":null}"#.to_string());
    CallToolResult::structured_error(value)  // content=[text block of body]
}
```

(Implementation detail: `structured_error(value)` serializes `value` for the
text block itself — the helper should produce the envelope once and pass it
to `structured_error`; exact shape decided at implementation to avoid double
serialization.)

### 3. Handler wiring (`mcp/mod.rs`)

- `search` / `extract_url` / `research` success arms: `text_ok(resp, …)` →
  `structured_ok(resp, …)` (the typed `SearchResponse` / `ExtractResponse` /
  `ResearchResponse`).
- All `tool_error(...)` call sites (validation, cancelled, provider failure):
  → `tool_error_structured(...)`. Exception: keep the wire `kind`/`message`
  keys identical — only the transport of the envelope gains
  `structuredContent`.
- `health`: unchanged (text-only).

### 4. `outputSchema` advertisement

The `#[tool]` macro accepts `output_schema = <expr>` where `<expr>` is an
`Arc<JsonObject>` (macro calls `.with_raw_output_schema(#s)`; verified in
rmcp-macros 3.1.2 `src/tool.rs:115-116`). rmcp provides
`schema_for_output::<T: JsonSchema + Any>()` (handler/server/tool.rs) for
exactly this.

Tool attrs gain:

```rust
#[tool(
    description = "...",
    output_schema = serpotter_api::mcp::output_schema::<serpotter_core::SearchResponse>(),
)]
```

where the MCP module re-exports rmcp's helper:

```rust
use rmcp::handler::server::tool::schema_for_output;
fn output_schema<T: schemars::JsonSchema + std::any::Any>() -> std::sync::Arc<serde_json::Map<String, serde_json::Value>> {
    schema_for_output::<T>()
}
```

(Exact `Arc<JsonObject>` alias type verified at implementation; the macro
expr just needs to produce it.)

### 5. `JsonSchema` derives — dependency implication

`schema_for_output::<T>()` requires `T: JsonSchema`. The result types are:

| Type | Crate | Has JsonSchema today? |
| --- | --- | --- |
| `SearchResponse` (+ `SearchItem`) | `serpotter-core` | no |
| `ExtractResponse` | `serpotter-product` | no |
| `ResearchResponse` / `ScrapedPage` / `Citation` / `Evidence` | `serpotter-product` | no |

So add `schemars` (already in Cargo.lock at 1.2.1, currently a transitive dep
via rmcp) as an explicit dependency:

- Root `[workspace.dependencies]`: `schemars = "1.2.1"`.
- `serpotter-core` and `serpotter-product`: `schemars = { workspace = true }`
  + `#[derive(schemars::JsonSchema)]` added to the response DTO structs.

`schemars`' derive respects `#[serde(rename_all = "camelCase")]`, so the
advertised schemas match the wire exactly — no dead or misnamed fields.
Derive-only, no behavior change. Workspace rule "versions in root
workspace.dependencies" is honored.

## Testing

### API integration (`crates/serpotter-api/tests/mcp_stateless.rs`)

1. `mcp_stateless_search_structured_content` — search tools/call (tavily key,
   `:9` failure path is fine for shape): assert `result.structuredContent` is
   a JSON object, and its text equals the parsed `content[0].text` (both
   present, identical).
2. Same for `extract_url` and `research` (shape presence + content/structured
   agreement).
3. Error path: a failing call (e.g. validation error, empty url) asserts
   `result.structuredContent.kind == "ValidationError"` and
   `result.isError == true` while the text block still carries the envelope.
4. `tools/list` — the three tools' `outputSchema` present, root
   `type: "object"`, and `properties` keys are camelCase (spot-check
   `search` exposes `maxResults`, `research` exposes `webResults`).

### Existing suites

`mcp_session`, `mcp_tools`, `mcp_stateless`, `extract_research` must pass
unchanged — `structuredContent` is additive; the text content is byte-identical
to today's output.

## Files touched

| File | Change |
| --- | --- |
| `Cargo.toml` (root) | add `schemars = "1.2.1"` to workspace deps |
| `crates/serpotter-core/Cargo.toml` | add schemars dep |
| `crates/serpotter-core/src/types.rs` | `#[derive(schemars::JsonSchema)]` on `SearchResponse`, `SearchItem` |
| `crates/serpotter-product/Cargo.toml` | add schemars dep |
| `crates/serpotter-product/src/dto.rs` | JsonSchema derives on response DTOs |
| `crates/serpotter-api/src/mcp/progress.rs` | `text_ok` → `structured_ok` |
| `crates/serpotter-api/src/mcp/errors.rs` | `tool_error_structured` (keep `tool_error` or fold — implementation detail) |
| `crates/serpotter-api/src/mcp/mod.rs` | handlers use structured helpers; tool attrs gain `output_schema` |
| `crates/serpotter-api/tests/mcp_stateless.rs` | 4 new tests |
| docs (`AGENTS.md`, `docs/ops/api.md`) | structuredContent + outputSchema note |

## Out of scope (documented non-goals)

- Cursor pagination (no upstream paging — evidence above).
- structuredContent on `health` (trivial body, YAGNI).
- REST/CLI changes.
- Changing the error envelope keys or any wire name.
