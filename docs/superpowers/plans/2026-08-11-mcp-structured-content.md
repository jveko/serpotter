# MCP StructuredContent Results Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every MCP tool result machine-readable by carrying the typed camelCase response in `CallToolResult.structuredContent` (alongside the existing human text block) and advertising `outputSchema` in `tools/list`.

**Architecture:** Replace the MCP `text_ok` success helper with `structured_ok` (uses rmcp 3.1.2 `CallToolResult::structured`), add a structured error twin (`tool_error_structured` via `CallToolResult::structured_error`), update the three handlers, and advertise `output_schema` on the three result-bearing `#[tool]` attrs using rmcp's `schema_for_output::<T>()`, backed by `JsonSchema` derives on the response DTOs.

**Tech Stack:** Rust 1.97 (pinned), rmcp 3.1.2, schemars 1.2.1 (new explicit dep), axum, serpotter workspace crates.

**Spec:** `docs/superpowers/specs/2026-08-11-mcp-structured-content-design.md`

## Global Constraints

- Toolchain pinned to `rust-toolchain.toml` (1.97.0). Use `cargo +1.97.0` for all commands.
- Gates: `cargo +1.97.0 test --workspace --locked` and `cargo +1.97.0 clippy --workspace --locked -- -D warnings` must both pass.
- Workspace deps only: versions live in root `[workspace.dependencies]`; members use `{ workspace = true }`. Add `schemars = "1.2.1"` to the root block (already in Cargo.lock at 1.2.1 via rmcp — no version change).
- Wire frozen: tool names `search`/`extract_url`/`research`/`health`; REST camelCase; MCP snake_case args with camel aliases; error envelope keys `kind`/`message`/`requestId` unchanged. `structuredContent`/`outputSchema` are additive 2026-07-28 fields.
- serpotter-product and serpotter-core stay pure: schemars derive is allowed (derive-only, no runtime logic); no rmcp/axum/http deps.
- `health` tool stays text-only (no structuredContent, no outputSchema) — YAGNI, per spec.
- No `--no-verify`; conventional commits.

---
## File structure

- Modify: `Cargo.toml` (root workspace deps) — add schemars.
- Modify: `crates/serpotter-core/Cargo.toml`, `crates/serpotter-product/Cargo.toml` — add schemars dep.
- Modify: `crates/serpotter-core/src/types.rs` — `#[derive(schemars::JsonSchema)]` on `SearchResponse`, `SearchItem`.
- Modify: `crates/serpotter-product/src/dto.rs` — JsonSchema derives on response DTOs.
- Modify: `crates/serpotter-api/src/mcp/progress.rs` — `text_ok` → `structured_ok`.
- Modify: `crates/serpotter-api/src/mcp/errors.rs` — add `tool_error_structured`.
- Modify: `crates/serpotter-api/src/mcp/mod.rs` — handlers use structured helpers; tool attrs gain `output_schema`; helper fn re-exporting `schema_for_output`.
- Test: `crates/serpotter-api/tests/mcp_stateless.rs` — structuredContent + outputSchema tests.
- Docs: `AGENTS.md`, `docs/ops/api.md` — structuredContent note.

---

### Task 1: JsonSchema derives + schemars deps

**Files:**
- Modify: `Cargo.toml` (root)
- Modify: `crates/serpotter-core/Cargo.toml`, `crates/serpotter-product/Cargo.toml`
- Modify: `crates/serpotter-core/src/types.rs`
- Modify: `crates/serpotter-product/src/dto.rs`

**Interfaces:**
- Produces: `SearchResponse`, `SearchItem` (core) and `ExtractResponse`, `ResearchResponse`, `ScrapedPage`, `Citation`, `Evidence` (product) implement `schemars::JsonSchema`. Cargo.lock gains schemars as a direct dep of core/product (version unchanged 1.2.1).

- [ ] **Step 1: Add schemars to root workspace deps** (`Cargo.toml`, `[workspace.dependencies]` — find the block, add alphabetically near serde):

```toml
schemars = "1.2.1"
```

- [ ] **Step 2: Add the dep to core and product** (`crates/serpotter-core/Cargo.toml` and `crates/serpotter-product/Cargo.toml` [dependencies]):

```toml
schemars = { workspace = true }
```

