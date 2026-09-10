# Overnight Audit → Lease-Integrity Goal

**Date:** 2026-09-11 · **Tree:** `main` @ `0ff4b10` (clean) · **Gates at audit time:** `cargo fmt --check` ✅ · `cargo test --workspace --locked` 783 passed / 0 failed ✅ · `cargo clippy --workspace --all-targets --locked -- -D warnings` ✅

Six read-only audit agents (cohesion, claims-verification, Rust correctness, security, test surface, web/CI/ops) plus parent spot-verification of every P1 and the load-bearing P2s. 33,682 src lines (prod 18,457 / test 15,225); ~779 `#[test]`/`#[tokio::test]` macros counted by grep (approximate — attribute-generated cases; ground truth is the 783-test green run above).

---

## THE GOAL: Lease integrity — holder-scoped key/node accounting

One root cause behind all four P1s. The multi-hold pool model (`lease_until` = row-wide reclaim deadline, `inflight` = row-wide shared counter) has **no holder identity anywhere**, and one product flow already breaches the TTL by design.

### The verified defect chain

1. **The enabler — pre-refresh TTL gap** (parent-verified). Structured extract (`extract/extract_url.rs:432-473`) issues *start* (≤60 s HTTP) then the *first status call* (≤60 s) **before** the first `key_refresh.refresh()` at `:465`; the Tavily research loop (`extract/research.rs:988-1012`, refresh at `:1075-1078`) has the identical ordering. Worst unrefreshed segment ≈ **120 s vs `*_HOLD_TTL_SECS` = 90 s** (`db/src/lib.rs:29,35`). Batch extract is a single atomic vendor call — safe; normal search legs are ≤60 s — safe. Only these two flows.
2. **The damage — ID-only release/report** (parent-verified SQL). Every acquire first runs the reclaim `UPDATE api_keys SET inflight = 0, lease_until = NULL WHERE lease_until <= now` (`keys/acquire_report.rs:7-8`; nodes parity `nodes.rs:183`). If holder A overruns the gap above, request C reclaims + re-acquires the same row, and A's late finish — `release_api_key_inflight`/`report_api_key_success`/`report_api_key_failure`/`release_node_inflight`/`report_node_*` — is `WHERE id = ?` with no holder predicate (`acquire_report.rs:109-168`, `nodes.rs:209-263`). The `CASE WHEN inflight > 0` guard prevents negatives only.
3. **Refresh is also unscoped**: `WHERE id = ? AND inflight > 0` (`acquire_report.rs:270-279`, `nodes.rs:312-320`) — a stale holder's poll extends the *current* holder's lease; 0-rows-affected is silently `Ok(())`.

**Observed effect class (honest blast radius):** `inflight` is a documented *soft-cap heuristic* and `lease_until` an explicitly *non-exclusive* reclaim deadline — so the damage is silent over-subscription past `KEY_MAX_INFLIGHT` and mis-attributed lease/release accounting (premature `lease_until` clear → cascade reclaims while a live holder runs), **not** data corruption. Row-scoped health fields (`consecutive_fails`, credits burn) are legitimately per-row. The corruption is specifically the **inflight/lease accounting**.

### Fix design (two steps — A now, B is the goal)

- **Fix A (hotfix, ~15 lines, no schema):** refresh both holds immediately after job creation / before the first status call in both loops, bounding every unrefreshed segment to one ≤60 s HTTP call + tick margin < 90 s. Also make refresh *honest*: treat `rows_affected() == 0` as a lost lease (warn log at minimum).
- **Fix B (schema v19, correct multi-hold):** add a per-acquire holder token — `lease_token INTEGER` generated at acquire (e.g. `abs(random())`), returned in the `RETURNING`/pick, threaded `LeasedKey`/`NodeRow` → `KeyHold`/`ProxyHold` → every finish/refresh SQL: `WHERE id = ? AND lease_token = ?` (release/report/refresh scoped; reclaim stays row-wide). `report_*` returns a bool "still mine"; the pool logs a warn on lost-lease instead of silently mutating a fresh holder. Bonus: unlocks per-hold reclaim granularity later.
- Gate: fail-before-fix regression = force short TTL + expired-lease SQL state, assert late release cannot touch the new holder's `inflight`/`lease_until` (migrate.rs seam) and the poll-ordering test asserts refresh precedes first status call.

### Ride-along P2s (same wave, small)

- `keypool/src/lib.rs:81-87` + `outbound/src/lib.rs:83-93`: `KEY_/NODE_HOLD_TTL_SECS <= 0` silently clamps to **1 s** and bypasses `warn_if_hold_below_timeout` (`:306-311`) — reject or loudly default; widens every race above.
- `product/src/lease.rs:224`: `let _guard = span.enter();` held across awaits (`:242 .await` + all `finish_*`) — async span-enter leaks this request's `provider_attempt` span onto unrelated tasks polled on the worker; use `.instrument(span)` on the attempt future.
- `extract/research.rs:858-861`: synthesis `keys.acquire(SVC_XAI)` maps **all** errors (incl. `Db`) to silent `None` — classify/log like `lease.rs:170-184`.
- `extract/research.rs:462`: `Instant::now() + ctx.request_timeout` with unrestricted parsed `REQUEST_TIMEOUT_SECS` — `checked_add` + clamp (panic path on absurd config).

