# Fat-File Module Restructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split fat production source files into cohesive modules under a frozen crate graph so no production `src/**/*.rs` stays above ~350 LOC (tests excluded).

**Architecture:** Approach A — serial waves by crate (product → core → optional providers/pools → db keys → api MCP). Pure file→module moves with `pub use` barriers; no behavior, wire, or schema changes. Hard constraints: MCP `#[tool_router] impl` stays one contiguous block; product `search/` / `extract/` re-export the same public symbols as today.

**Tech Stack:** Rust 2021 workspace, cargo test/clippy, existing unit + `serpotter-api` integration tests. No new deps.

**Spec:** `docs/superpowers/specs/2026-07-28-fat-file-module-restructure-design.md`

## Global Constraints

- **Crate graph frozen** — still the eight members; no new packages; no admin/mcp HTTP peels
- **Production LOC cap ~350** — strip `#[cfg(test)]` modules before measuring; `tests/**` exempt
- **H1 MCP:** never split `#[tool_router] impl SerpotterMcp` across files; peel only free fns/types
- **H2 product:** `search/mod.rs` + root `lib.rs` keep `search_inner`, `run_provider`, `is_exhausted_status`, `first_blend_err`, `multi_leg_errors`, `hybrid_leg_errors`; extract keeps its six `pub use` names
- **No `search/inner.rs`** — `search_inner` lives in `search/mod.rs`
- **Routing** — only `mod.rs` + `rules.rs` + `resolve.rs` (no intent/strategy/match/fallback micro-files)
- **Waves 3–4 optional** if already ≤350 prod after remeasure
- **No behavior / wire / schema** changes; pure moves + re-exports
- **Never** `git commit --no-verify`
- Product purity: no `serpotter-auth` / axum / `AppState` in product
- Prefer `rtk cargo test` / `rtk cargo clippy` when available

## File map (end state)

| Path | Responsibility |
| --- | --- |
| `crates/serpotter-product/src/search/mod.rs` | `search_inner` + `pub use` siblings |
| `crates/serpotter-product/src/search/run_provider.rs` | Dual-pool attempt loop |
| `crates/serpotter-product/src/search/execute.rs` | single / hybrid / blend executors |
| `crates/serpotter-product/src/search/leg_errors.rs` | blend/hybrid soft-merge helpers + tests |
| `crates/serpotter-product/src/search/exhausted.rs` | `is_exhausted_status` + tests |
| `crates/serpotter-product/src/extract/mod.rs` | `pub use` extract surface |
| `crates/serpotter-product/src/extract/extract_url.rs` | extract chain + attempt loop |
| `crates/serpotter-product/src/extract/research.rs` | `research_inner` |
| `crates/serpotter-product/src/extract/helpers.rs` | pure mappers + unit tests |
| `crates/serpotter-product/src/lib.rs` | Unchanged `pub use` names (paths still `search::` / `extract::`) |
| `crates/serpotter-core/src/routing/mod.rs` | types + `route_search` + tests |
| `crates/serpotter-core/src/routing/rules.rs` | `Rule` + `RULES` |
| `crates/serpotter-core/src/routing/resolve.rs` | intent/strategy/match/fallback helpers |
| `crates/serpotter-core/src/lib.rs` | Same routing re-exports |
| `crates/serpotter-db/src/keys/mod.rs` | re-export rows + glue |
| `crates/serpotter-db/src/keys/rows.rs` | row types + mappers |
| `crates/serpotter-db/src/keys/acquire_report.rs` | pool SQL `impl Db` |
| `crates/serpotter-db/src/keys/admin_crud.rs` | admin/list/credits `impl Db` |
| `crates/serpotter-db/src/lib.rs` | `mod keys;` + same `pub use keys::{ApiKeyAdminRow, ApiKeyRow}` |
| `crates/serpotter-api/src/mcp/mod.rs` | `service`, `SerpotterMcp`, full `#[tool_router]` + `#[tool_handler]` |
| `crates/serpotter-api/src/mcp/auth.rs` | `mcp_auth_middleware` |
| `crates/serpotter-api/src/mcp/params.rs` | param DTOs + mappers |
| `crates/serpotter-api/src/mcp/progress.rs` | `soft_progress`, `text_ok` |