- [ ] **Step 3: Add derives in core** (`crates/serpotter-core/src/types.rs`) — change the derive lines for `SearchItem` and `SearchResponse` to include `schemars::JsonSchema` (keep existing derives):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct SearchItem { … }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse { … }
```

(Check each struct's exact current derive list and append `schemars::JsonSchema`. If `SearchResponse`/`SearchItem` currently derive `Default` or others, keep them all.)

- [ ] **Step 4: Add derives in product DTOs** (`crates/serpotter-product/src/dto.rs`) — same pattern on `ExtractResponse`, `ResearchResponse`, `ScrapedPage`, `Citation`, `Evidence` (keep `#[serde(rename_all = "camelCase")]` attrs unchanged). Fields with `#[serde(skip_serializing_if = "Option::is_none")]` must stay as-is — schemars reads serde attrs.

- [ ] **Step 5: Build to verify derives compile**

Run: `cargo +1.97.0 build -p serpotter-core -p serpotter-product`
Expected: compiles; `cargo +1.97.0 test -p serpotter-core -p serpotter-product` passes (derive-only, no behavior change).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/serpotter-core/Cargo.toml crates/serpotter-product/Cargo.toml crates/serpotter-core/src/types.rs crates/serpotter-product/src/dto.rs
git commit -m "feat(api): JsonSchema derives on search/extract/research result DTOs"
```

---

### Task 2: Structured helpers in the MCP module

**Files:**
- Modify: `crates/serpotter-api/src/mcp/progress.rs`
- Modify: `crates/serpotter-api/src/mcp/errors.rs`

**Interfaces:**
- Consumes: Task 1 derives (not strictly needed for the helpers — `structured_ok` is generic over `Serialize`).
- Produces:
  - `pub(crate) fn structured_ok<T: serde::Serialize>(value: T, request_id: Option<String>) -> Result<CallToolResult, rmcp::ErrorData>`
  - `pub fn tool_error_structured(kind: &str, message: String, request_id: Option<String>) -> CallToolResult`
  - `text_ok` is **removed** (no other callers — verified: only `progress.rs` defines it, only `mod.rs` used it).

- [ ] **Step 1: Write the failing tests** (append to `crates/serpotter-api/src/mcp/progress.rs` `#[cfg(test)]` module — check if one exists; if not, create it):

```rust
#[cfg(test)]
mod structured_tests {
    use super::*;

    #[test]
    fn structured_ok_carries_both_content_and_structured() {
        let r = structured_ok(serde_json::json!({"a": 1}), None).expect("ok");
        let text = &r.content[0];
        let text_str = match text {
            ContentBlock::Text(t) => t.text.as_str(),
            _ => panic!("expected text block"),
        };
        let structured = r.structured_content.expect("structuredContent present");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text_str).expect("text is JSON"),
            structured,
            "text block and structuredContent must carry identical data"
        );
        assert_eq!(r.is_error, Some(false));
    }

    #[test]
    fn structured_error_carries_envelope_both_ways() {
        let r = tool_error_structured("NoHealthyKey", "search failed: no keys".into(), Some("rid-1".into()));
        assert_eq!(r.is_error, Some(true));
        let structured = r.structured_content.expect("structuredContent present");
        assert_eq!(structured["kind"], "NoHealthyKey");
        assert_eq!(structured["requestId"], "rid-1");
        // text block still carries the envelope for humans
        let text_str = match &r.content[0] {
            ContentBlock::Text(t) => t.text.as_str(),
            _ => panic!("expected text block"),
        };
        let text_v: serde_json::Value = serde_json::from_str(text_str).expect("text is JSON");
        assert_eq!(text_v["kind"], "NoHealthyKey");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo +1.97.0 test -p serpotter-api structured_` (or the lib unit tests)
Expected: FAIL — `structured_ok` / `tool_error_structured` not found.

- [ ] **Step 3: Replace `text_ok` with `structured_ok`** (`progress.rs`) — rename the existing function and change the body:

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

Update the doc comment and the `use` line for `super::errors::tool_error`.

- [ ] **Step 4: Add `tool_error_structured`** (`errors.rs`) — alongside the existing `tool_error`; keep `tool_error` (cancel/validation paths may still want the plain text version, or all sites migrate — Task 3 decides; provide both):

