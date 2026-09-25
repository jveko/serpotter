# Gap Closure (2026-09-25) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development with dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix every finding from the 2026-09-25 seven-agent gap audit (P1/P2/P3, deduplicated) across the serpotter workspace.

**Architecture:** Fixes stay inside existing crate boundaries (product purity, thin API shells, sqlx raw queries). All findings live verbatim in `docs/plans/2026-09-25-gap-audit.md` (7 sections, one per audit agent). Each task below cites its source finding titles from that archive — the archive IS the spec; the task text adds fix direction, files, tests, and verify commands.

**Tech Stack:** Rust workspace (axum 0.8 / sqlx / rmcp 3.x), Vite+ React SPA (`web/`), GitHub Actions, Docker.

**Execution ground rules (all tasks):**
- Worktree: `/Users/dimaz/workspace/projects/serpotter/.worktrees/gap-closure` (branch `gap-closure`). ALL edits stay here.
- NEVER commit; the controller commits after each batch passes both reviews.
- Add/adjust a regression test for every behavioral fix; run the narrow test first, then `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` before reporting (SPA tasks: `cd web && npm run typecheck && npm run check && npm test`).
- `Cargo.toml` / `Cargo.lock` / root workspace files: only the task that says so may touch them; others must run cargo WITHOUT `--locked` edits (do not regenerate the lockfile).
- Migration numbering is owned sequentially: T-lease creates `0019`, T-dbh creates `0020`. `EXPECTED_SCHEMA_VERSION` bumps with them (18 → 19 → 20), including `crates/serpotter-db/tests/migrate.rs` pins.
- Report format: status `DONE | DONE_WITH_CONCERNS | NEEDS_CONTEXT | BLOCKED`, exact files modified, test results.

---

## Batch overview (sequential batches; ≤3 independent tasks per batch, parallel inside)

| Batch | Tasks | Theme |
|---|---|---|
| B1 | T-lease | Lease integrity (holder-set schema 0019, hold-TTL guard) |
| B2 | T-trace, T-social, T-question | Log redaction, source alias fold, question-poll refresh |
| B3 | T-search, T-routing, T-poolsenv | Search result integrity, routing cleanup, pool env discipline |
| B4 | T-apienv, T-prov, T-adminsec | API env, provider responses, admin auth + CLI (owns `main.rs`) |
| B5 | T-leaseaux, T-research, T-credit | Blame/report/extract ladder, research loop, credit sync (controller pre-step: `retry_backoff_ms` → `pub(crate)`) |
| B6 | T-adminevents, T-adminapi, T-cache | Event coverage (+ product MetaSink), admin API contract, cache keys |
| B7 | T-mcp1, T-mcp2, T-mcp3 | MCP schema contract, params validation, runtime behavior |
| B8 | T-dbh, T-web1, T-web2 | DB hygiene (schema 0020), SPA auth/session, SPA surface |
| B9 | T-tests, T-ci, T-metrics | Gap tests, CI gates, metrics bracket |
| B10 | T-docs | Docs/comment drift sweep (incl. `docs/ops` schema-version refs) |
| FINAL | — | Whole-branch review + full verification |

Dependency notes: B1 is foundational (lease API shapes used by later product tasks). Within each batch no two tasks share a file. Cross-batch file reuse is sequential-only and intentional (e.g. `extract_url.rs` in T-question → T-leaseaux).

---

## Tasks

### T-lease (Batch 1) — Lease integrity: holder-blind release/accounting