**Delete after split:** flat `search.rs`, `extract.rs`, `routing.rs`, `keys.rs` once directory modules replace them.

**Optional (Tasks 5–6):** providers / keypool / outbound peels — only if remeasure says over cap or trivial test move.

---

### Task 1: Product `search/` split

**Files:**
- Create: `crates/serpotter-product/src/search/mod.rs`
- Create: `crates/serpotter-product/src/search/run_provider.rs`
- Create: `crates/serpotter-product/src/search/execute.rs`
- Create: `crates/serpotter-product/src/search/leg_errors.rs`
- Create: `crates/serpotter-product/src/search/exhausted.rs`
- Delete: `crates/serpotter-product/src/search.rs` (after directory exists)
- Modify: none required in `lib.rs` if it already has `mod search;` + existing `pub use search::{…}` (directory `search/` replaces `search.rs`)

**Interfaces:**
- Consumes: `ProductCtx`, `SearchExecError`, `KeyHold`/`ProxyHold`, `classify_proxied_http`, core `route_search` / `fallback_chain` / RRF, providers types
- Produces (must remain reachable as `crate::search::NAME` and `serpotter_product::NAME`):
  - `pub async fn search_inner(ctx: &ProductCtx, body: SearchQuery) -> Result<SearchResponse, SearchExecError>`
  - `pub async fn run_provider(...)` — same signature as today (`#[allow(clippy::too_many_arguments)]`)
  - `pub fn is_exhausted_status(provider: &str, status: u16) -> bool`
  - `pub fn first_blend_err(a, b, c: Option<SearchExecError>) -> SearchExecError`
  - `pub fn multi_leg_errors<'a, I>(...) -> Option<Vec<String>>`
  - `pub fn hybrid_leg_errors(...) -> Option<Vec<String>>`
- Intra: `execute.rs` calls `run_provider`; `mod.rs` `search_inner` calls execute_*; extract later uses `crate::search::{run_provider, is_exhausted_status, search_inner}`

- [ ] **Step 1: Create `search/exhausted.rs`**

Move verbatim from current `search.rs` lines ~558–599 (`is_exhausted_status` + `exhausted_tests`). File should look like:

```rust
//! Exhausted HTTP status parity (mysearch).

/// Mysearch `EXHAUSTED_STATUS` / `isExhaustedStatus` parity.
/// Credit/plan limits → `report_exhausted` (not consecutive fail).
pub fn is_exhausted_status(provider: &str, status: u16) -> bool {
    match provider {
        "tavily" => matches!(status, 429 | 432 | 433),
        "firecrawl" | "exa" => matches!(status, 402 | 429),
        "xai" => status == 429,
        _ => status == 402,
    }
}

#[cfg(test)]
mod exhausted_tests {
    use super::is_exhausted_status;
    // ... existing tests unchanged ...
}
```

- [ ] **Step 2: Create `search/leg_errors.rs`**

Move `first_blend_err`, `multi_leg_errors`, `hybrid_leg_errors` + `blend_err_tests` (~265–304 and ~601–679). Imports: `crate::error::SearchExecError` (or `crate::SearchExecError` if re-exported — match current test which uses `crate::SearchExecError`).

- [ ] **Step 3: Create `search/run_provider.rs`**

Move `run_provider` (~303–492). Keep all imports needed (`KeyPoolError`, `ProviderError`, `ProviderSearchParams`, `KeyHold`, `ProxyHold`, `SVC_XAI`, `is_exhausted_status` from `super::exhausted` or `crate::search::exhausted` — prefer `super::is_exhausted_status` after mod re-export, or `super::exhausted::is_exhausted_status`).

Use:

```rust
use super::is_exhausted_status;
// or
use super::exhausted::is_exhausted_status;
```

Do not change the report matrix logic.

- [ ] **Step 4: Create `search/execute.rs`**

Move `execute_single_chain`, `execute_hybrid`, `execute_blend` (~17–263). They call `run_provider` and leg error helpers via `super::`.

```rust
use super::run_provider;
use super::{first_blend_err, multi_leg_errors /*, hybrid if needed */};
```

- [ ] **Step 5: Create `search/mod.rs` with `search_inner` + re-exports**

