# Serpotter Fat-File Module Restructure Design

**Date:** 2026-07-28  
**Status:** Approved for implementation planning  
**Scope:** Intra-crate module splits only; crate graph frozen

## Problem

The 2026-07-22 restructure already landed the target crate graph:

- `serpotter-core` / `auth` / `db` / `keypool` / `outbound` / `providers` / `product` / `api`
- Pure `serpotter-product` (no auth/axum/`AppState`)
- Thin API shells under `admin/` / `mcp/` / `product/`
- Multi-module `serpotter-db`

Post-growth (MCP rmcp, dual-pool, research, residual polish, schema v10), several **production** source files are hard to navigate. Pain is **file size and cohesion**, not missing crates or wrong package edges.

Measured production LOC (strip `#[cfg(test)]` modules before counting):

| File | Prod LOC | Total | Cap |
| --- | --- | --- | --- |
| `product/src/search.rs` | ~569 | 679 | over |
| `api/src/mcp/mod.rs` | ~529 | 529 | over |
| `core/src/routing.rs` | ~479 | 548 | over |
| `product/src/extract.rs` | ~425 | 528 | over |
| `db/src/keys.rs` | ~409 | 409 | over |
| `providers/src/firecrawl.rs` | ~307 | 406 | under |
| `providers/src/xai.rs` | ~278 | 365 | under |
| `api/src/admin/keys.rs` | ~267 | 267 | under |
| `db/src/nodes.rs` | ~241 | 241 | under |
| `keypool` / `outbound` `lib.rs` | ~203 / ~204 | 471 / 391 | under (tests inflate total) |

## Goals

1. Every production `src/**/*.rs` has **one clear job** (cohesion first).
2. Soft cap **~350 production LOC** per file after cohesion peels (tests excluded from the count).
3. **Crate graph unchanged** — still eight workspace members; no new packages.
4. **Public symbol names stable** — root `pub use` and external call sites keep compiling with minimal churn.
5. Ship wave-by-crate with a green gate between waves.

## Non-goals

- New crates or merging small crates (`auth` / `keypool` / `outbound` stay).
- Peeling `admin` / `mcp` / product HTTP into separate crates (would reintroduce `AppState` cycles).
- Shared generic “attempt loop” abstraction across search/extract (YAGNI; two explicit loops).
- Wire path/JSON/tool renames, schema migrations, or behavior changes.
- Inventing `search/inner.rs` for a thin `search_inner` dispatcher.
- Splitting MCP `#[tool]` methods across multiple `impl` blocks/files.
- Hard rewrite of tests for style — only move with their symbols.
- Cap enforcement on `tests/**` integration suites or pure `#[cfg(test)]` modules.

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Approach | **A — wave by crate** (serial; green between waves) |
| Cap metric | **Production LOC only** (strip `#[cfg(test)]` modules) |
| Soft cap | ~350 lines |
| Product search layout | `search/mod.rs` holds `search_inner` + re-exports; siblings: `run_provider`, `execute`, `leg_errors`, `exhausted` — **no `inner.rs`** |
| Product extract layout | `extract/mod.rs` + `extract_url` / `research` / `helpers` |
| Core routing layout | `routing/mod.rs` + `rules.rs` + `resolve.rs` only (no micro-files per helper) |
| MCP layout | Peel `auth` / `params` / `progress`; **one contiguous** `#[tool_router] impl` stays in `mod.rs` |
| Waves 3–4 | **Optional** if already ≤350 prod (cohesion/test peel only) |
| Wave 5 | Split `keys/` if still over cap; `nodes/` only if needed or for symmetry |
| Public API | Name-stable `pub use` barriers at crate/`mod.rs` |
| Product purity | Unchanged — no auth/axum/`AppState` in product |

## Hard constraints

### H1 — MCP `#[tool_router]` is atomic

rmcp builds the tool router from **one** contiguous `#[tool_router] impl SerpotterMcp { … }` (today ~lines 277–490 in `mcp/mod.rs`). That block generates `Self::tool_router()`.

- **Do not** split `#[tool]` methods across files or sibling `impl`s.
- **Do** peel free functions and types only:
  - `auth.rs` — `mcp_auth_middleware`
  - `params.rs` — `McpStringList`, `SearchParams`, `ExtractParams`, `ResearchParams`, mappers
  - `progress.rs` — `soft_progress`, `text_ok`