**Source findings (archive §TestsCiOpsGaps):** "P1 — Holder-blind lease accounting: a late release mutates a *different* holder's row"; "P1 — Shipped defaults put the hold TTL *below* the request deadline, and the misconfig guard compares the wrong pair". Context (archive §DataLayerGaps #3): no on-disk concurrency coverage.

**Design SoT:** `docs/superpowers/specs/2026-09-11-overnight-audit-lease-integrity-design.md` — "Fix B" (lease_token), steps 2-5 documented as open. READ IT FIRST and follow it; if its schema sketch is incomplete, extend conservatively (per-acquire token column scoped on every release/report/refresh, multi-hold safe).

**Files:** `crates/serpotter-db/migrations/0019_lease_token.sql` (new), `crates/serpotter-db/src/lib.rs` (EXPECTED_SCHEMA_VERSION=19), `crates/serpotter-db/src/keys/acquire_report.rs`, `crates/serpotter-db/src/nodes.rs`, `crates/serpotter-db/tests/migrate.rs`, `crates/serpotter-keypool/src/lib.rs` (+ tests), `crates/serpotter-outbound/src/lib.rs` (+ tests), `crates/serpotter-product/src/hold.rs`, `crates/serpotter-product/src/lease.rs` (+ tests).

**Fix:**
1. **Holder-set lease representation** (review #1): a single mutable `lease_token` column CANNOT work — `KEY_MAX_INFLIGHT=3` allows concurrent holders on one `api_keys` row and node acquisition is uncapped, so a later acquire would overwrite the token and orphan earlier releases. Model each hold as its own child lease row (e.g. `api_key_leases` / `node_leases` keyed by a per-acquire token, or the design doc's equivalent holder-set): acquire inserts a holder row, `release_*`/`report_*`/`refresh_*` target `WHERE token = ?` (row-scoped, never id-scoped mutation of a shared counter), reclaim deletes expired holders and reconciles `inflight`, `lease_until` = max holder expiry. Preserve current observable semantics: cap enforcement, last-hold clearing, boot zeroing (also clears holder rows).
2. Token minted at acquire, carried through `KeyPool`/`ProxyPool` holds into product `KeyHold`/`ProxyHold`; `Drop` best-effort release passes it (a stale drop must affect 0 rows).
3. Boot guard: warn when `KEY_HOLD_TTL_SECS` < `REQUEST_TIMEOUT_SECS` (the missing misconfig check), next to `warn_if_hold_below_timeout`.
3b. **Refresh honesty (design-doc line 17):** `KeyPool::refresh_hold` / `ProxyPool::refresh` currently return `Ok(())` when `rows_affected() == 0` (silent lost lease). Treat 0 rows as a LOST LEASE: `warn!` with id/token and reflect it in the return value — never fail the product request.
4. Schema-version consumers (review #8): bump `EXPECTED_SCHEMA_VERSION` 18→19 AND update hardcoded pins `crates/serpotter-api/tests/admin_session.rs:23,118` and `crates/serpotter-api/tests/health.rs:143,169`. (`docs/ops/*.md` 18-references belong to T-docs — do not edit docs here.)

**Tests:** migrate.rs version pin 19; SQL-level regression: acquire → reclaim-expire → late release must NOT decrement the new holder's inflight / clear its `lease_until`; **two concurrent holds on ONE key row: first release keeps the second hold intact (inflight 2→1, not →0)**; `hold.rs` armed-drop vs disarmed-drop inflight tests (covers the TestsCiOps P3 "Drop safety net" gap); keypool refresh-scoped update test.

**Verify:** `cargo test -p serpotter-db -p serpotter-keypool -p serpotter-outbound -p serpotter-product` then workspace test + clippy.

### T-trace (Batch 2) — Stop logging live `adm-` session tokens

**Source finding (archive §TestsCiOpsGaps / §WebSpaGaps):** "P1 — Live `adm-` session tokens are written to durable logs" — `trace_layer.rs:114,120` logs `request.uri().path()` verbatim; `DELETE /api/admin/sessions/{token}` puts a valid 7-day credential in every span.

**Files:** `crates/serpotter-api/src/trace_layer.rs`, `crates/serpotter-api/tests/tracing.rs`.

**Fix:** `make_span` logs the axum route template (`MatchedPath` when available) instead of the raw path; for unmatched routes keep the raw path but redact the `admin/sessions/` segment (defence in depth). Response-side span fields unchanged otherwise; do not break existing request-id/mint tests.

**Tests:** integration test asserting a revoke call's recorded `path` field contains the template (or a redacted id), never the raw token; existing tracing tests still pass.

**Verify:** `cargo test -p serpotter-api tracing`.

### T-social (Batch 2) — Fold `social` → `x` before routing

**Source finding (archive §ProductCoreGaps):** "P1 — `sources: [\"web\",\"social\"]` silently loses the web leg (alias not folded before hybrid detection)" — `normalize_sources` returns the matched member verbatim; `hybrid` check at `routing/mod.rs:55` never sees the pair.

**Files:** `crates/serpotter-core/src/validation.rs` (fold inside `normalize_sources`), tests in `crates/serpotter-core/src/validation.rs` test module (do NOT edit `routing/mod.rs`).

**Fix:** `normalize_sources` maps `social` → `x` so one canonical member reaches every downstream literal comparison; dedupe after fold (e.g. `["web","x","social"]` → `["web","x"]`).

**Tests:** `[web, social]` folds to `[web, x]` (hybrid true downstream), single `social` still valid, duplicates collapse. Keep `routing/mod.rs:317 social_source_aliases_to_x` passing untouched.

**Verify:** `cargo test -p serpotter-core`.

### T-question (Batch 2) — Refresh holds during Firecrawl `format=question` poll

**Source finding (archive §PoolsProvidersGaps F1):** "P2 — Firecrawl `format=question` long-poll never refreshes key/node holds" — the wait loop lives in `FirecrawlClient::extract_question` (`crates/serpotter-providers/src/firecrawl.rs:414-445`, up to ~120s wall); the product awaits it at `extract_url.rs:888-907` and drops the handles (`_hold, _proxy_hold`).

**Files:** `crates/serpotter-product/src/extract/extract_url.rs`, `crates/serpotter-providers/src/firecrawl.rs` (expose start/status as public calls if not already), `crates/serpotter-providers/src/lib.rs` (re-exports if needed).

**Fix (review #2):** move the poll loop INTO the product layer, mirroring the structured-job pattern (`extract_url.rs:449-478`): product calls Firecrawl start-job once, then loops status checks calling `KeyRefresh::refresh` / `ProxyRefresh::refresh` each tick until deadline. `extract_question` shrinks to start+single status (or gains a callback seam) — pick the smallest change that puts the wait loop under product control.

**Additional scope (design-doc Fix A, folded in by plan review):** refresh BOTH holds immediately after job creation / before the first status call in the structured-extract loop (`extract_url.rs:432-473`) so the unrefreshed segment is bounded below HOLD_TTL — this is the named trigger of the whole defect chain. (The research-loop counterpart at `research.rs:988-1012` belongs to T-research; refresh honesty `rows_affected==0 → warn` belongs to T-lease — do not duplicate them here.)

**Tests:** question path refreshes at least once during a multi-tick poll (two-tick loop), mirroring how the structured-job refresh is tested; job still completes and deadline still aborts.

**Verify:** `cargo test -p serpotter-product extract -p serpotter-providers`.

### T-search (Batch 3) — Search result integrity: deep filters, dedupe, RRF ties

**Source findings (archive §ProductCoreGaps):** "P1 — Deep (Exa) search leg silently drops domain/date filters → unfiltered 200"; "P2 — The default (Single) search plan runs no dedupe at all"; "P2 — RRF tie ordering is nondeterministic (HashMap), so the `take(max_results)` cut is random".

**Files:** `crates/serpotter-providers/src/exa.rs`, `crates/serpotter-product/src/search/execute.rs`, `crates/serpotter-product/src/search/mod.rs`, `crates/serpotter-core/src/pipeline.rs` (+ tests).

**Fix:**
1. `search_deep` gains include/exclude domains + date filters (Exa supports `includeDomains`/`excludeDomains`/`startPublishedDate`/`endPublishedDate`); `execute_deep_search` forwards the computed values from `search/mod.rs:160-176`. Filters Exa cannot honor (`time_range` beyond dates, `country`, `include_content`) → fail loudly with `ProviderError::Unsupported` when set (never silent).
2. Single-plan results run `normalize_url` + `dedupe_near_duplicates` (reuse the pipeline helpers; cheap path — no RRF needed).
3. RRF: carry a first-seen ordinal next to each entry; sort by `(score desc, ordinal asc)` so ties resolve deterministically (fixes cache/citation instability).
4. **(review #5)** `search_inner` empty-query maps to `SearchExecError::InvalidRequest` (not `Search("missing_query")`) at `search/mod.rs:127-131`, so the boundary answers 400 `ValidationError` instead of a retryable 502. The `InvalidRequest` variant and boundary mapping already exist — no `product/errors.rs` edit.

**Tests:** deep-path request with domain filter asserts the Exa body includes `includeDomains` (or Unsupported on refusal); single-plan duplicate URLs collapse; equal-score RRF output stable across repeated runs with shuffled insertion order; empty-query through `search_inner` returns `InvalidRequest`.

**Verify:** `cargo test -p serpotter-core -p serpotter-providers -p serpotter-product`.

### T-routing (Batch 3) — Remove unreachable routing rules/gates; make `mode` honest

**Source finding (archive §ProductCoreGaps):** "P2 — Route table carries three unreachable rules/gates, and `mode: \"web\"` is a no-op dial" (rules.rs priority-50 research rule, priority-10 default-web rule, Gate 6 reachable only by rejected intents, `resolve.rs` dead `return false`/`other` arm, `mode:"web"` does nothing).

**Files:** `crates/serpotter-core/src/routing/rules.rs`, `routing/mod.rs`, `routing/resolve.rs` (+ tests in-module).

**Fix (review #15):** delete the dead rule arms, Gate 6, and the dead `resolve.rs` arms (per finding detail). **`mode:"web"` gets a behavioral contract, not a comment:** `VALID_MODES` still advertises it, so treat `mode:"web"` as "web-only search" — fold it in canonicalization/resolve so sources are forced to web-primary (exclude `x`/social legs; equivalent to `sources:["web"]` semantics), making Gate 3's handle hijack impossible just like `sources:["web"]`. (`validation.rs` may be edited here — T-social in B2 precedes this batch.) Every remaining rule must be reachable with boundary-legal input; update `routing/AGENTS.md` rule-count notes if present.

**Tests:** replace the `intent:"banana"` Gate-6 test with tests that pin each REMAINING rule via `route_search` with legal inputs; keep all existing routing tests green (adjust only those that pinned dead behavior, documenting why).

**Verify:** `cargo test -p serpotter-core`.

### T-poolsenv (Batch 3) — Pool tunable warn discipline + node userinfo encoding

**Source findings (archive §PoolsProvidersGaps):** "P3 — `KEY_*` clamp policy is inconsistent with the warned-misconfiguration discipline" (`KEY_MAX_INFLIGHT=0`, `KEY_ACQUIRE_TIMEOUT_SECS=0`, `KEY_UNKNOWN_CREDIT_WEIGHT=-5` silent); "P2 — Node proxy userinfo percent-encoding is incomplete" (only `%`, space, `@`, `:` encoded; `/ # ? & = [ ]` corrupt the authority).

**Files:** `crates/serpotter-keypool/src/lib.rs` (+ tests), `crates/serpotter-outbound/src/lib.rs` (+ tests), `docs/ops/env.md` (knob ranges — residual doc lines for these knobs only).

**Fix:** warn + safe default for all four `KEY_*` tunables on out-of-range values (mirror `KEY_HOLD_TTL_SECS`'s loud warn); same for `NODE_HOLD_TTL_SECS` if silently clamped. Percent-encode the full userinfo non-allowed set (RFC 3986 userinfo: encode everything except unreserved + `!$&'()*+,;=`; conservatively encode `!$&'()*+,;=` too if simpler — the invariant is: any byte round-trips through `Url::parse`).

**Tests:** env parse tests for 0/negative/negative-weight values (warn + default, following `keypool/tests.rs` existing matrix); userinfo test with password containing `/ # ? & = [ ]` asserting `Url::parse(proxy_url).username()/password()` round-trip.

**Verify:** `cargo test -p serpotter-keypool -p serpotter-outbound`.

### T-apienv (Batch 4) — API env parsing discipline

**Source findings (archive §TestsCiOpsGaps):** "`CACHE_TTL_SECS` falls back silently" (breaks warn-on-invalid house rule), "`seed-token` swallows unknown/missing flag values" + "the `serpotter.latest` symlink claim" (both MOVED to T-adminsec, which owns `main.rs` this batch); (archive §ApiAdminGaps): "per-request re-warn" (`product_ctx()` re-reads `REQUEST_TIMEOUT_SECS` per request → WARN flood).

**Files:** `crates/serpotter-api/src/lib.rs` (CACHE_TTL parse-once at startup with warn), `crates/serpotter-api/src/product/mod.rs` (request-timeout parsed once / warn once — hoist into `AppState`), `docs/ops/env.md` (CACHE_TTL fallback + ranges; remove `serpotter.latest` claim) (+ tests). **Do NOT edit `main.rs` (T-adminsec owns it in this batch).**

**Fix:** single startup parse with `warn!` on invalid/negative `CACHE_TTL_SECS` and `REQUEST_TIMEOUT_SECS`, stored in state.

**Tests:** port warning tests in the style of `main.rs:372-380` (`port_from_env`) for both knobs (unit-test the parse fns if the warn happens in a constructor).

**Verify:** `cargo test -p serpotter-api`.

### T-prov (Batch 4) — Provider response honesty + dead surface deletion

**Source findings (archive §PoolsProvidersGaps):** "P2 — Empty extract bodies succeed (and are cached)" (firecrawl `unwrap_or_default`, tavily row-exists, exa `is_none` only — single-extract paths); "P3 — xAI refuses `include_content` but silently drops `include_raw_content`"; "P3 — Dead exported provider surface" (`ExaClient::answer`+DTOs, `XaiClient::complete`, `TavilyClient::with_default` — zero callers); (archive §DataLayerGaps): providers crate doc drift `AGENTS.md` extract row.

**Files:** `crates/serpotter-providers/src/{firecrawl,tavily,exa,xai}.rs`, `crates/serpotter-providers/src/lib.rs` (re-exports/docs), `crates/serpotter-providers/AGENTS.md`.

**Fix:** single-extract paths return `Unextractable` when content trims empty (all three providers); `validate_xai_search_policy` also refuses `include_raw_content` (same wire capability); delete `answer`/`complete`/`with_default` + their DTOs and orphaned tests; fix the AGENTS extract row (Firecrawl/Tavily/Exa).

**Tests:** empty-content regression per provider (`Unextractable`, not `Ok("")`); xai policy test for `include_raw_content`; delete tests for removed API.

**Verify:** `cargo test -p serpotter-providers`.

### T-adminsec (Batch 4) — Admin auth hardening

**Source findings (archive §TestsCiOpsGaps):** "`ADMIN_SECRET` compared with `==` (non-constant-time) and no throttle on credential routes"; (archive §ApiAdminGaps): "`POST /api/admin/login`: unauthenticated argon2 DoS + username enumeration", "`logout` swallows the DB failure and always answers 204", "Bootstrap has no password strength floor", "non-constant-time secret compare + case-sensitive `Bearer `".

**Files:** `crates/serpotter-api/src/admin/mod.rs`, `crates/serpotter-api/src/admin/session.rs`, `crates/serpotter-api/src/main.rs` (**owned by this task in B4**: connect-info wiring + seed-token strictness + symlink-comment removal), `crates/serpotter-api/tests/` (new/extended admin auth tests), `docs/ops/deploy.md` (throttle/proxy caveat section ONLY).

**Fix:**
1. Constant-time compare for `ADMIN_SECRET` (workspace has no `subtle` dep — implement a small constant-time byte-compare helper, no new dependency) at both compare sites.
2. RFC-7235 case-insensitive `Bearer` parse shared with the product path style.
3. Login **and bootstrap** (review #13): always run argon2 against a dummy hash for unknown usernames (kills the timing oracle); add an in-memory per-IP failed-attempt window (fixed-capacity map + expiry, no new dependency) limiting e.g. 10 failures/5min → 429, applied to both routes.
4. **Throttle identity (review #13):** production must serve with `into_make_service_with_connect_info::<SocketAddr>` (`main.rs:141` region) so handlers extract `ConnectInfo<SocketAddr>` directly (no XFF trust — document the reverse-proxy caveat in `deploy.md`). Tests without a socket: provide a test-only reset/backdoor (e.g. `#[cfg(test)]` store reset + a unit test of the window fn) so suites stay deterministic; missing ConnectInfo → key the throttle on a shared "unknown" bucket.
5. Bootstrap: enforce the same `>= 8` password floor as `change_password`.
6. Logout: return 500 problem+json when `delete_admin_session` errors; keep 204 for unknown/expired tokens.
7. **(moved from T-apienv, review #9)** `parse_name_flag` bails on unknown/missing `--name` exactly like `parse_seed_key` (`main.rs:279-285`); delete the `serpotter.latest` claim from the log-init comment (`main.rs:199-203`).

**Tests:** dummy-hash 401 shape parity, throttle trips at limit then recovers after window (unit on the store), bootstrap short password → 400, bootstrap throttling shares the login window, logout DB failure mapping, `bearer` case-insensitivity, `parse_name_flag` unknown/missing flag → error.

**Verify:** `cargo test -p serpotter-api`.

### T-leaseaux (Batch 5) — Blame/report consistency + extract retry ladder

**Source findings (archive §PoolsProvidersGaps F3):** "P2 — A node whose proxy URL won't build is never blamed and becomes a least-inflight magnet"; (archive §ProductCoreGaps): "P2 — `ReportMode` mapping is triplicated… two copies carry a doc claiming a difference that doesn't exist"; "P2 — Extract and research-social retries have no backoff" (extract half); "P2 — `vendor_suspended` is described as self-healing … on a loose substring match"; "P3 — Error-class drift… `resp_json` `unwrap_or_default()` writes an empty-string cache row".

**Files:** `crates/serpotter-product/src/lease.rs`, `crates/serpotter-product/src/search/run_provider.rs`, `crates/serpotter-product/src/extract/extract_url.rs`, `crates/serpotter-product/src/report.rs`, `crates/serpotter-product/src/search/banned.rs`, `crates/serpotter-product/src/hold.rs` (comment only), `crates/serpotter-product/src/lib.rs` (re-export cleanup if `classify_proxied_http` deleted), `crates/serpotter-product/AGENTS.md` (dead-API row).

**Fix:**
1. `client_for` error path calls `finish_failure` on the proxy hold (URL-build errors prove node config is bad → `consecutive_fails` moves → fail@3 reachable).
2. Delete both duplicate `report_mode` copies (`run_provider.rs`, `extract_url.rs`); call `lease.rs` `verdict_for`; drop the stale "EXCEPT transport" comments and duplicate pinned tests (keep one).
3. Extract retry ladder: sleep `retry_backoff_ms(attempt)` before retry-class `continue`. **B5 pre-step (controller, before dispatch): `retry_backoff_ms` becomes `pub(crate)` in its current location** so T-research can share it without a same-batch dependency — do not move it.
4. **(review #4)** Normalize extract `provider`: `fold_extract_provider` maps `"auto"`/blank → `None` at the shared entry so ALL four public paths agree (single 200 path, question/highlights 400, batch auto→Tavily dispatch, `extract_dispatch` filter); fix the four stale `chain_for` comments (`extract_url.rs:16,583,1505,1511`).
5. `banned.rs`: gate the likely-tier on a phrase (e.g. `account .* suspended`) not a bare word; fix the "self-healing" comments in `banned.rs` and `hold.rs` to "operator re-enable only" (schema 18 made `vendor_suspended` permanent).
6. `resp_json` guard: skip the cache put on serialize failure (match the other five sites).
7. Delete dead `classify_proxied_http` (logic stays inlined in `lease.rs`) **and** dead `merge_providers_consulted_real` + its tests (`extract/helpers.rs`) + both `lib.rs` re-exports (review #7); remove the `serpotter-product/AGENTS.md` rows here (T-docs will not re-touch them).

**Tests:** client-build-failure moves `consecutive_fails`; one verdict-matrix test (was triplicated); extract backoff invoked (inject a sleeper seam narrowly if no pattern exists); `provider:"auto"` behaves identically across all four entry points; banned matcher: bare word in a 403 page does NOT ban, phrase DOES; no test references the two deleted functions.

**Verify:** `cargo test -p serpotter-product`.

### T-research (Batch 5) — Deep/standard research knob parity + refinement dedupe

**Source findings (archive §ProductCoreGaps):** "P2 — Deep research ignores/contradicts the knobs the standard loop honors" (`scrapeTopN:0`→1, social/handle legs dropped silently); "P2 — Deep-research refinement dedupes on raw URLs, contradicting the `normalize_url` contract"; "P3 — Error-class drift… synthetic `Upstream { status: 0 }` rendered as '(status 0)'"; (archive §McpGaps P2-6 is the MCP-boundary half — NOT this task).

**Files:** `crates/serpotter-product/src/extract/research.rs` ONLY (+ tests).

**Fix:** **(design-doc Fix A, research half)** refresh BOTH holds immediately after research job creation / before the first status call in the tavily research loop (`research.rs:988-1012`) so the unrefreshed segment stays below HOLD_TTL (T-question owns the extract-side counterpart); `scrape_top_n == 0` skips scraping in the deep path (mirror standard `research.rs:122-123`); when deep drops social/handle input, surface a note in `evidence.web_leg_errors` (or an equivalent visible warning field already on the DTO — check `ResearchResponse` shape, do not invent new wire fields without noting it in the report); refinement dedupe compares `normalize_url(...)` on both sides; synthetic poll failures map to a real error class (transport → `Http`-class 502) instead of `Upstream { status: 0 }`; **(review #6) the xAI social leg's 3-attempt loop (`research.rs:335-347`) sleeps `retry_backoff_ms(attempt)` before each retry-class continue — the helper is already `pub(crate)` via the B5 controller pre-step; research gets its own backoff coverage, not the extract copy's.**

**Tests:** deep+`scrapeTopN:0` performs zero scrapes; pass-2 with `www.`-variant URL is not re-scraped; synthetic transport error renders without "status 0"; social leg does not fire 3 immediate attempts (sleeper seam or elapsed-ordering assertion).

**Verify:** `cargo test -p serpotter-product research`.

### T-credit (Batch 5) — Credit sync honesty + throttle + re-enable floor + alert cadence

**Source findings (archive §PoolsProvidersGaps F5):** "P2 — Credit-sync fabricates `credits_remaining=0` from any 200 with unrecognized JSON, and also resets `consecutive_fails`"; (archive §ApiAdminGaps): "P2 — Credit sync has no throttle or batch cap (Tavily 10 calls/10 min)", "P3 — Error-window alerting is sampled at 15 min for a 5 min window"; (archive §DataLayerGaps #8): "`KEY_REENABLE_AFTER_HOURS=0` disables fail@3 backoff … no floor validation" and negative values become a silent no-op.

**Files:** `crates/serpotter-providers/src/usage.rs` (+ tests), `crates/serpotter-api/src/credit_sync.rs`, `crates/serpotter-api/src/cron.rs`, `crates/serpotter-db/src/keys/admin_crud.rs` (+ tests), `docs/ops/env.md` (re-enable range, alert cadence, credit throttle).

**Fix:**
1. `parse` of vendor usage JSON: when no recognized limit/remaining field is present → `Err` (skip write entirely); never `unwrap_or(0.0)` into a snapshot. Update the tests at `usage.rs:150-174` that pin the old fabrication.
2. `update_api_key_usage` must not reset `consecutive_fails` (credential-usage update ≠ failure history).
3. Throttle: cap Tavily keys per sync pass (≤10/10min window — implement as chunking with skipped-key reporting) and bound the batch; cron respects the same cap.
4. Alert check: run `alert_if_high_error_rate` on its own ≤60s cadence (separate tokio interval), independent of the 15-min maintenance tick.
5. Re-enable floor: reject/clamp `KEY_REENABLE_AFTER_HOURS < 1` with a loud warn at startup (treat `0`/negative as "cron disabled" is NOT chosen — clamp to ≥1 and warn; document in env.md).

**Tests:** unrecognized-JSON → no DB write (and old fabrication tests now expect Err); consecutive_fails survives a sync; re-enable floor warn+clamp test; alert interval unit (cadence value extracted as const with a test).

**Verify:** `cargo test -p serpotter-providers -p serpotter-api -p serpotter-db` (env: unset `CREDIT_SYNC_CRON`).

### T-adminevents (Batch 6) — Request-event coverage holes

**Source findings (archive §ApiAdminGaps):** "P2 — Boundary-rejected requests emit no event"; "P2 — Every F10 504 is attributed to service `unknown` with zero attempts"; "P3 — `kind` drift: wire says `RequestTimeout`, events/ring say `Timeout`"; (archive §McpGaps P2-5): "MCP auth failures emit no request event; REST's F08 does"; (archive §ApiAdminGaps): "Raw SQLite error text is echoed … marked `retryable: true`".

**Files:** `crates/serpotter-api/src/events.rs`, `crates/serpotter-api/src/product/{mod,search,extract}.rs`, `crates/serpotter-api/src/mcp/auth.rs`, `crates/serpotter-api/src/product/errors.rs`, **`crates/serpotter-product/src/meta.rs` + writer sites (`search/…`, `extract/research.rs`, `lease.rs` — B5 tasks precede this batch, so edits stack safely)**, tests under `crates/serpotter-api/tests/`.

**Fix:**
1. Emit an event for extractor-level rejections on the product route stack (thin wrapper/`FromRequest` wrapper or response-mapping middleware for `AppJson` rejections + 405/413 where reachable) — count toward ring + error window + metrics.
2. **(review #12) Product-side execution-metadata sink:** `ExecMeta` is created/mutated INSIDE product futures and only returned on completion, and `run_with_deadline` drops that future before the `Elapsed` arm — an API-side `Mutex<ExecMeta>` can never observe it. Add a product-context metadata sink (e.g. an `Arc<MetaSink>` with `observe(&ExecMeta)` writers at attempt/lease/provider call sites in `meta.rs`/`lease.rs`/research), threaded from the API deadline wrapper into `search_inner`/`extract_url`/`research_inner` via their parameter structs (Option field; no new deps, no `dyn` traits). `Elapsed` then reads `sink.last()` → real `service`/`attemptCount`/`keyId`/`nodeId` instead of `ExecMeta::default()`.
3. Emit `error_kind = "RequestTimeout"` on REST 504 arms (match wire `kind`).
4. `mcp_auth_middleware` emits the shared auth-failure event (same field constructor as `ApiTokenLogged`).
5. `DatabaseError` problem detail: fixed generic message (log real error server-side), and exclude `DatabaseError` from `kind_retryable`.

**Tests:** malformed-body rejection produces a ring/error-window entry; timeout event carries non-unknown service (integration if feasible, else unit on the sink + Elapsed-field builder — say which); MCP 401 produces an event; `DatabaseError` problem has generic detail + `retryable` absent/false; sink unit: writers → `last()` returns the last attempted vendor.

**Verify:** `cargo test -p serpotter-api`.

### T-adminapi (Batch 6) — Admin API error-shape + usage/log contracts

**Source findings (archive §ApiAdminGaps):** "P2 — Every admin body/query/path rejection answers axum plain-text, not `application/problem+json`"; "P1 — `GET /api/usage` 180-day window silently truncated to 90 by the DB layer" (also §DataLayerGaps #2); "P3 — Collected-but-unexposed request data" (`LogOut` omits `cache_hit`/tokens/`cost_est`, no `errorKind` filter); "P3 — Unbounded admin inputs / unbounded analytics queries" (spend endpoints full-table, no window; token/node/key string fields uncapped).

**Files:** `crates/serpotter-api/src/admin/{keys,nodes,tokens,settings,session,logs,usage,stats}.rs`, `crates/serpotter-db/src/usage.rs` (+ tests), `crates/serpotter-api/tests/`.

**Fix:**
1. All admin handlers use the `AppJson` wrapper (product pattern, `product/mod.rs:17-22`); `Query`/`Path` rejections map to `application/problem+json` (a shared rejection wrapper for extractors).
2. Single `days` clamp bound: export `USAGE_MAX_DAYS` from `serpotter-db`, used by both API (`1..=180` — keep 180, DB must not re-clamp to 90) with `docs/ops/api.md` updated in-task.
3. `LogOut` gains `cacheHit`, `inputTokens`, `outputTokens`, `totalTokens`, `costEst`; ring filter gains `errorKind` (query param `errorKind`, documented).
4. Spend endpoints: required/derived `days` window + `LIMIT` (default 90d, clamp like usage); admin string inputs bounded (name/key ≤ reasonable lengths e.g. 256, `host` syntax validated at create → 400 instead of per-request dial failure).

**Tests:** admin malformed body/query/path → problem+json content-type; `?days=120` returns rows older than 90 (seeded); LogOut fields round-trip; `errorKind` filter works; oversize name/host → 400; spend with window returns only window rows.

**Verify:** `cargo test -p serpotter-api -p serpotter-db`.

### T-cache (Batch 6) — Cache key integrity

**Source findings (archive §DataLayerGaps #1):** "P2 — Query-cache key is FNV-1a 64-bit (forgeable) and is NOT service-scoped, contradicting the DDL contract" (migration `0015` claims a "service-aware content hash"; canonical strings contain no service prefix; `ON CONFLICT(key_hash) DO UPDATE SET service = excluded.service` lets a cross-surface collision evict another surface's row); (archive §ProductCoreGaps): unquoted canonicalization lets distinct requests collide (`cache.rs:144-146` comma-joined `urls`, `:81-83` unquoted `query`, `:85` `sources` join) — a colliding key serves another caller's JSON with `cache_hit:true`. *(Added by plan review findings #3 — previously unowned.)*

**Files:** `crates/serpotter-product/src/cache.rs` (+ tests). **No new migration** — SHA-256 over a framed `service || canonical` input makes forgery/collision computationally infeasible without storing anything.

**Fix:**
1. Hash: SHA-256 (`sha2`, already a workspace dep used by `serpotter-db`'s key hashing) over `service || '\x00' || canonical`; never FNV.
2. Canonicalization: every field/list serialized unambiguously (length-prefixed or `{:?}` debug-quoting per element) so `urls=["a,b"]` and `urls=["a","b"]` can never alias; service prefix inside the hashed input (satisfies the 0015 DDL's "service-aware content hash" contract).
3. `ON CONFLICT(key_hash) DO UPDATE SET service = excluded.service` must not silently rewrite another surface's row — with service in the key, per-service rows are genuinely separate; verify the upsert no longer crosses surfaces (drop the `service` overwrite if it becomes unreachable).

**Tests:** cross-surface (search vs extract) keys never collide even for identical query text; comma-in-URL vs comma-split lists produce different keys; forged/extended input cannot reuse another row's key (hash mismatch path); existing cache tests green.

**Verify:** `cargo test -p serpotter-product -p serpotter-db`.

### T-metrics (Batch 9) — Metrics bracket honesty + dead metric/extractor surface

**Source findings (archive §TestsCiOpsGaps):** "Metrics in-flight bracket: wiring contradicts its own docs and silently excludes the SPA"; (archive §ApiAdminGaps): "`metrics::observe` accepts `_input_tokens`/`_output_tokens` and discards them… doc claims callers pass `false` for `cache_hit`"; dead `crate::ApiToken` extractor. *(Batch 9, not 6: `events.rs`/`lib.rs` are shared with B6 tasks — review #9.)*

**Files:** `crates/serpotter-api/src/metrics.rs`, `crates/serpotter-api/src/lib.rs` (layer position + comment + `ApiToken` deletion), `crates/serpotter-api/src/events.rs` (call-site arg removal — B6's edits to this file land first).

**Fix:** either move `metrics_middleware` to truly outermost (last `.layer` after SPA fallback — verify axum layer-vs-`fallback_service` ordering empirically with a test asserting SPA/asset requests increment the counters) or correct BOTH comments to describe reality; **(review #11) the "pass real values" option is already true at `events.rs:557-564` — the defect is `metrics.rs:150-170` discarding them. Chosen: DELETE the unused `_input_tokens`/`_output_tokens` args and fix the stale doc comment (tokens/cost live in `usage_daily` — no new collector, YAGNI); keep `cache_hit` wired as events passes it.** Delete dead `ApiToken` (`lib.rs:264-276`).

**Tests:** metrics observed for a fallback (SPA) request if layer moves; `observe` signature change compiles at all call sites; `/metrics` auth test belongs to T-tests (do NOT add here).

**Verify:** `cargo test -p serpotter-api metrics`.

### T-mcp1 (Batch 7) — MCP error results conform to `outputSchema`

**Source finding (archive §McpGaps P1-1):** "Error results put a non-conforming object in `structuredContent` while `outputSchema` is advertised" — spec (2026-07-28) requires structured results conform; spec's Tool Execution Error example carries `content` only. Tests currently PIN the violation (`tests/mcp_stateless.rs:486,574,609,658`, `mcp/progress.rs:146-152`).

**Files:** `crates/serpotter-api/src/mcp/errors.rs`, `crates/serpotter-api/src/mcp/progress.rs` (ONLY the envelope pin in its test module at :146-152 — this task owns `progress.rs` in B7, not T-mcp3), `crates/serpotter-api/tests/mcp_stateless.rs` (update pins), `docs/ops/api.md:155` region.

**Fix (conservative pick):** error envelopes go into `content` (the JSON text block stays byte-identical) with NO `structured_content` when `is_error`; success paths unchanged. `outputSchema` stays the response type. Update ALL FIVE pinned assertions (review #10): `tests/mcp_stateless.rs:486,574,609,658` **and** `mcp_stateless_search_structured_content` at `tests/mcp_stateless.rs:430-453`, plus `mcp/progress.rs:146-152` — all read the envelope from `content[0].text`.

**Tests:** every updated pin asserts the same envelope values from `content`; add an assertion that an error result has `structured_content == None`; success result still carries conforming `structuredContent`.

**Verify:** `cargo test -p serpotter-api mcp`.

### T-mcp2 (Batch 7) — MCP input-schema/param validation parity

**Source findings (archive §McpGaps):** P1-2 "`extract_url` batch mode unreachable: `url` required"; P2-6 "`research` advertises knobs the deep loop silently discards"; P3-12 "Extract `format` closed set duplicated as literals / only case-sensitive knob"; P3-13 "`time_range` closed set for search, passthrough for research"; P3-14 "stale tool/comment contracts and one no-op serde alias".

**Files:** `crates/serpotter-api/src/mcp/params.rs` (+ unit tests), `crates/serpotter-core/src/validation.rs` (`VALID_EXTRACT_FORMATS` + `normalize` helper — T-social landed first, extend, don't restructure), `crates/serpotter-product/src/extract/extract_url.rs` (single format-literal site now reads the core constant), tests `crates/serpotter-api/tests/mcp_tools.rs`.

**Fix:**
1. `ExtractParams.url: Option<String>`; validation reuses REST's `has_batch` gate (`product/extract.rs:28-30`) — batch extract works without `url`; single mode still requires it.
2. `validate_research_params` rejects incompatible combos: `deep` + (`research_backend` | `citation_format` | `social_max_results`), `deep` + `scrape_top_n == 0` is NOT rejected (product now honors 0 — align: reject only knobs product ignores); `time_range` runs through `normalize_time_range` (match search).
3. `VALID_EXTRACT_FORMATS` in core; both boundary and product route gate via `normalize_choice` (case/whitespace tolerant).
4. Fix stale descriptions ("only firecrawl/tavily" → actual provider set, tool description naming all dispatch modes); delete the self-referential `urls` alias.

**Tests:** batch extract MCP call without `url` succeeds (or reaches validation beyond url); `format:"MARKDOWN"` accepted; research `timeRange:"W"` + typo'd value behave like search; deep+`researchBackend` → 400 with clear kind; no `urls→urls` alias (compile-level: assert unknown alias field still parses — or simply assert deserialization of a camelCase misspell fails as before).

**Verify:** `cargo test -p serpotter-core -p serpotter-api mcp`.

### T-mcp3 (Batch 7) — MCP runtime: deadline race, progress flush, health, CORS, session binding, rate limit

**Source findings (archive §McpGaps):** P2-3 "`tokio::select!` races deadline vs completed future"; P2-4 "progress frames never flushed on cancel/timeout exits"; P2-7 "No CORS/preflight path for `/mcp`"; P2-8 "Legacy session ids bound to no identity"; P2-9 "No rate limiting on `/mcp`"; P3-10 "`MCP_SESSION_HEADER` dead public API"; P3-11 "`health` tool escapes error-envelope and request-log contract".

**Files:** `crates/serpotter-api/src/mcp/{mod,auth}.rs` (**NOT `progress.rs` — T-mcp1 owns it in this batch**; the `flush` helper already exists and is idempotent, T-mcp3 only calls it from `mod.rs`), `Cargo.toml` (add `cors` feature to `tower-http`; ONLY this task touches workspace Cargo files), tests `crates/serpotter-api/tests/mcp_session.rs`.

**Fix:**
1. `tokio::select! { biased; ... }` (product future polled first) — matches REST `timeout` semantics.
2. `sink.flush().await` before every early `return` in `run_tool` (Cancelled + Timeout paths).
3. CORS: skip auth for `OPTIONS`; `CorsLayer` driven by `MCP_ALLOWED_ORIGINS` (allowlist; empty → no CORS headers). Add `tower-http` `cors` feature to workspace `Cargo.toml` and let `Cargo.lock` update ONCE here.
4. Session↔token binding: map `session_id → token id` at `initialize`; subsequent GET/DELETE/POST with mismatched token → 404. Persisted in-process alongside `LocalSessionManager`.
5. Per-token in-flight cap for tools/call (bounded semaphore keyed on token id, e.g. `KEY_MAX_INFLIGHT`-style default 8) → retryable `KeyBusy`-like envelope; reject without spawning a progress delivery task.
6. `health`: DB error → `tool_error_structured("DatabaseError", …)` with `isError`; emit a request event like other tools. Delete `MCP_SESSION_HEADER` (or use it in tests — pick: use it in `mcp_session.rs` so it's load-bearing).

**Tests:** completed-future-wins-the-tick regression (deterministic: 0-timeout select with ready future); flush-before-return observable via sink spy; preflight OPTIONS 2xx without auth; foreign-token session DELETE → 404; cap exceeded → retryable envelope; `health` with broken DB → error envelope + event row.

**Verify:** `cargo test -p serpotter-api` (lockfile changed — do NOT hand-edit `Cargo.lock`; run `cargo test` once to update it).

### T-dbh (Batch 8) — DB hygiene: schema 0020, pragmas, indexes, WAL test

**Source findings (archive §DataLayerGaps):** #4 dead `api_keys.email`; #5 dead `Db::bump_node_inflight`; #6 declared FK never enforced (no `foreign_keys=ON`); #7 reclaim/cron scans have no supporting index; #12 `busy_timeout` never configured; #3 (test half) "no on-disk WAL multi-connection test anywhere".

**Files:** `crates/serpotter-db/migrations/0020_hygiene.sql` (new: `DROP COLUMN email` — SQLite ≥3.35 supports it, else table rebuild; partial indexes on `lease_until` for keys+nodes and `(active, last_used_at)`), `crates/serpotter-db/src/lib.rs` (EXPECTED=20, `foreign_keys(true)`, explicit `busy_timeout(5s)`), `crates/serpotter-db/src/nodes.rs` (delete `bump_node_inflight`), `crates/serpotter-db/tests/migrate.rs` (pin 20; new tempdir on-disk test).

**Fix:** as above; keep FK enable compatible with existing data (no orphaned `admin_sessions.user_id` — verify with a pre-check query, document result in the test). **Schema-version consumers (review #8):** bump 19→20 AND update hardcoded pins `crates/serpotter-api/tests/admin_session.rs:23,118` and `crates/serpotter-api/tests/health.rs:143,169` (they now read 19 after B1); `docs/ops/*.md` hardcoded versions belong to T-docs.

**Tests:** migration applies cleanly to 20 with existing rows (the migrate.rs suite rebuilds from scratch — add a data-path check for FK enforcement: insert orphan session → error); new `#[tokio::test]` spawning N concurrent `acquire_api_key_shared` on a tempdir DB with `max_connections=5` asserting cap never exceeded; `bump_node_inflight` callers — confirm zero before deleting.

**Verify:** `cargo test -p serpotter-db`.

### T-web1 (Batch 8) — SPA auth/session correctness

**Source findings (archive §WebSpaGaps):** P1 "Wrong current password logs the admin out (401-as-domain-error vs global teardown)"; P2 "`relativeTime()` parses server UTC stamps as local time"; P3 "Cross-tab logout … never tears down the other tab's live view"; P3 "Topbar Refresh on /settings never refreshes the sessions list".

**Files:** `web/src/lib/query-client.ts`, `web/src/features/settings/{SettingsPanel.tsx,queries.ts}` (only if needed), `web/src/lib/relative-time.ts`, `web/src/features/auth/auth-context.tsx`, `web/src/features/shell/Topbar.tsx`, tests `web/src/**/*.test.ts` as applicable.

**Fix:**
1. `meta.authTeardown: false` (or equivalent) on the change-password mutation; MutationCache 401 handler respects it → inline `changeErr` renders instead of logout.
1b. Revoke button: `disabled` for the current session row (`s.current`) or routed through `ConfirmDeleteDialog` — pick disabled+title tooltip (smallest change; matches "no silent self-revocation" without adding a dialog). *(This SettingsPanel edit is owned here, not by T-web2.)*
2. `relativeTime` normalizes zone-less UTC (`T` + `Z`, reuse `parseSessionExpiry` logic — factor shared helper into `lib/`), `<time dateTime>` gets ISO.
3. Storage-event handler: **(review #14)** `onAuthStorageChanged` fires for login/secret-switch/logout — gate teardown on `!next.isAuthenticated` ONLY, then invoke `endAdminSession(qc)` (unconditional teardown would let a login in another tab wipe the new credentials everywhere).
4. Settings section key returns `[qk.settings.all, qk.admin.sessions()]`.

**Tests:** vitest — change-password 401 does not trigger teardown; relativeTime on `"2026-09-25 12:00:00"` parses as UTC; cross-tab storage event clears query cache; refresh key includes sessions.

**Verify:** `cd web && npm run typecheck && npm run check && npm test`.

### T-web2 (Batch 8) — SPA surface: dashboard status, disabledReason, dead code, mutation hygiene

**Source findings (archive §WebSpaGaps):** P2 "Dashboard has no loading/error surface and never publishes panel status"; P2 "`disabledReason` typed nowhere and shown nowhere"; P3 "Sticky mutation errors without a reset path" (KeysPanel/TokensPanel); P3 "Dead code/config" (`qk.dashboard.all`, `adminFetch` bearer option, `clampOffset`, dev-proxy `/live`). *(Self-revoke guard moved to T-web1, which owns `SettingsPanel.tsx`.)*

**Files:** `web/src/routes/_auth/dashboard.tsx`, `web/src/features/keys/{types.ts,KeysPanel.tsx}`, `web/src/features/tokens/TokensPanel.tsx`, `web/src/lib/{query-keys.ts,api.ts}`, `web/src/features/logs/queries.ts`, `web/vite.config.ts`, tests.

**Fix:** dashboard publishes panel status + per-source `role="alert"` error blocks (mirror StatsPanel pattern); `KeyRow.disabledReason` typed + warn chip for `vendor_suspended` vs `manual`; `mutation.reset()` discipline for edit/toggle/delete banners; dead code deletions listed above (delete `clampOffset` + its test).

**Tests:** dashboard error state test; disabledReason chip rendering (by variant); no test references deleted symbols.

**Verify:** `cd web && npm run typecheck && npm run check && npm test`.

### T-tests (Batch 9) — Untested-contract test suite

**Source findings (archive §TestsCiOpsGaps):** "/metrics auth gate has no test at any layer"; "Expired admin-session rejection never goes through the HTTP layer"; "Extract/research 504 `RequestTimeout` branches untested; only the search copy is"; (archive §DataLayerGaps #3 test half — WAL test already lands in T-dbh, do NOT duplicate it here).

**Files:** `crates/serpotter-api/tests/` (extend existing suites: `health.rs` or a new `metrics_auth.rs`, `admin_session.rs`, `extract_research.rs`), possibly `crates/serpotter-api/src/` ONLY if a test seam is genuinely required (report it).

**Fix:** add the missing contract tests:
1. `GET /metrics` unauthenticated → 401 problem+json; with `Authorization: Bearer $ADMIN_SECRET` → 200 `text/plain; version=0.0.4`.
2. Insert admin session with past `expires_at` → `GET /api/stats` → 401 (goes through HTTP + `require_admin`).
3. `REQUEST_TIMEOUT_SECS=1` (or minimum allowed) against `/api/extract` and `/api/research` → 504 `RequestTimeout` problem with `retryable` true — mirroring `tests/search_auth.rs:279-303`. If the min clamp forbids a fast timeout, use the same mechanism that test uses.

**Tests:** these ARE the tests. Keep them deterministic (providers already pinned to `127.0.0.1:9`).

**Verify:** `cargo test -p serpotter-api`.

### T-ci (Batch 9) — CI gates + container healthcheck port

**Source findings (archive §WebSpaGaps/TestsCiOpsGaps):** "CI never executes the SPA vitest suite"; "`docker-publish.yml` publishes semver images with zero quality gate" (and PR-only docker-smoke can cancel before publish); "Health probes hardcode port 8080 while `PORT` is a supported knob".

**Files:** `.github/workflows/ci.yml`, `.github/workflows/docker-publish.yml`, `Dockerfile`, `docker-compose.yml`, `docker-compose.prod.yml`, `docs/ops/deploy.md` (probe paragraph only).

**Fix:**
1. admin job: add `npm test` after `npm run check`.
2. `docker-publish.yml`: gate the push job on re-running the rust+admin checks (either `uses: ./.github/workflows/ci.yml` reusable-workflow refactor or duplicated gate jobs with `needs`) — do NOT publish from an unverified commit; keep the no-re-test intent for tags pointing at already-green main by making the gate cheap (`cargo test --workspace --locked` + `npm run build` + `npm test` at the tagged SHA).
3. HEALTHCHECK/probes use `${PORT:-8080}`: image `CMD` → shell form `curl -fsS "http://127.0.0.1:${PORT:-8080}/ready"`; compose `healthcheck.test` → `["CMD-SHELL", "curl -fsS http://127.0.0.1:$${PORT:-8080}/ready"]` (watch compose `$` escaping).

**Tests:** no local runtime for workflows — validate YAML with a parser (`python -c "import yaml,sys;yaml.safe_load(open(...))"` for both workflows + both compose files), and `docker build` only if Docker is available (report if not).

**Verify:** YAML parse + `cargo test --workspace` (unchanged code) + confirm `npm test` line present.

### T-docs (Batch 10) — Residual docs/comment drift sweep

**Source findings (archive §TestsCiOpsGaps "Ops-doc drift cluster" a–f; §DataLayerGaps #9 #10; §PoolsProvidersGaps F9; §ProductCoreGaps "Dead public API whose own docs/tests still claim it is the authority"; §ApiAdminGaps "`docs/ops/api.md` omits registered routes and newer product body fields"; §McpGaps stale comments).**

**Files:** root `AGENTS.md` (line 22 "schema v14" → follow the 0019/0020 reality; the rest of the file's schema notes), `crates/serpotter-db/AGENTS.md` (version + drop `bump_node_inflight` row + migration-0018 header note as prose), `crates/serpotter-api/src/admin/logs.rs:1` (stale `request_log` title), `crates/serpotter-product/src/cache.rs:20` (stale comment), `crates/serpotter-product/AGENTS.md` (delete `classify_proxied_http` row; `merge_providers_consulted_real` row), `docs/ops/api.md` (route table: `/metrics`, `change-password`, `sessions[/{id}]`, `nodes/{id}/test`; extract batch body fields; research `researchBackend`/`citationFormat`; request-logs `offset` + `errorKind`; usage clamp statement now 1..=180), `docs/ops/env.md` (residual: re-enable floor, alert cadence — T-credit already did its lines; verify not double-edited), `docs/ops/deploy.md` (literal `\n\n` markdown break at :183; `offset` param if documented there), `crates/serpotter-providers/AGENTS.md` if T-prov missed rows.

**Fix:** make every cited line match post-fix reality. Do not re-document what task-local edits already covered — grep first (`grep -n "schema v14\|currently 17\|serpotter.latest\|request_log" …`).

**Tests:** none (docs task). Verify by re-running the drift greps listed in the archive findings.

**Verify:** the specific greps return only intentional hits; `cargo test --workspace` still green (comments only).

### FINAL — Whole-branch review

After B10: controller runs full verification (`cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cd web && npm run typecheck && npm run check && npm test && npm run build`), then dispatches one final reviewer over the entire `gap-closure` diff against this plan + the audit archive, then finishing-a-development-branch.

---

## Self-review

- [x] **Spec coverage:** every P1/P2/P3 finding in the 7 audit reports maps to exactly one task (dedup: usage-days → T-adminapi; adm-token-logs → T-trace; SPA-vitest-CI → T-ci; hold-TTL guard → T-lease; reenable floor → T-credit; CACHE_TTL → T-apienv; cache-key findings → T-cache; seed-token/symlink → T-adminsec). Plan-review findings #1–#15 applied 2026-09-25. "Verified OK" sections contain no findings.
- [x] **Placeholder scan:** every task has Files, Fix (concrete), Tests (named behaviors), Verify (exact commands). Decisions left to implementer are explicitly marked "pick:" with a recommended conservative option.
- [x] **Type consistency:** migration numbering (0019→T-lease, 0020→T-dbh) and `EXPECTED_SCHEMA_VERSION` 18→19→20 are consistent and sequential; `SettingsPanel.tsx` owned solely by T-web1; `admin_crud.rs` owned solely by T-credit (B5) after T-apienv; `extract_url.rs` owned by T-question (B2) then T-leaseaux (B5) then T-mcp2 (B7) — sequential by batch.