```rust
//! Search orchestration (multi-provider routing + RRF). No HTTP / auth.

mod exhausted;
mod execute;
mod leg_errors;
mod run_provider;

pub use exhausted::is_exhausted_status;
pub use leg_errors::{first_blend_err, hybrid_leg_errors, multi_leg_errors};
pub use run_provider::run_provider;

use serpotter_core::{route_search, RouteDebug, RouteInput, SearchQuery, SearchResponse};
use crate::error::SearchExecError;
use crate::ProductCtx;

use execute::{execute_blend, execute_hybrid, execute_single_chain};

/// Public search used by HTTP handlers / MCP / research (auth already checked).
pub async fn search_inner(
    ctx: &ProductCtx,
    body: SearchQuery,
) -> Result<SearchResponse, SearchExecError> {
    // body: move verbatim from old search.rs search_inner (~495–556)
    // ...
}
```

- [ ] **Step 6: Remove flat `search.rs`**

Ensure only `search/` directory remains (Rust will not allow both `search.rs` and `search/`).

- [ ] **Step 7: Verify root `lib.rs` still has**

```rust
mod search;
// ...
pub use search::{
    first_blend_err, hybrid_leg_errors, is_exhausted_status, multi_leg_errors, run_provider,
    search_inner,
};
```

Do not change names.

- [ ] **Step 8: Gate**

```bash
rtk cargo test -p serpotter-product
rtk cargo clippy -p serpotter-product -- -D warnings
```

Expected: all product unit tests pass (exhausted + blend_err).

- [ ] **Step 9: Commit**

```bash
rtk git add crates/serpotter-product/src/search crates/serpotter-product/src/search.rs crates/serpotter-product/src/lib.rs
rtk git commit -m "refactor(product): split search into cohesion modules"
```

(If `search.rs` deleted, stage the deletion.)

---

### Task 2: Product `extract/` split + wave-1 consumer gate

**Files:**
- Create: `crates/serpotter-product/src/extract/mod.rs`
- Create: `crates/serpotter-product/src/extract/extract_url.rs`
- Create: `crates/serpotter-product/src/extract/research.rs`
- Create: `crates/serpotter-product/src/extract/helpers.rs`
- Delete: `crates/serpotter-product/src/extract.rs`
- Modify: only if `lib.rs` needs touch (prefer unchanged `mod extract;` + `pub use`)

**Interfaces:**
- Consumes: `crate::search::{is_exhausted_status, run_provider, search_inner}` (H2 — must compile after Task 1)
- Produces (`crate::extract::` / root `pub use`):
  - `extract_url`, `research_inner`
  - `merge_providers_consulted`, `select_scrape_targets`, `scraped_page_from_extract`, `map_social_leg`

- [ ] **Step 1: Create `extract/helpers.rs`**

Move pure helpers + their `#[cfg(test)]` modules from ~353–528:

- `merge_providers_consulted`
- `select_scrape_targets`
- `scraped_page_from_extract`
- `map_social_leg`
- tests: `social_leg_tests`, `providers_consulted_tests`, `scrape_mapper_tests`

- [ ] **Step 2: Create `extract/extract_url.rs`**

Move `extract_url`, `try_extract_provider`, `to_response` (~17–186). Keep:

```rust
use crate::search::is_exhausted_status;
// KeyHold/ProxyHold, providers, ssrf, etc.
```

- [ ] **Step 3: Create `extract/research.rs`**

Move `research_inner` (~188–351). Imports:

```rust
use crate::search::{run_provider, search_inner};
use super::extract_url::extract_url; // or super::extract_url if re-exported
use super::helpers::{
    map_social_leg, merge_providers_consulted, scraped_page_from_extract, select_scrape_targets,
};
```

- [ ] **Step 4: Create `extract/mod.rs`**

```rust
//! Extract / research orchestration. No HTTP / auth.

mod extract_url;
mod helpers;
mod research;

pub use extract_url::extract_url;
pub use helpers::{
    map_social_leg, merge_providers_consulted, scraped_page_from_extract, select_scrape_targets,
};
pub use research::research_inner;
```

- [ ] **Step 5: Delete flat `extract.rs`**

- [ ] **Step 6: Confirm root `lib.rs`**

```rust
pub use extract::{
    extract_url, map_social_leg, merge_providers_consulted, research_inner,
    scraped_page_from_extract, select_scrape_targets,
};
```