---

## Second-priority cluster (P2, independent)

1. **Live `adm-` session tokens land in durable logs.** `DELETE /api/admin/sessions/{token}` passes the full token as the path segment (`admin/session.rs:349-353`, route `lib.rs:153`, SPA caller `web/src/features/settings/queries.ts:52-54`) and `make_span` records `path = request.uri().path()` verbatim (`trace_layer.rs:114,120`) → every `request started/finished` line, stdout JSON + `LOG_DIR` rolling files, carries a valid 7-day admin credential. **Primary fix (one-line, kills the whole class): log axum's `MatchedPath` (the route template `/api/admin/sessions/{id}`) instead of the raw path in `make_span`.** Belt-and-braces, *not* a cheap tweak: revoking by a non-secret public handle requires a schema change — `admin_sessions.token` is itself the PRIMARY KEY holding the raw token in plaintext (`0008_admin_sessions.sql:7`, `admin_auth.rs:183-187`; accepted plaintext-at-rest threat model), so a separate id/handle column is a migration, not an API reshape.
2. **No throttle on admin credential endpoints** (`/api/admin/login`, `/bootstrap`, secret-gated paths) while the server binds `0.0.0.0:8080` (compose default publish `:44-46`). Pair with constant-time compare for `ADMIN_SECRET` (`admin/mod.rs:70-82`, currently `==`). Minimum: per-IP token bucket on the two routes.
3. **CI never runs the SPA suite**: `web/package.json:12` `test: vitest run` + 9 test files (781 LOC incl. auth-snapshot/session-end/log-paging) exist; admin job (`ci.yml:68-70`) runs only ci/check/build → add `npm test`.
4. **Movable `:latest` without a version trail**: `docker-publish.yml:54` enables `latest` on `workflow_dispatch`@main — a path with no semver release and no test gate in that workflow — while `docker-compose.prod.yml:43` defaults to it, so a prod `pull` can silently shift images with no release record. Drop `latest` from the dispatch flow (semver+sha only) or require a tag context.
5. **Test-hygiene flakes**: unguarded `set_var` + no restore in `api/tests/mcp_stateless.rs:631,722` and `search_auth.rs:279` (repo owns the ENV_LOCK pattern, `cron.rs:294`); fixed 15–50 ms sleep races `keypool/tests.rs:79,97,333`.
6. **Untested safety nets**: `hold.rs` Drop-release (`:143-158,207-217`) — zero dropped-armed-guard tests; `KeyPool::report_suspended` pool-level contract unpinned; main.rs two-stage drain state machine (`:137-175`) inline/untestable.

## Third: hygiene & docs (P3, batchable)

- **AGENTS.md drift** (3): root STRUCTURE says schema v14 (is 18); `serpotter-db/AGENTS.md:7` says 17; CI line omits fmt-check + `npm run check`; COMMANDS "matches CI" block under-describes (add `--locked --all-targets`, fmt). `web/AGENTS.md:25` stale `components/` claim; `docs/ops/env.md`/`api.md` missing request-logs `offset` param; `deploy.md:180` literal `\n\n` corruption + `:46` dangling cross-ref; `AGENTS.md:122` `npm i` vs lockfile CI (use `npm ci`); `vite.config.ts:20` dead `/live` proxy; `VITE_API_BASE` undocumented.
- **Cohesion (14 src files > 350 prod LOC; same methodology as the 07-28 restructure, which these files outgrew since)**: worst `extract/research.rs` 1,103 (3 jobs: research/deep/tavily-backend) and `extract_url.rs` 1,025 (5 jobs: chain/structured/batch/variants); four provider adapters 572–689 (client + pure filters/policy split per vendor); `api/events.rs` 638; `mcp/mod.rs` 605; `core/types.rs` 494; `mcp/params.rs` 435; `search/execute.rs` 533; `admin/keys.rs`/`session.rs` 374/372 with **zero tests each**; `validation.rs` 428 argued-to-stay. `lease.rs` prod 338 OK but move its 739-line test mod to sibling `tests.rs` per keypool precedent. Web: `NodesPanel` 580 / `KeysPanel` 489 / `TokensPanel` 368 — extract dialogs.
- **Vacuous tests** `providers/http.rs:143-163`, `cron.rs:535-541` (assert-free "does not hang" with no armed timeout).

## Suggested execution order

**Status (2026-09-11):** step 1 LANDED — `ee30635` (Fix A: post-start refresh in both poll loops, `rows_affected` honest refresh, TTL `<=0` fallback+warn, `Instrument` span fix, synthesis acquire warn, 24 h timeout clamp, `test_node` userinfo redaction; tests pinned) + `bbdb989` (this spec). Steps 2–5 open.

1. Fix A hotfix + ride-along P2s (1 wave, product+keypool+outbound+db, no migration). ✅
2. Security cluster (sessions-in-logs + throttle + CI npm test + publish gate).
3. Fix B lease-token (schema v19) with the Drop-net tests folded in — this is the goal's spine.
4. AGENTS/docs drift sweep (mechanical, can ride anywhere).
5. Cohesion splits (research/extract first — they also make wave-3's lease threading smaller).