- Keep `service()`, `SerpotterMcp`, the full `#[tool_router] impl`, and `#[tool_handler]` in `mcp/mod.rs`.

### H2 — Product re-export surface after `search/` / `extract/` split

`extract` / research already depend on:

```rust
use crate::search::{is_exhausted_status, run_provider, search_inner};
```

Root `lib.rs` today re-exports:

```rust
pub use search::{
    first_blend_err, hybrid_leg_errors, is_exhausted_status, multi_leg_errors,
    run_provider, search_inner,
};
pub use extract::{
    extract_url, map_social_leg, merge_providers_consulted, research_inner,
    scraped_page_from_extract, select_scrape_targets,
};
```

**Wave 1 is not done unless:**

1. `search/mod.rs` `pub use`s at least those six search symbols (and holds `search_inner`).
2. `extract/mod.rs` `pub use`s the six extract symbols.
3. Root `lib.rs` `pub use` **paths and names unchanged** (only the module path behind them changes: `search.rs` → `search/`).
4. Intra-crate call sites keep `crate::search::…` / `crate::extract::…` (no deep paths like `crate::search::run_provider::run_provider` at call sites).
5. `cargo test -p serpotter-product` and api product/mcp suites that call those symbols stay green.

## Architecture

```text
Frozen crate graph (no peels):

  core ← providers
  db ← keypool
  db ← outbound
  core + db + keypool + outbound + providers ← product
  all ← api  (binary + admin/mcp/product shells)

Only change: files → modules inside existing crates.
```

### Wave order

| Wave | Crate | Why |
| --- | --- | --- |
| 1 | `serpotter-product` | Largest orchestration; pure; re-export consumers in api |
| 2 | `serpotter-core` | `routing` only real fat; product depends on it |
| 3 | `serpotter-providers` | Optional if ≤350 prod after remeasure |
| 4 | `serpotter-keypool` + `serpotter-outbound` | Optional; already ~200 prod |
| 5 | `serpotter-db` | `keys` over cap; multi-`impl Db` pattern already exists |
| 6 | `serpotter-api` | MCP peel last (highest external/test surface) |

`serpotter-auth` (~157 LOC) and already-thin product helpers (`hold`, `report`, `ssrf`, `dto`, `error`) are out of scope.

## Module maps

### Wave 1 — product

```text
search/
  mod.rs           // search_inner + pub use of siblings
  run_provider.rs  // attempt loop + dual-pool report matrix
  execute.rs       // execute_single_chain, execute_hybrid, execute_blend
  leg_errors.rs    // first_blend_err, multi_leg_errors, hybrid_leg_errors (+ tests)
  exhausted.rs     // is_exhausted_status (+ tests)

extract/
  mod.rs           // pub use extract_url, research, helpers
  extract_url.rs   // extract_url, try_extract_provider, to_response
  research.rs      // research_inner
  helpers.rs       // merge_providers_consulted, select_scrape_targets,
                   // scraped_page_from_extract, map_social_leg (+ unit tests)
```

Rationale: `search_inner` is already a ~60-line dispatcher — it stays in `mod.rs`. No shared attempt engine with extract.

### Wave 2 — core routing

```text
routing/
  mod.rs      // Strategy, RouteDecision, RouteInput, route_search (6 gates),
              // pub use resolve::*; tests here or tests.rs if needed
  rules.rs    // Rule + RULES table
  resolve.rs  // resolve_intent, resolve_strategy, has_any, sources_list,
              // rule_matches, fallback_chain
```

Do **not** split into separate `intent` / `strategy` / `match_rule` / `fallback` files — those helpers are each small; separate files add navigation tax without cohesion gain. `route_search` stays the readable spine in `mod.rs`.

Crate `lib.rs` continues to re-export `route_search`, `fallback_chain`, `Strategy`, `RouteDecision`, `RouteInput` as today.

### Wave 3 — providers (optional if under cap)

Already multi-file. Only act if production LOC still >350 or for clear cohesion:

| File | Action if needed |
| --- | --- |
| `lib.rs` | Peel `ProviderError` / params / results → `types.rs`; registry stays thin |
| `firecrawl.rs` / `xai.rs` | Move unit tests out; peel pure helpers only if prod body still over cap |
| `tavily` / `exa` / `http` / `usage` | Leave unless over cap |

No per-provider crates.

### Wave 4 — keypool + outbound (optional)