- [ ] **Step 7: Wave-1 gates (product + api consumers)**

```bash
rtk cargo test -p serpotter-product
rtk cargo clippy -p serpotter-product -- -D warnings
rtk cargo test -p serpotter-api --test extract_research --test search_auth --test mcp_tools --test mcp_session
rtk cargo clippy -p serpotter-api -- -D warnings
```

Expected: PASS (H2 acceptance).

- [ ] **Step 8: Commit**

```bash
rtk git add crates/serpotter-product/src/extract crates/serpotter-product/src/extract.rs
rtk git commit -m "refactor(product): split extract and research modules"
```

---

### Task 3: Core `routing/` split

**Files:**
- Create: `crates/serpotter-core/src/routing/mod.rs`
- Create: `crates/serpotter-core/src/routing/rules.rs`
- Create: `crates/serpotter-core/src/routing/resolve.rs`
- Delete: `crates/serpotter-core/src/routing.rs`
- Modify: `crates/serpotter-core/src/lib.rs` only if re-export list must stay identical (prefer no change)

**Interfaces:**
- Consumes: `crate::types::SearchQuery` (and `Sources`/`VecOrOne` in tests)
- Produces (crate root re-exports today):
  - `fallback_chain`, `resolve_strategy`, `route_search`, `RouteDecision`, `RouteInput`, `Strategy`
  - Also keep `pub fn resolve_intent` public on the routing module if it was `pub` (even if not in root `pub use`)

- [ ] **Step 1: Create `routing/rules.rs`**

Move `struct Rule` + `const RULES: &[Rule] = &[ … ];` (~42–184). Make `Rule` and `RULES` `pub(crate)` so `mod.rs` / `resolve` can use them:

```rust
pub(crate) struct Rule { /* fields unchanged */ }
pub(crate) const RULES: &[Rule] = &[ /* unchanged table */ ];
```

- [ ] **Step 2: Create `routing/resolve.rs`**

Move:

- `resolve_intent`
- `resolve_strategy`
- `has_any`
- `sources_list`
- `rule_matches`
- `fallback_chain`

`rule_matches` needs `Rule` from `super::rules::Rule`. `fallback_chain` stays `pub`.

```rust
use super::rules::Rule;
use super::Strategy;
use crate::types::SearchQuery;

pub fn resolve_intent(q: &SearchQuery) -> String { /* verbatim */ }
pub fn resolve_strategy(q: &SearchQuery, intent: &str, hybrid: bool) -> Strategy { /* verbatim */ }
pub(crate) fn has_any(...) -> bool { ... }
pub(crate) fn sources_list(...) -> Vec<String> { ... }
pub(crate) fn rule_matches(...) -> bool { ... }
pub fn fallback_chain(provider: &str) -> Vec<&'static str> { ... }
```

Note: `Strategy` is defined in `mod.rs` — either define `Strategy` in `mod.rs` and have `resolve` use `super::Strategy`, **or** put `Strategy` in a tiny shared place. Spec: types + `route_search` in `mod.rs`. So `resolve.rs` uses `super::Strategy`.

If compile order is awkward, define `Strategy` + `RouteDecision` + `RouteInput` in `mod.rs` first; `resolve` only needs `Strategy`.

- [ ] **Step 3: Create `routing/mod.rs`**

```rust
//! 6-gate search routing (mysearch routing.ts lean port).

mod resolve;
mod rules;

pub use resolve::{fallback_chain, resolve_intent, resolve_strategy};

use crate::types::SearchQuery;
use resolve::{rule_matches, sources_list};
use rules::RULES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy { /* Fast, Balanced, Verify, Deep + as_str */ }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision { /* fields unchanged */ }

#[derive(Debug, Clone)]
pub struct RouteInput<'a> {
    pub query: &'a SearchQuery,
}

pub fn route_search(input: RouteInput<'_>) -> RouteDecision {
    // gates verbatim from old routing.rs ~351–478
}

#[cfg(test)]
mod tests {
    // move existing tests; use super::*
}
```

- [ ] **Step 4: Delete flat `routing.rs`**

- [ ] **Step 5: Confirm `crates/serpotter-core/src/lib.rs`**

