# Failure-Class Observability (402/401/403/429) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development with dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the handling of the four provider failure classes (insufficient credits 402, invalid key 401, forbidden 403, rate limited 429) queryably observable end-to-end: a per-attempt outcome counter fed through the existing `events::emit` funnel, verdict fields on WARN lines, key-transition logs/metrics, per-request attempt/transition fields in the ring + admin filters, plus the honesty fixes the audit tied to them (auth_fail reason, Retry-After cooldown, ban archive, drained-credit retryability).

**Architecture:** Product layer records plain data on `ExecMeta` (attempt outcomes, key transitions) at the one shared dispatch site (`lease.rs` `with_key_proxy` → `Ok(_) => Ok, Err(e) => report(e)`); the API layer observes it from `events::emit` into new `IntCounterVec`s in the dedicated metrics registry. Product never imports prometheus (product purity). Schema 0021 (cooldown_until, api_keys_archive, auth_fail backfill) is owned by exactly ONE task.

**Tech Stack:** Rust workspace (axum/tracing/prometheus/sqlx), React SPA. All gates pinned: `cargo fmt --all --check`, `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, web `npm run typecheck && npm run check && npm test && npm run build`.

---

## Hard rules (from review)

1. **One migration owner:** only Task 8 creates `migrations/0021_*.sql` and bumps `EXPECTED_SCHEMA_VERSION` (20→21).
2. **Product never depends on `serpotter-api`/prometheus:** counters are observed in `events::emit`; product only records data.
3. **Cooldown is ordering-demotion, never a filter:** `cooldown_until` joins the acquire `ORDER BY` as a demote tier; cooling keys remain servable when nothing better exists.
4. **Counter labels are bounded:** `(service, outcome)` and `(service, transition)` only. Never `key_id`, never raw status as label. Per-key state stays in the keys admin API + ring.
5. **Counter emission site:** every attempt outcome recorded at the shared `lease.rs` dispatch (plus the `client_for` failure arm and the two bespoke `research.rs` xAI sites), so swallowed legs (`|_| report` closures) cannot dodge the count.
6. **No deploy this campaign** (CI/CD scope is build+push only).

## Outcome label set (closed, bounded — 8 values)

`ok` · `payment_required` (402) · `rate_limited` · `auth_invalid` (401) · `forbidden` (403) · `banned` · `retryable` (5xx/transport/unknown-429) · `failure` (everything else)

**`rate_limited` semantic (documented, not literal):** it is the label for `ReportMode::Exhausted`, which fires when the status is exhausted *per `is_exhausted_status`* — 429 plus tavily's 432/433 plan-limit codes. 402 can NEVER land here (`verdict_for` checks payment-required first). The label means "vendor told us to back off / plan-capped", not strictly HTTP 429. Reused verbatim by the WARN `verdict` field (T2).

Derived at the dispatch from `(ReportMode, Option<upstream_status>)` — 401 vs 403 stay distinct even though `verdict_for` folds them into `AuthFailure`.

## Batch map (file ownership; sequential batches, parallel tasks inside)

| Batch | Tasks (parallel) | Owns | Gate after batch |
|---|---|---|---|
| B1 | T1 meta ∥ T2 warn | product: meta.rs, lease.rs ∥ run_provider.rs, extract_url.rs, research.rs WARN sites | rust fmt+test+clippy |
| B2 | T3 metric ∥ T4 authfail ∥ T5 fallbacklog | api: metrics.rs, events.rs ∥ db: acquire_report.rs + rows.rs docs + web disabled-reason.tsx ∥ product: chain.rs, extract_url fallback site | rust gates + web gates (web touched) |
| B3 | T6 keytrans (alone) → T7 eventenrich (alone) — **sequential**: T7 consumes T6's `TransitionRecord` and both own events.rs/metrics.rs | product: hold.rs, lease.rs, meta.rs → api: events.rs, admin/logs.rs, web RowDetail.tsx | rust gates + web gates |
| B4 | T8 schema (ALONE) | db: migrations/0021, lib.rs version, migrate.rs pins | rust gates |
| B5 | T9 cooldown ∥ T10 banarchive ∥ T11 retrycopy | providers/* + db acquire ∥ db archive fn + keypool ∥ product extract/search + api errors | rust gates |
| B6 | T12 docs | docs/ops/*, AGENTS.md notes | rust gates (fmt only) + web gates if web docs touched |

Controller commits after each green batch (conventional subject, no `--no-verify`).

## File structure (new/modified responsibilities)

- `crates/serpotter-product/src/meta.rs` — `AttemptRecord`, `TransitionRecord`, `ExecMeta.attempt_log`/`key_transitions`, `note_outcome`/`note_transition`, absorb merge.
- `crates/serpotter-product/src/lease.rs` — `outcome_label()` fn; record at dispatch.
- `crates/serpotter-api/src/metrics.rs` — `serpotter_provider_attempt_total{service,outcome}`, `serpotter_key_transition_total{service,transition}` + test helpers.
- `crates/serpotter-api/src/events.rs` — observe loops in `emit`; `LogFields`/ring additions; ring filter.
- `crates/serpotter-db/src/keys/acquire_report.rs` — auth_fail CASE, cooldown column usage, post-state returns.
- `crates/serpotter-db/migrations/0021_observability.sql` — cooldown_until + api_keys_archive + auth_fail backfill (Task 8 ONLY).

## Task 1 (B1, parallel): T-meta — attempt outcome recording

**Files:**
- Modify: `crates/serpotter-product/src/meta.rs` (ExecMeta + absorb)
- Modify: `crates/serpotter-product/src/lease.rs` (dispatch recording, ~lines 258-264, 344-346)
- Modify: `crates/serpotter-product/src/extract/research.rs` (bespoke xAI sites ~949, ~966)
- Test: unit tests in `meta.rs` + `lease.rs` tests module

- [ ] **Step 1: Add `AttemptRecord` + field + absorb merge to meta.rs**

```rust
/// One finished provider attempt's class-level outcome (observability
/// funnel). Bounded label set — see plan `outcome_label`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRecord {
    pub service: String,
    pub key_id: i64,
    /// One of the 8 closed outcome labels: ok | payment_required |
    /// rate_limited | auth_invalid | forbidden | banned | retryable | failure
    pub outcome: &'static str,
    pub upstream_status: Option<u16>,
}
```
On `ExecMeta`: `pub attempt_log: Vec<AttemptRecord>,` (derives Default already). In `ExecMeta::absorb`: `self.attempt_log.extend(other.attempt_log.iter().cloned());` next to the existing field merges.

- [ ] **Step 2: Add `note_outcome`**

```rust
pub fn note_outcome(&mut self, service: &str, key_id: i64, outcome: &'static str, upstream_status: Option<u16>) {
    self.attempt_log.push(AttemptRecord { service: service.to_string(), key_id, outcome, upstream_status });
}
```
Keep `note_attempt` unchanged (sticky key_id / providers_consulted logic untouched).

- [ ] **Step 3: lease.rs — `outcome_label` + dispatch recording**

```rust
/// Closed outcome label for the observability counter (plan §label set).
/// `AuthFailure` splits on the upstream status: 403 → forbidden, else auth_invalid.
pub fn outcome_label(mode: ReportMode, upstream_status: Option<u16>) -> &'static str {
    match mode {
        ReportMode::Ok => "ok",
        ReportMode::PaymentRequired => "payment_required",
        ReportMode::Exhausted => "rate_limited",
        ReportMode::AuthFailure if upstream_status == Some(403) => "forbidden",
        ReportMode::AuthFailure => "auth_invalid",
        ReportMode::Banned => "banned",
        ReportMode::Retryable => "retryable",
        ReportMode::Failure => "failure",
    }
}
```
At the main dispatch (right after `meta.note_attempt(service, key_id, node_id, result.is_ok());` at ~:344):

```rust
let upstream_status = match &result {
    Err(ProviderError::Upstream { status, .. }) => Some(*status),
    _ => None,
};
meta.note_outcome(service, key_id, outcome_label(verdict, upstream_status), upstream_status);
```
Also record in the `client_for` failure arm (~:262, verdict `Failure`, status `None`).

- [ ] **Step 4: research.rs bespoke xAI sites (~949 success / ~966 failure)** — add the same `meta.note_outcome(...)` beside each `note_attempt`. At the failure arm (research.rs:961-968) the error is currently a wildcard `_` discarded by the timeout wrapper: **bind it first** (restructure the arm so the inner `Result`'s error is a named `e`) so `verdict_for(SVC_XAI, &e)` and its upstream status are reachable; record `outcome_label(mode, status_of(&e))`. If the wrapper truly cannot expose it, record `("failure", None)` and flag DONE_WITH_CONCERNS — do not guess.

- [ ] **Step 5: research.rs WARN verdict field** (B1 file-ownership: T1 owns research.rs entirely, so this one-liner lives here, not in T2) — at the `provider upstream error; full body logged` WARN in `map_tavily_poll_error` (research.rs:1011-1012) add `verdict = crate::lease::outcome_label(mode, Some(*status)),` computing `let mode = verdict_for(SVC_TAVILY, &e);` if `mode` is not in scope.

- [ ] **Step 6: re-export the type** — `crates/serpotter-product/src/lib.rs` has a fixed `pub use` list for the private `meta`/`lease` modules; add `pub use meta::AttemptRecord;` (api crate must name it in `LogFields`). Grep existing re-exports first and follow the pattern.

- [ ] **Step 7: tests** — meta absorb merges `attempt_log`; lease dispatch test: seeded 401 key → `outcome == "auth_invalid"` + `upstream_status == Some(401)`; 402 → `payment_required`; Ok → `ok`, status None. Run `cargo test -p serpotter-product`.

## Task 2 (B1, parallel): T-warn — verdict field on WARN lines

**Files (B1 ownership: T2 owns run_provider.rs + extract_url.rs ONLY — research.rs belongs to T1):**
- Modify: `crates/serpotter-product/src/search/run_provider.rs` (~227-256 WARN block)
- Modify: `crates/serpotter-product/src/extract/extract_url.rs` (WARN blocks at :328 and :346)
- Test: existing WARN-path tests extended with field assertion where present

- [ ] **Step 1:** In both `provider upstream error; full body logged` WARN blocks (run_provider.rs, extract_url.rs), add a `verdict = ...` field next to the existing `reason = "upstream_error"`: `verdict = crate::lease::outcome_label(mode, Some(*status)),` (`mode` is already in scope at both sites; the banned arm already branches on `mode == ReportMode::Banned` — the banned WARN gets `verdict = "banned"`).
- [ ] **Step 2:** Run `cargo test -p serpotter-product`; grep tests asserting the WARN via `tracing_test`/capture if present, else add one field assertion where the harness exists. No new test harnesses.

## Task 3 (B2, parallel): T-metric — attempt counter + emit wiring

**Files:**
- Modify: `crates/serpotter-api/src/metrics.rs` (Metrics struct, LazyLock, observe fns, test helper)
- Modify: `crates/serpotter-api/src/events.rs` (`emit` — observe loop from `meta.attempt_log`)
- Test: `metrics.rs` tests + one funnel test in `tests/events_funnel.rs`

- [ ] **Step 1:** Add to `Metrics`: `provider_attempt_total: IntCounterVec` with labels `["service","outcome"]` (new on its own registry, following the existing counter constructors). `pub fn observe_attempt(service: &str, outcome: &str)` bumps it.

- [ ] **Step 2 (carrier — named, no hedge):** `events::emit` receives `fields: LogFields`, not `ExecMeta` (events.rs:537); `ExecMeta` is consumed earlier by `fields_from_meta` (events.rs:118-127). So:
  - `LogFields` gains `pub attempt_log: Vec<AttemptRecord>` (re-exported from product per T1 Step 5), populated in `fields_from_meta`.
  - In `emit`, next to the existing `metrics::observe(...)` call:

```rust
for rec in fields.attempt_log.iter() {
    metrics::observe_attempt(&rec.service, rec.outcome);
}
```

- [ ] **Step 3 (shared test harness):** add ONE scripted-provider helper to the api test `common` module — `spawn_scripted(status: u16, body: &'static str) -> SocketAddr` (a oneshot listener that answers every request with the given status/body), because the existing `spawn_blackhole` only produces timeouts (`ProviderError::Http` → `Retryable` → `finish_release`) and the dead `127.0.0.1:9` endpoints can never reach fail@3/402/403 states. T3/T6/T7 funnel tests all use it; reuse `common`'s existing provider-override mechanism to point a service at it.

- [ ] **Step 4:** Test helper `#[doc(hidden)] pub fn test_attempt_count(service: &str, outcome: &str) -> u64` mirroring `test_requests_count`. Funnel test with `spawn_scripted`: tavily answers 401 for two attempts then a scripted/fallback provider answers 200 (or, simplest, 401-only so the final row is 502) → assert `test_attempt_count("tavily","auth_invalid") >= 1` — proof that absorbed failures are countable.
- [ ] **Step 5:** Run `cargo test -p serpotter-api`.

## Task 4 (B2, parallel): T-authfail — stamp `auth_fail` at fail@3

**Files:**
- Modify: `crates/serpotter-db/src/keys/acquire_report.rs` (failure arm of `release_key_token`, and `note_key_health_failure`)
- Modify: `crates/serpotter-db/src/keys/rows.rs` (~39-47 disposition doc)
- Modify: `web/src/features/keys/disabled-reason.tsx` (warn styling for `auth_fail`)
- Docs: `crates/serpotter-db/AGENTS.md` (~57-63)
- Test: `crates/serpotter-db/tests/migrate.rs` (fail@3 stamps reason; re-enable clears it)

- [ ] **Step 1:** Both failure-arm SQLs get a companion CASE (same `consecutive_fails + 1 >= ?` predicate that flips `active`):

```sql
active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END,
disabled_reason = CASE WHEN disabled_reason IS NULL AND consecutive_fails + 1 >= ? THEN 'auth_fail' ELSE disabled_reason END
```
(binds: max_fails twice + id, keep the existing bind order coherent. **The `IS NULL` guard is mandatory** — mirror `set_api_key_active` (admin_crud.rs:194-198): a concurrent leg's `vendor_suspended` marker must never be clobbered by the cron-eligible `auth_fail`, or the 0018 resurrection regression returns. The stamp predicate is strictly narrower than the flip predicate by design.)

- [ ] **Step 2:** Verify the re-enable paths (`reenable_stale_keys`, admin toggle/key rotation) already NULL `disabled_reason` when re-activating — if not, make them do so (AGENTS.md: "NULL = never-disabled-or-re-enabled"). Preserve cron skip-list as-is: `vendor_suspended` only → `auth_fail` rows still return after `KEY_REENABLE_AFTER_HOURS`.
- [ ] **Step 3:** `rows.rs` doc + `serpotter-db/AGENTS.md` disposition table: inactive `NULL` + `consecutive_fails >= 3` becomes **`'auth_fail'`** for new flips (the NULL case shrinks to *pre-0021 legacy*, which Task 8 backfills).
- [ ] **Step 4:** Web: `auth_fail` renders in `disabled-reason.tsx` — add it to the warn-styled branch if styling is enumerated per-reason (read first; verbatim fallback already renders).
- [ ] **Step 5:** Tests: fail@3 → `disabled_reason == 'auth_fail'` + `active == 0`; re-enable → `disabled_reason IS NULL`; non-failure arms never touch the reason. Then web gates (`typecheck`, `check`, `test`) + rust db tests.

## Task 5 (B2, parallel): T-fallbacklog — server-side fallback visibility

**Files:**
- Modify: `crates/serpotter-product/src/search/chain.rs` (~55-59 fallback emission)
- Modify: `crates/serpotter-product/src/extract/extract_url.rs` (~156-162 fallback emission)
- Test: extend existing chain/fallback tests if a log-capture harness exists; otherwise assert via existing progress-test structures — do not build new harnesses.

- [ ] **Step 1:** At each `ProgressEvent::Fallback` construction site add:

```rust
tracing::info!(from = from_svc, to = to_svc, reason = reason.as_str(), "provider fallback");
```
- Use the actual variable names at the site (reason strings already exist, e.g. "out of credits"). One line per emission, level INFO, target stays the module default. **String fields are BARE (no `%` sigil)** — house convention (events.rs audit line, all WARN blocks): bare `&str` yields clean JSON strings in the LOG_DIR layer (`"service":"tavily"`), while `%` embeds escaped quotes and breaks jq queries.
- [ ] **Step 2:** Same treatment for `ProgressEvent::Retry` if constructed adjacent (reason + attempt); if retries are emitted only from the `run_provider` ladder, add it there beside the existing ladder logging.
- [ ] **Step 3:** `cargo test -p serpotter-product`.

## Task 6 (B3, alone — runs first): T-keytrans — key-transition post-state, logs, metric

**Files:**
- Modify: `crates/serpotter-db/src/keys/acquire_report.rs` (report fns return post-state, `RETURNING`)
- Modify: `crates/serpotter-keypool/src/lib.rs` (`report_*`, `revoke_key_row` — derive transition + WARN)
- Modify: `crates/serpotter-product/src/hold.rs` (`finish_*` return `KeyTransition`)
- Modify: `crates/serpotter-product/src/lease.rs` (record transition on meta after each finish arm)
- Modify: `crates/serpotter-product/src/meta.rs` (`TransitionRecord` + absorb merge)
- Modify: `crates/serpotter-api/src/events.rs` + `metrics.rs` (`observe_key_transition` + loop — mirror Task 3)
- Test: db, keypool, product, api funnel tests

- [ ] **Step 1 (db):** Replace `Ok(bool)` returns on `report_api_key_failure_lease` / `_exhausted_lease` / `_payment_required_lease` / `suspend_api_key_lease` with `Ok(KeyPostState)` where:

```rust
/// Post-report row state (observability: derive the transition, never bool).
pub struct KeyPostState {
    pub existed: bool,          // lease token resolved to a row
    pub active: bool,
    pub consecutive_fails: i64,
    pub credits_remaining: Option<i64>,      // AFTER the update
    pub credits_before: Option<i64>,         // read in the SAME tx before the UPDATE
}
```
Use `SELECT credits_remaining FROM api_keys WHERE id = ?` (as `credits_before`) then `UPDATE ... WHERE id = ? RETURNING active, consecutive_fails, credits_remaining` inside the existing transaction (Step 2 needs both to detect real transitions); `existed=false` when the lease token is gone (the current lost-lease WARN path). Keep `release_api_key_lease` (health `None`) as bool — no transition there.

- [ ] **Step 2 (keypool):** `report_*` map post-state → `KeyTransition` and WARN on real flips:

```rust
pub enum KeyTransition { None, Disabled, CreditsZeroed, Suspended, Deleted }
```
- **Transition detection compares pre-state to post-state** (RETURNING alone re-counts already-zero keys, because zero-credit rows are still acquirable): read `credits_remaining` BEFORE the UPDATE in the same transaction; a `CreditsZeroed` transition fires only when the value actually changed (`pre != post`). `Disabled` is safe from post-state alone (only `active = 1` rows are leased).
- failure + `!active` → `Disabled` + `warn!(key_id, service, "api key disabled after consecutive failures", consecutive_fails)`
- credits actually changed to 0 → `CreditsZeroed` + INFO (not WARN)
- suspend → `Suspended` + WARN
- `revoke_key_row` (rows_affected>0 from T10's returning delete) → `Deleted` + WARN
- `existed == false` keeps today's lost-lease WARN, transition `None`.
Signatures: `-> Result<KeyTransition, KeyPoolError>`. **keypool must not reference serpotter-api or prometheus.**

- [ ] **Step 3 (hold):** `finish_failure/exhausted/payment_required/suspended/banned` return the `KeyTransition` (they already `if ...is_ok() { disarm() }` — return the transition on Ok, `None` on Err).

- [ ] **Step 4 (lease + meta):** `meta.note_transition(service, key_id, transition)` right after each finish arm in `with_key_proxy` (also record `Deleted` from the banned arm). `TransitionRecord { service: String, key_id: i64, transition: &'static str }` with `transition` ∈ {disabled, credits_zeroed, suspended, deleted}; absorb merges like `attempt_log`. **Re-export:** add `pub use meta::TransitionRecord;` to `crates/serpotter-product/src/lib.rs` (T7's `LogFields` names it) — same pattern as T1 Step 5.

- [ ] **Step 5 (api):** metrics: `serpotter_key_transition_total{service,transition}` + `observe_key_transition` + `test_key_transition_count`; emit loop mirrors Task 3; ring/log field arrives in Task 7.

- [ ] **Step 6: tests** — db post-state (fail@3 → `active:false`); keypool transition mapping; funnel: one request that fail@3-disables a key asserts `test_key_transition_count("tavily","disabled") >= 1`. Gates: rust + (web untouched, skip web).

## Task 7 (B3, alone — after T6): T-eventenrich — ring/admin/web surfacing

**Files:**
- Modify: `crates/serpotter-api/src/events.rs` (`LogFields`/`fields_from_meta`, ring entry, filter)
- Modify: `crates/serpotter-api/src/admin/logs.rs` (~20-39 query params)
- Modify: `web/src/features/logs/RowDetail.tsx` (+ `queries.ts` URL param if filter added)
- Test: `events` ring tests, `admin_logs` API test, web logs query test

- [ ] **Step 1 (LogFields):** add three camelCase fields derived from `ExecMeta` (anchor `fields_from_meta`):
  - `attemptOutcomes`: `Vec<AttemptRecord>` → CSV `"tavily:auth_invalid:401,firecrawl:ok"` (service:outcome:status, status omitted when None)
  - `lastUpstreamStatus`: `Option<i64>` — status of the LAST record that has one
  - `keyIds`: CSV of `attempt_log` key_ids (distinct, order-preserving); falls back to existing single `key_id` when log empty
  - `keyTransitions`: CSV of `TransitionRecord` → `"tavily:disabled"` (empty string when none)
- [ ] **Step 2 (ring row):** mirror the four fields on the ring row struct + serialization (ring is in-memory JSON — follow existing optional-field pattern, absent vs null per house style).
- [ ] **Step 3 (admin filter):** `GET /api/request-logs` accepts `lastUpstreamStatus=<u16>` (exact match; `None` rows excluded when the param is present). Wire through the same filter chain as `status`/`service`.
- [ ] **Step 4 (web):** `RowDetail.tsx` renders attempt outcomes / transitions / lastUpstreamStatus / keyIds rows; `queries.ts` + LogsPanel pass `lastUpstreamStatus` if exposed in UI — minimum: display in RowDetail, filter param exposed in `queries.ts` test only if trivial.
- [ ] **Step 5: tests** — funnel row carries the CSV after a fallback sequence (reuse T3's `spawn_scripted` harness); admin filter returns only matching rows; web RowDetail renders new fields (existing logs test files). Gates: rust + web.

## Task 8 (B4, alone): T-schema — migration 0021

**Files:**
- Create: `crates/serpotter-db/migrations/0021_cooldown_archive_authfail.sql`
- Modify: `crates/serpotter-db/src/lib.rs` (`EXPECTED_SCHEMA_VERSION` 20→21)
- Modify: `crates/serpotter-db/tests/migrate.rs` (version pin + new pins)
- Docs: root `AGENTS.md` (schema note), `crates/serpotter-db/AGENTS.md`

- [ ] **Step 1: migration content** (checksum-frozen once committed — get it right):

```sql
-- 0021: rate-limit cooldown, ban archive, fail@3 reason backfill.
ALTER TABLE api_keys ADD COLUMN cooldown_until DATETIME;

CREATE TABLE api_keys_archive (
    id INTEGER PRIMARY KEY,
    api_key_id INTEGER NOT NULL,
    service TEXT NOT NULL,
    key_fingerprint TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL,               -- 'vendor_banned' etc; NEVER the key text
    consecutive_fails INTEGER NOT NULL DEFAULT 0,
    credits_remaining INTEGER,
    archived_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Legacy fail@3 rows: inactive + NULL reason + fails >= 3 is the documented
-- fail@3 signature (root AGENTS.md disposition rule).
UPDATE api_keys SET disabled_reason = 'auth_fail'
 WHERE active = 0 AND disabled_reason IS NULL AND consecutive_fails >= 3;

-- Version stamp: EVERY migration ends with this (0002..0020 pattern) —
-- without it schema_version stays 20, /ready 503s and migrate.rs fails.
UPDATE schema_version SET version = 21 WHERE id = 1;
```

- [ ] **Step 2:** Bump `EXPECTED_SCHEMA_VERSION` to 21 (single const; `/ready` follows).
- [ ] **Step 3: tests in migrate.rs** — schema version == **21** (literal); `cooldown_until` column exists and is NULL by default; archive table empty + insert/select roundtrip without key material; backfill: seed an old-shape fail@3 row (inactive, NULL, fails=3) + an inactive NULL fails=0 row → only the first becomes `auth_fail`.
- [ ] **Step 4:** Docs: root `AGENTS.md` schema bullet (v21 = cooldown_until + api_keys_archive + auth_fail backfill), db `AGENTS.md`. **Also document:** `cooldown_until` is write-once-per-429 and NEVER cleared (no cron touches it; stale timestamps are inert under the acquire's `> datetime('now')` predicate) — deliberate, no cleanup job. Gates: rust.

## Task 9 (B5, parallel): T-cooldown — Retry-After parse + cooldown demotion

**Files:**
- Modify: `crates/serpotter-providers/src/http.rs` (`parse_retry_after` helper)
- Modify: `crates/serpotter-providers/src/{tavily,firecrawl,exa,xai}.rs` (Upstream construction sites)
- Modify: `crates/serpotter-providers/src/lib.rs` (`Upstream` gains `retry_after_secs: Option<u64>`)
- Modify: `crates/serpotter-product/src/hold.rs` + `crates/serpotter-db/src/keys/acquire_report.rs` (stamp `cooldown_until`)
- Test: providers parse tests, db ordering test, lease integration

- [ ] **Step 1:** `ProviderError::Upstream` gains `pub retry_after_secs: Option<u64>`. **Exhaustive sweep (compile-enforced):** a struct literal without `..` requires every field, so find ALL sites by grep/ast across EVERY crate, not just providers — known set: 4 providers' non-2xx arms, `extract/research.rs:1087,1110,1158,1174` (synthetic poll errors → `retry_after_secs: None`), `api/credit_sync.rs:101`, and test helpers in `lease.rs` (~9), `extract_url.rs`, `run_provider.rs`, `research.rs`. Every literal gains `retry_after_secs: None` except the provider non-2xx arms that parse the header (read it BEFORE `res.text()` consumes the body):

```rust
let retry_after_secs = crate::http::parse_retry_after(res.headers());
```

```rust
/// Delta-seconds only (`Retry-After: 120`). HTTP-date values parse as None —
/// honest absence beats a wrong cooldown.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers.get(reqwest::header::RETRY_AFTER)?
        .to_str().ok()?
        .trim().parse::<u64>().ok()
}
```

- [ ] **Step 2 (stamp):** `verdict` path: when the mode is `Exhausted` (429/432/433), the key gets `cooldown_until = datetime('now', '+' || ? || ' seconds')` — extend `report_api_key_exhausted_lease(token, cooldown_secs)` / `release_key_token(Some("exhausted"))` arm; seconds = `retry_after_secs.min(3600)` when present, **60 default** when absent. `hold::finish_exhausted` gains `retry_after_secs: Option<u64>` (no default argument — a default would silently record 60 s at unlisted sites). **Exhaustive caller list: grep `finish_exhausted` in the workspace and update EVERY call** (known: hold.rs definition, lease.rs main dispatch `ReportMode::Exhausted` arm, research/extract legs if any, lease.rs tests) — thread the value from the `ProviderError` where one is in hand, `None` otherwise. `cooldown_until` is never cleared (documented in T8).
- [ ] **Step 3 (demote, never filter):** acquire `ORDER BY` gets the cooldown tier FIRST (acquire_report.rs:72):

```sql
ORDER BY CASE WHEN cooldown_until IS NOT NULL AND cooldown_until > datetime('now') THEN 1 ELSE 0 END,
  CASE WHEN credits_remaining = 0 THEN 1 ELSE 0 END, …
```
(keep the remaining keys/tiers byte-identical). No `WHERE cooldown_until` predicate anywhere.

- [ ] **Step 4: tests** — `parse_retry_after` (valid, garbage, missing, HTTP-date→None); acquire: cooling key picked only when no non-cooling key is free (seed two rows); 429 verdict sets `cooldown_until` in the future, `credits_remaining` untouched when NULL; lease integration: header value caps at 3600. Gates: rust.

## Task 10 (B5, parallel): T-banarchive — archive before hard DELETE

**Files:**
- Modify: `crates/serpotter-db/src/keys/` (new `archive_and_delete_api_key` replacing `delete_api_key` internals)
- Modify: `crates/serpotter-keypool/src/lib.rs` (`revoke_key_row`)
- Test: db migrate/keypool tests

- [ ] **Step 1:** Transactional archive-then-delete, returning rows_affected (Task 6 expects a count from `revoke`):

```sql
INSERT INTO api_keys_archive (api_key_id, service, key_fingerprint, reason, consecutive_fails, credits_remaining)
SELECT id, service, COALESCE(key_fingerprint, ''), 'vendor_banned', consecutive_fails, credits_remaining FROM api_keys WHERE id = ?;
DELETE FROM api_keys WHERE id = ?;
```
**No `key` column is ever copied** — fingerprint only (audit-trail, not credential storage). `COALESCE` is mandatory: `api_keys.key_fingerprint` is nullable (0003) and a SELECT-supplied NULL would abort the NOT NULL archive column — killing the ban transaction on legacy rows.

- [ ] **Step 2:** `revoke_key_row` calls it, keeps `notify_waiters`, returns/warns `Deleted` (Task 6 contract). Existing `account_banned` WARN with `disposition=deleted` stays the request-side record.
- [ ] **Step 3: tests** — archive row exists after ban (fields sane, `key_fingerprint` present, no key text column), source row gone, second revoke is a no-op (archive not duplicated — `INSERT ... SELECT` on missing row inserts nothing), **NULL-fingerprint legacy row revokes successfully (archived with `''`)**. Gates: rust.

## Task 11 (B5, parallel): T-retrycopy — honest retryability + real verdicts on extract legs

**Files:**
- Modify: `crates/serpotter-product/src/extract/extract_url.rs` (~483, ~799, ~963, ~1115 single-attempt legs; ~859 batch copy)
- Modify: `crates/serpotter-product/src/search/run_provider.rs` (final-map class)
- Modify: `crates/serpotter-api/src/product/errors.rs` (status/kind mapping) + `crates/serpotter-api/src/mcp/errors.rs` path (kind passthrough)
- Test: `crates/serpotter-api/tests/` (research_parity / mcp_tools 402 pins), `errors.rs` unit pins updated DELIBERATELY

- [ ] **Step 1 (real verdicts — scoped):** the single-attempt legs pass `|_| ReportMode::Failure` (extract_url.rs ~483, ~799, ~963, ~1115 **and** research.rs:1073) — replace with a closure that runs `verdict_for` but **remaps `Banned` → `AuthFailure`**:

```rust
|e| match verdict_for(PROVIDER, &e) {
    // Structured legs never hard-DELETE: a ban-phrase match on a
    // structured-extract body must not irreversibly destroy a healthy
    // Firecrawl key (lease.rs ban tier deletes firecrawl rows).
    ReportMode::Banned => ReportMode::AuthFailure,
    m => m,
}
```
Consequence: 402 demotes (`finish_payment_required`) and 401/403 accumulate fail@3 on those legs — that is the fix; hard-DELETE stays exclusive to the search/main paths. **Test:** a ban body on a structured leg does NOT delete the row (row exists, `active=0`).

- [ ] **Step 2 (drained-credit final):** when the FINAL ladder error classified `PaymentRequired`, stop presenting it as generic retryable 502:
  - REST: `503` + type `/CreditsExhausted` + `retryable: false` (detail keeps the honest `"{provider} is out of credits (upstream 402)"` copy).
  - MCP: kind `CreditsExhausted`, `kind_retryable` gains it in the FALSE set (errors.rs:25-27) — retrying a drained account cannot help until top-up.
  - **Decided plumbing (no ReportMode re-export needed):** new thiserror variant on `SearchExecError`/`ExtractError` (`CreditsExhausted(String)`, same message), produced by the ladders when the final mode is `PaymentRequired` and surfaced through the existing api `product/errors.rs` mapping (search/extract/research problem builders gain the 503 arm). Deep path (`search/execute.rs:550`) routes 402 through the same variant.
  - **Pins that actually break — disposition per pin:**
    - `errors.rs:169-187` `search_vendor_rejected_statuses_map_to_provider_error` hand-builds strings — move the 402 case OUT into its own test asserting `(503, "CreditsExhausted", !kind_retryable)`; 401/403/429 rows stay 502-retryable.
    - `errors.rs:222-227` (deep `"exa deep upstream error (status 402)"` → 502) — moves to 503 `CreditsExhausted` (deep path changes too).
    - `run_provider.rs:404-410` and `extract_url.rs:1832-1834` product message assertions — 402 rows update to the new class (message text unchanged).
    - `kind_retryable` table test gains `CreditsExhausted: false`.

- [ ] **Step 3 (batch copy):** extract_url.rs ~859 generic arm routes 402 to the same out-of-credits copy as the main path (~247-249) before the generic `status N` fallback.

- [ ] **Step 4: tests** — REST: drained search/extract/deep → 503 `/CreditsExhausted` + `retryable:false`; MCP: envelope `kind=CreditsExhausted, retryable:false`; single-attempt leg 402 now zeroes credits (db assert); ban body on structured leg does not delete; 401 legs still `ProviderError` 502. Gates: rust (api tests + product + clippy).

## Task 12 (B6): docs sweep

**Files:** `docs/ops/api.md`, `docs/ops/env.md` (only if a knob emerged — none planned), root `AGENTS.md`, `crates/serpotter-api/AGENTS.md` (metrics/events notes), `crates/serpotter-db/AGENTS.md` (0021, auth_fail, cooldown, archive), `crates/serpotter-product/AGENTS.md` if present (verdict/outcome labels).

- [ ] **Step 1:** api.md: document the two new metric families + labels, the ring/log fields (`attemptOutcomes`, `lastUpstreamStatus`, `keyIds`, `keyTransitions`), the `lastUpstreamStatus` filter, the WARN `verdict` field, and `CreditsExhausted` (503, non-retryable) alongside the existing error table.
- [ ] **Step 2:** db AGENTS: disposition rule now says fail@3 ⇒ `'auth_fail'` (no more NULL ambiguity post-backfill); cooldown demotes, never filters; archive holds fingerprint-only rows. **Also:** `crates/serpotter-api/src/admin/keys.rs:35` enumerates only `vendor_suspended`/`manual`/absent — add `auth_fail` (stale by omission since B2).
- [ ] **Step 3:** root AGENTS schema bullet → v21. Grep for stale claims the campaign invalidates (e.g. "retryable:true on every vendor error", "NULL reason" wording). Gates: fmt + web gates if web docs touched.

---

## Verification (per batch, controller-run)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all && cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
# web batches:
cd web && npm run typecheck && npm run check && npm test && npm run build
```

**Definition of done (per class):** for each of 402/401/403/429 an operator can (a) count it: `serpotter_provider_attempt_total{outcome="payment_required|auth_invalid|forbidden|rate_limited"}`, (b) see it on the WARN line (`verdict=` field), (c) find affected requests: ring filter `lastUpstreamStatus` + `attemptOutcomes` CSV, (d) see what the pool did: `serpotter_key_transition_total` + `keyTransitions` field + keys-panel reason chip.

## Self-review

- [x] **Spec coverage:** audit gaps 1→T1+T3, 2→T2, 3→T6, 4→T4(+T8 backfill), 5→T9, 6→T5, 7→T7, 8→T10, 9→T11, 10→T7 `keyIds`. All ten mapped.
- [x] **Placeholder scan:** every step names files, anchors, or code; no TBDs.
- [x] **Type consistency:** `attempt_log`/`key_transitions` field names, `outcome_label`, `KeyPostState`→`KeyTransition`→`TransitionRecord` chain, `retry_after_secs` reused by T9 stamp and T11 verdict threading.
- [x] **Ownership:** single migration owner (T8); product→prometheus edge absent; cooldown is ORDER BY only.

## Oracle review (REVISE → 13 findings, all applied)

P0: 0021 schema_version stamp (T8) · archive COALESCE (T10) · B3 sequenced T6→T7 (batch map) · lib.rs re-exports (T1/T6). P1: research.rs xAI error binding (T1) · research WARN anchor/files (T2) · `spawn_scripted` harness (T3) · `LogFields` carrier named (T3) · pre/post-state transition detection (T6) · exhaustive `retry_after_secs` sweep (T9) · `Banned→AuthFailure` scoping on converted legs (T11). P2: `finish_exhausted` caller enumeration + cooldown never-cleared doc (T9/T8) · pin re-anchoring with per-pin disposition (T11) · `rate_limited` semantics note (label set). `ReportMode` re-export dropped: T11 plumbing decided as a new error variant instead.