Both are ~200 production LOC; total is inflated by inline tests. Optional symmetry peel:

```text
src/
  lib.rs     // re-exports + thin surface
  error.rs   // *PoolError
  pool.rs    // struct + acquire/report
  env.rs     // env helpers / Fixed parse (as applicable)
  url.rs     // outbound: proxy_url_from_node + encode (if peeled)
  tests.rs   // #[cfg(test)] moved out of pool file
```

Not mandatory for the soft cap if prod already ≤350.

### Wave 5 — db

```text
keys/   (if still >350 prod)
  mod.rs            // re-export; row types as needed
  rows.rs           // ApiKeyRow, ApiKeyAdminRow, mappers
  acquire_report.rs // acquire / report_* / inflight / lease SQL
  admin_crud.rs     // list/insert/toggle/delete/credits admin path

nodes/  (optional symmetry)
  mod.rs / rows.rs / pool_sql.rs / admin_crud.rs
```

Public remains `impl Db` methods via re-exports and multiple `impl Db` blocks — **no call-site renames**.

### Wave 6 — api MCP (+ admin only if needed)

```text
mcp/
  mod.rs       // service(), constants, SerpotterMcp,
               // ENTIRE #[tool_router] impl (all tools),
               // #[tool_handler] ServerHandler
  auth.rs      // mcp_auth_middleware
  params.rs    // param DTOs + mappers
  progress.rs  // soft_progress, text_ok
```

Admin handlers are already under cap; split only if a file creeps over after growth (e.g. peel DTOs from `keys.rs`). No new admin crate.

## Method (every wave)

1. **Cohesion cut first** — name modules by job, not `part1`/`part2`.
2. **Remeasure production LOC** (exclude `#[cfg(test)]`).
3. If still **> ~350** production — one more peel of the largest remaining concern.
4. **Re-export barrier** at every new `mod.rs` so external + intra-crate paths stay stable.
5. **Green gate** before the next wave:
   - `cargo test -p <crate>`
   - `cargo clippy -p <crate> -- -D warnings`
   - After wave 1: also api product + mcp test suites (re-export consumers).
6. **No behavior change** — pure moves + `pub use`; no dual-pool, routing logic, or wire edits.

End of program: `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`.

## Testing

| Gate | What |
| --- | --- |
| Per-wave unit | Existing unit tests move with symbols; must pass in new modules |
| Wave 1 extra | `cargo test -p serpotter-api` covering product REST + MCP (consumers of product `pub use`) |
| Wave 6 | `mcp_tools` + `mcp_session` integration tests |
| Final | Full workspace test + clippy `-D warnings` |

No new contract tests unless a split accidentally drops a public symbol (caught by compile + existing suites). Do not invent tests that only assert module paths.

## Risks

| Risk | Mitigation |
| --- | --- |
| rmcp macro breaks if tools split | **H1**: never split `#[tool_router] impl` |
| extract/research break after `search/` | **H2**: re-export checklist + api tests in wave 1 gate |
| Over-split thin dispatcher | No `inner.rs`; `search_inner` stays in `search/mod.rs` |
| Cap counted with tests | Production-only LOC rule |
| Routing micro-files | Collapsed to `mod` + `rules` + `resolve` |
| Drive-by wire/schema | Explicit freeze; pure moves only |
| Shared attempt-loop abstraction | Explicit non-goal |
| Optional waves become scope creep | Waves 3–4 skip if already under cap unless cohesion peel is trivial |

## Acceptance criteria

1. Mandatory over-cap files (product search/extract, core routing, db keys if still over, mcp mod) are split per maps above.
2. No production `src` file remains **> ~350 LOC** after excluding `#[cfg(test)]` (integration tests exempt).
3. Crate graph and package names unchanged.
4. Root product `pub use` symbol set unchanged; H1 MCP tools still register; H2 consumers compile.
5. Workspace `cargo test` + `clippy -D warnings` green.
6. No intentional wire, schema, or behavior diffs.

## Out of scope follow-ups (explicit)

- Second binary / library consumer peels.
- Admin SPA structure.
- Further provider crate splits.
- Generic dual-pool attempt framework.

## Implementation next step

After this spec is reviewed and approved as written: invoke **writing-plans** to produce a task-by-task plan under `docs/superpowers/plans/2026-07-28-fat-file-module-restructure.md`, then execute wave-by-wave (prefer subagent-driven development after plan approval).