```rust
pub use routing::{
    fallback_chain, resolve_strategy, route_search, RouteDecision, RouteInput, Strategy,
};
```

Unchanged names. (`resolve_intent` may stay routing-only `pub` without root re-export — match prior public surface of root crate.)

- [ ] **Step 6: Gate**

```bash
rtk cargo test -p serpotter-core
rtk cargo clippy -p serpotter-core -- -D warnings
rtk cargo test -p serpotter-product
```

Expected: routing unit tests (`explicit_provider`, `news_mode_tavily_topic`, `hybrid_web_x`, `handle_filter_routes_xai`, `bare_web_query_not_xai`, `fallback_chain_tavily`) pass; product still builds.

- [ ] **Step 7: Commit**

```bash
rtk git add crates/serpotter-core/src/routing crates/serpotter-core/src/routing.rs crates/serpotter-core/src/lib.rs
rtk git commit -m "refactor(core): split routing into rules and resolve"
```

---

### Task 4: Remeasure optional waves (providers / keypool / outbound)

**Files:** none until remeasure says yes.

**Interfaces:** N/A decision task.

- [ ] **Step 1: Remeasure production LOC**

```bash
# Prefer the same approach as design: strip #[cfg(test)] modules, count remaining lines
# Targets:
#   crates/serpotter-providers/src/{lib,firecrawl,xai,tavily,exa,http,usage}.rs
#   crates/serpotter-keypool/src/lib.rs
#   crates/serpotter-outbound/src/lib.rs
```

- [ ] **Step 2: Decision**

| If | Then |
| --- | --- |
| All ≤350 prod | **Skip Tasks 5–6 content** — record “skipped under cap” in commit message of a docs-only note **or** simply proceed to Task 7 with no providers/pool commits |
| Any >350 prod | Execute optional peels below for **only** over-cap files |

Optional peel recipes (only if needed):

**Providers:** peel `ProviderError` / `ProviderSearchParams` / `ProviderResult` / `ExtractResult` from `lib.rs` → `types.rs`; `pub use` from `lib.rs`. Move large `#[cfg(test)]` blocks to sibling `*_tests` only if prod body still over.

**Keypool / outbound:** move `#[cfg(test)] mod tests` to `src/tests.rs` with `#[cfg(test)] mod tests;` in `lib.rs` — often enough since prod is already ~200.

- [ ] **Step 3: If any peel applied, gate + commit per crate**

```bash
rtk cargo test -p serpotter-providers   # or keypool / outbound
rtk cargo clippy -p <crate> -- -D warnings
rtk git commit -m "refactor(<crate>): module peel under loc cap"
```

If skipped:

- [ ] **Step 4: No code commit required** — continue to Task 5 (db).

---

### Task 5: DB `keys/` split

**Files:**
- Create: `crates/serpotter-db/src/keys/mod.rs`
- Create: `crates/serpotter-db/src/keys/rows.rs`
- Create: `crates/serpotter-db/src/keys/acquire_report.rs`
- Create: `crates/serpotter-db/src/keys/admin_crud.rs`
- Delete: `crates/serpotter-db/src/keys.rs`
- Modify: `crates/serpotter-db/src/lib.rs` only if needed (`mod keys;` already; keep `pub use keys::{ApiKeyAdminRow, ApiKeyRow}`)

**Interfaces:**
- Consumes: `crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES}`, sqlx
- Produces: same `impl Db` method names (no renames). Row types `ApiKeyRow`, `ApiKeyAdminRow` still at `serpotter_db::{ApiKeyRow, ApiKeyAdminRow}`.

**Method split (by current `keys.rs` concerns):**

`acquire_report.rs` — pool / hold path (approx lines 67–221 + report helpers + last_used):

- `reclaim_expired_key_holds`
- `zero_all_key_inflight`
- `acquire_api_key_shared`
- report / failure / exhausted / release methods currently in the first `impl Db` block after acquire
- `set_api_key_lease_until`
- `list_active_keys_for_service`
- `get_api_key` (if present for pool path — keep with whoever owns it today by call sites; prefer pool if used by keypool)
- `set_api_key_last_used_at`

`admin_crud.rs` — admin / credits:

- `insert_api_key`
- `set_api_key_credits`
- `update_api_key_usage`
- `list_api_keys`
- `get_api_key_admin` / detail if any
- `delete_api_key` / `set_api_key_active` (toggle)
- `count_api_keys`, `count_active_api_keys`

If a method is ambiguous, open call sites with:

```bash
rg -n "fn_name" crates/
```

and place with the dominant consumer (keypool → acquire_report; admin API → admin_crud).

`rows.rs`:

```rust
pub struct ApiKeyRow { ... }
pub struct ApiKeyAdminRow { ... }
pub(crate) fn map_api_key_admin_row(...) -> Result<ApiKeyAdminRow, DbError> { ... }
```

`mod.rs`:

```rust
mod acquire_report;
mod admin_crud;
mod rows;

pub use rows::{ApiKeyAdminRow, ApiKeyRow};
// multiple impl Db blocks live in acquire_report.rs and admin_crud.rs — no re-export of methods needed
```

- [ ] **Step 1: Create `keys/rows.rs`** — move structs + mapper

- [ ] **Step 2: Create `keys/acquire_report.rs`** — `impl Db { … pool methods … }`

```rust
use super::rows::ApiKeyRow;
use crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES};
// ...
impl Db {
    // pool methods verbatim
}
```

- [ ] **Step 3: Create `keys/admin_crud.rs`** — second `impl Db` block

```rust
use super::rows::{map_api_key_admin_row, ApiKeyAdminRow, ApiKeyRow};
use crate::{Db, DbError};
impl Db {
    // admin methods verbatim
}
```

- [ ] **Step 4: Create `keys/mod.rs` + delete `keys.rs`**

- [ ] **Step 5: Confirm `lib.rs`**

```rust
mod keys;
pub use keys::{ApiKeyAdminRow, ApiKeyRow};
```

- [ ] **Step 6: Gate**

```bash
rtk cargo test -p serpotter-db
rtk cargo test -p serpotter-keypool
rtk cargo test -p serpotter-api --test admin_keys_credits
rtk cargo clippy -p serpotter-db -- -D warnings
```

Expected: PASS; method names unchanged so keypool/admin compile without call-site edits.

- [ ] **Step 7: Commit**

```bash
rtk git add crates/serpotter-db/src/keys crates/serpotter-db/src/keys.rs crates/serpotter-db/src/lib.rs
rtk git commit -m "refactor(db): split keys into rows acquire_report admin_crud"
```

**Nodes:** skip unless `nodes.rs` prod LOC >350 after remeasure (design: optional). If splitting, mirror keys pattern in a follow-up commit — not required for acceptance if under cap.

---

### Task 6: API MCP peel (H1)

**Files:**
- Create: `crates/serpotter-api/src/mcp/auth.rs`
- Create: `crates/serpotter-api/src/mcp/params.rs`
- Create: `crates/serpotter-api/src/mcp/progress.rs`
- Modify: `crates/serpotter-api/src/mcp/mod.rs` (shrink; keep tool impl)

**Interfaces:**
- Consumes: `AppState`, `require_api_token`, `ProductCtx`, product free-fns, `log_request`, error log helpers
- Produces: `pub fn service(state: AppState) -> impl Service<…>` unchanged mount behavior; tools `search`, `extract_url`, `research`, `health` still registered

- [ ] **Step 1: Create `mcp/auth.rs`**

Move `mcp_auth_middleware` (~79–88):

```rust
use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::body::Body;
use axum::response::Response;
use crate::{require_api_token, AppState};

pub async fn mcp_auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Err(r) = require_api_token(&state, request.headers()).await {
        return r;
    }
    next.run(request).await
}
```

- [ ] **Step 2: Create `mcp/params.rs`**

Move from ~107–275:

- `McpStringList` + impl
- `mcp_list_field`, `mcp_list_to_vec_or_one`, `search_params_to_query`
- `SearchParams`, `ExtractParams`, `ResearchParams`

These types are used only by the `#[tool_router] impl` in `mod.rs`. Make them `pub(crate)` so `mod.rs` can import:

```rust
pub(crate) struct SearchParams { ... }
// etc.
```

- [ ] **Step 3: Create `mcp/progress.rs`**

Move `soft_progress` and `text_ok` (~492–529). Adjust visibility to `pub(crate)`.

- [ ] **Step 4: Rewrite `mcp/mod.rs` skeleton**