```rust
/// Structured failure: the error envelope `{kind,message,requestId}` lands in
/// `structuredContent` as well as the human text block, so clients can read
/// failures without string extraction.
pub fn tool_error_structured(
    kind: &str,
    message: String,
    request_id: Option<String>,
) -> CallToolResult {
    let value = serde_json::json!({
        "kind": kind,
        "message": message,
        "requestId": request_id,
    });
    let body = serde_json::to_string(&value).unwrap_or_else(|_| {
        r#"{"kind":"InternalError","message":"failed to serialize tool error","requestId":null}"#
            .to_string()
    });
    let mut result = CallToolResult::structured_error(value);
    // structured_error already builds a text block from the value; replace it
    // with the exact same serialization we used for the text body for parity.
    result.content = vec![ContentBlock::text(body)];
    result
}
```

(Adjust so `result.content` carries the same string that today's `tool_error` produces — byte parity with the current text block. If `CallToolResult::structured_error` already produces the identical text, drop the content overwrite. Verify with the unit test.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo +1.97.0 test -p serpotter-api`
Expected: PASS — new unit tests; existing lib tests (note: `mod.rs` still calls `text_ok` — Task 3 fixes that, so `cargo test -p serpotter-api` may fail to compile in Task 2's end state. If so, either keep `text_ok` as a thin alias this task or add a temporary re-export; the task's scope is the helpers + their unit tests, and the build compiles because `mod.rs` still uses `text_ok`. To keep every commit green, keep `text_ok` in this task as a deprecated shim calling `structured_ok(...).map(|r| r)` — Task 3 removes the shim.)

- [ ] **Step 6: Commit**

```bash
git add crates/serpotter-api/src/mcp/progress.rs crates/serpotter-api/src/mcp/errors.rs
git commit -m "feat(mcp): structured_ok and tool_error_structured result helpers"
```

---

### Task 3: Handler wiring + outputSchema advertisement

**Files:**
- Modify: `crates/serpotter-api/src/mcp/mod.rs`
- Test: `crates/serpotter-api/tests/mcp_stateless.rs`

**Interfaces:**
- Consumes: Task 1 derives; Task 2 `structured_ok` / `tool_error_structured`.
- Produces: all three handlers return `CallToolResult` with `structuredContent`; tool attrs advertise `output_schema`; `text_ok` shim removed.

- [ ] **Step 1: Write the failing integration tests** (append to `crates/serpotter-api/tests/mcp_stateless.rs`):

```rust
/// Search result carries structuredContent identical to the text block.
#[tokio::test]
async fn mcp_stateless_search_structured_content() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-structured").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            stateless_body("tools/call", 40, serde_json::json!({"name": "search", "arguments": {"query": "hello", "max_results": 1}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let result = &v["result"];
    let structured = result["structuredContent"].as_object().cloned()
        .unwrap_or_else(|| panic!("structuredContent must be an object: {result}"));
    let text = result["content"][0]["text"].as_str()
        .unwrap_or_else(|| panic!("text block present: {result}"));
    let text_v: serde_json::Value = serde_json::from_str(text).expect("text is JSON");
    assert_eq!(serde_json::Value::Object(structured), text_v, "structured == text");
}

/// Error envelope is machine-readable in structuredContent.
#[tokio::test]
async fn mcp_stateless_error_is_structured() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("extract_url"),
            stateless_body("tools/call", 41, serde_json::json!({"name": "extract_url", "arguments": {"url": ""}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], true);
    assert_eq!(v["result"]["structuredContent"]["kind"], "ValidationError");
}

/// tools/list advertises outputSchema on the three result tools.
#[tokio::test]
async fn mcp_tools_list_advertises_output_schema() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request("tools/list", None, stateless_body("tools/list", 42, serde_json::json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    for name in ["search", "extract_url", "research"] {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("{name} present"));
        let schema = tool["outputSchema"].as_object()
            .unwrap_or_else(|| panic!("{name} outputSchema present"));
        assert_eq!(schema["type"], "object", "{name} outputSchema root type");
    }
    // health: no outputSchema (YAGNI)
    let health = tools.iter().find(|t| t["name"] == "health").expect("health present");
    assert!(health.get("outputSchema").is_none(), "health has no outputSchema");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo +1.97.0 test -p serpotter-api --test mcp_stateless mcp_stateless_search_structured`
Expected: FAIL — `structuredContent` absent; `outputSchema` absent.

- [ ] **Step 3: Wire the handlers** (`mod.rs`): replace every `text_ok(resp, request_id)` call (3 success arms) with `structured_ok(resp, request_id)`; replace every `tool_error(kind, format!(...), request_id)` call with `tool_error_structured(...)` (10 sites: validation, cancelled arms, provider-failure arms). Verify cancellation arms still return `Ok(tool_error_structured("Cancelled", ...))` — the structured envelope is fine there too.

- [ ] **Step 4: Add the `output_schema` helper** (`mod.rs`, near the imports):

```rust
use rmcp::handler::server::tool::schema_for_output;

fn output_schema<T: rmcp::schemars::JsonSchema + std::any::Any>()
    -> std::sync::Arc<serde_json::Map<String, serde_json::Value>> {
    schema_for_output::<T>()
}
```

Verify the exact return type of `schema_for_output::<T>()` (it's `Arc<JsonObject>` where `JsonObject = serde_json::Map<String, Value>`; the macro's `with_raw_output_schema(#s)` accepts it as an expression — match the alias exactly, or return the concrete rmcp type).

- [ ] **Step 5: Add `output_schema` to the three tool attrs** (`mod.rs` — search:153, extract:277, research:378):

```rust
    #[tool(
        description = "Multi-provider web search (routing + key filters: domains, dates, X handles, strategy/provider)",
        annotations(title = "Search", open_world_hint = true, read_only_hint = true),
        output_schema = output_schema::<serpotter_core::SearchResponse>(),
    )]
```

Same for `extract_url` with `crate::…ExtractResponse` (import `ExtractResponse` from `serpotter_product`) and `research` with `ResearchResponse`. `health` (504) unchanged.

- [ ] **Step 6: Remove the `text_ok` shim** (if Task 2 kept one) and clean up imports.

- [ ] **Step 7: Run the tests**

Run: `cargo +1.97.0 test -p serpotter-api --test mcp_stateless`
Expected: PASS — 3 new + existing. Then full api suite: `cargo +1.97.0 test -p serpotter-api`.

- [ ] **Step 8: Full gates**

Run: `cargo +1.97.0 test --workspace --locked && cargo +1.97.0 clippy --workspace --locked -- -D warnings`
Expected: all pass, clippy clean.

- [ ] **Step 9: Commit**

```bash
git add crates/serpotter-api/src/mcp/mod.rs crates/serpotter-api/tests/mcp_stateless.rs
git commit -m "feat(mcp): structuredContent results with advertised outputSchema"
```

---

### Task 4: Docs

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/ops/api.md`

- [ ] **Step 1: `AGENTS.md`** — extend the MCP bullet (the one with progress): append

```
Tools return typed `structuredContent` (camelCase) plus human text; `tools/list` advertises `outputSchema`; `health` stays text-only.
```

- [ ] **Step 2: `docs/ops/api.md`** — under the MCP table, add a row:

```
| Results | `structuredContent` carries the typed camelCase response object (plus human text block); `outputSchema` advertised for search/extract_url/research |
```

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md docs/ops/api.md
git commit -m "docs: MCP structuredContent results contract"
```

---

## Self-review

- **Spec coverage:** JsonSchema derives + deps (T1) ✓; `structured_ok`/`tool_error_structured` helpers with unit tests (T2) ✓; handler wiring + outputSchema attrs + integration tests (T3) ✓; docs (T4) ✓. Health text-only ✓. Cursor pagination documented as non-goal in spec, no task implements fake paging ✓.
- **Placeholders:** none — every step has concrete code or an exact command; the two "verify exact type" notes are legitimate implementation-time checks with the decision rule stated.
- **Type consistency:** `structured_ok<T: Serialize>` is generic — works for all three response types; `output_schema::<T: JsonSchema + Any>` matches `schema_for_output`'s bound; `structured_error` returns `CallToolResult` matching the `Result<CallToolResult, ErrorData>` handler signature; error envelope keys (`kind`,`message`,`requestId`) unchanged across Tasks 2-3.
- **Commit-green rule:** Task 2 keeps `text_ok` as a shim so the tree compiles until Task 3 migrates callers (explicitly noted in the plan).