Keep **in this file only**:

1. Module docs, imports
2. `mod auth; mod params; mod progress;`
3. `MCP_SESSION_HEADER`, `MCP_SESSION_TTL_SECS`
4. `service()` — use `auth::mcp_auth_middleware`
5. `struct SerpotterMcp` + `impl SerpotterMcp { fn new … }`
6. **Entire** `#[tool_router] impl SerpotterMcp { search; extract_url; research; health }` — do not move methods
7. `#[tool_handler(...)] impl ServerHandler for SerpotterMcp {}`

Wire imports:

```rust
mod auth;
mod params;
mod progress;

use auth::mcp_auth_middleware;
use params::{
    mcp_list_to_vec_or_one, search_params_to_query, ExtractParams, ResearchParams, SearchParams,
};
use progress::{soft_progress, text_ok};
```

**Forbidden:** creating `tools.rs` with a second `#[tool_router] impl`.

- [ ] **Step 5: Gate**

```bash
rtk cargo test -p serpotter-api --test mcp_tools --test mcp_session
rtk cargo clippy -p serpotter-api -- -D warnings
```

Expected: MCP tools still register; session tests pass.

- [ ] **Step 6: Commit**

```bash
rtk git add crates/serpotter-api/src/mcp
rtk git commit -m "refactor(api): peel mcp auth params progress under tool_router constraint"
```

---

### Task 7: Final workspace gate + LOC audit

**Files:** none (verify only); optional touch `AGENTS.md` code map paths if they still say `search.rs` / single `routing.rs` / single `mcp/mod.rs` as sole file — only if those docs claim flat files.

- [ ] **Step 1: Full quality gate**

```bash
rtk cargo test --workspace
rtk cargo clippy --workspace -- -D warnings
```

Expected: PASS (matches CI rust job).

- [ ] **Step 2: Production LOC audit**

Re-run the design’s prod-LOC measurement on all touched crates. Confirm no production `src` file > ~350.

Mandatory must be under cap:

- product `search/*`, `extract/*`
- core `routing/*`
- db `keys/*`
- api `mcp/mod.rs` (after peel) and siblings

- [ ] **Step 3: Smoke public surfaces**

```bash
rg -n "pub use search::|pub use extract::" crates/serpotter-product/src/lib.rs
rg -n "pub use routing::" crates/serpotter-core/src/lib.rs
rg -n "pub use keys::" crates/serpotter-db/src/lib.rs
```

Confirm symbol lists match the design H2 / core / db sections.

- [ ] **Step 4: Optional docs**

If `AGENTS.md` or crate `AGENTS.md` still points at deleted flat paths, update path strings only (no redesign). Commit:

```bash
rtk git commit -m "docs: update module paths after fat-file restructure"
```

- [ ] **Step 5: Final commit only if audit script/docs changed; else done**

---

## Spec coverage checklist

| Spec requirement | Task |
| --- | --- |
| Wave 1 product search split (no `inner.rs`) | Task 1 |
| Wave 1 product extract split | Task 2 |
| H2 re-exports + api consumer gate | Task 2 Step 7 |
| Wave 2 routing `mod`+`rules`+`resolve` | Task 3 |
| Waves 3–4 optional under cap | Task 4 |
| Wave 5 db keys multi-impl | Task 5 |
| Wave 6 MCP H1 peel | Task 6 |
| Prod-only ~350 cap + workspace green | Task 7 |
| Frozen graph / no wire/schema | Global + every task “verbatim move” |
| No shared attempt engine | Non-goal; Tasks 1–2 keep two loops |

## Plan self-review

1. **Spec coverage:** All mandatory waves have tasks; optional waves gated by remeasure.
2. **Placeholders:** None — moves cite current symbol names and gate commands.
3. **Type consistency:** Public symbol names match design H2 and current `lib.rs` re-exports; MCP tools stay on one `impl`.
4. **TDD note:** This program is pure moves; “tests” are existing unit/integration suites used as compile+behavior gates (no new contract tests that assert module paths).

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-28-fat-file-module-restructure.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)
2. **Parallel Independent Domains** — only where tasks do not share write targets (this plan is mostly serial by wave; parallel is a poor fit except Task 4 skip + docs)

**Which approach?**
