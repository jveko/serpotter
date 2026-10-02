# Daily Key Health Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) with dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** once per calendar day, every active API key gets exactly one real provider probe; definitive dead-key verdicts (401, ban phrases) remove or disable the key immediately, so production never retries accounts that are already gone.

**Architecture:** a new product free-fn `probe_due_keys` owns the pass (due-key query → per-key search call through the normal provider registry → existing `verdict_for` classification → existing lease-free `note_key_health_*` / archive report methods → `last_probe_at` stamp). `cron.rs` grows `spawn_key_probes`, an env-gated loop that runs the pass then sleeps until the next `KEY_PROBE_AT_HOUR`. Schema v22 adds `api_keys.last_probe_at`; the archive tombstone gains one allowlisted reason `probe_auth_401`.

**Tech Stack:** Rust 1.97.0 workspace (axum/tokio/sqlx/reqwest), SQLite via sqlx raw queries, `tracing`.

**Spec:** `docs/superpowers/specs/2026-10-02-daily-key-probe-design.md` (approved; the plan argues from it — executors read both).

## Global Constraints

- Every cargo/fmt/clippy/test command runs as `env -u RUSTUP_TOOLCHAIN <command>` (ambient shell toolchains must not win).
- No new dependencies; workspace deps only (`{ workspace = true }`).
- `EXPECTED_SCHEMA_VERSION` goes 21 → 22; migration file is `crates/serpotter-db/migrations/0022_api_keys_last_probe_at.sql` and MUST end with `UPDATE schema_version SET version = 22 WHERE id = 1;` (bump is manual in this repo).
- Env defaults, verbatim: `KEY_PROBE_CRON` off (enables on `1`/`true`/`yes`); `KEY_PROBE_AT_HOUR` = `4` (valid `0..=23`, UTC); `KEY_PROBE_STAGGER_MS` = `300`.
- Probe call constants, verbatim: `PROBE_QUERY = "health check"`, `max_results = 1`, no filters.
- Archive reason is a CLOSED set: `vendor_banned` | `probe_auth_401`, carried by an enum — never free text from an upstream body.
- xAI is never proxied (registry ignores proxy anyway; the probe must not lease a node for it). `REQUIRE_OUTBOUND_PROXY` truthy (`1`/`true`/`yes`) + no healthy node → abort the pass with no stamps.
- Probe-only behavior: NO changes to live search/extract/research ladders, NO admin API or SPA changes, NO `request_events`/`events::emit` entries.
- sqlx style: raw `query(...)` + `.bind(...)`; no `query!` macros; migrations are append-only (never edit 0001–0021).
- New/modified files end with a trailing newline — the write/edit tools may strip it; verify with `od -An -c -N1 -j $(($(wc -c < FILE)-1)) FILE` and use the sentinel trick (append `SENTINEL_EOF_NEWLINE` after a newline, then delete only the sentinel) if needed.
- Tests are hermetic: `127.0.0.1:0` scripted upstreams or `http://127.0.0.1:9`, never real network; assert `ProbeStats` fields and db state, never log text.
- CI gates (exact): `cargo fmt --all --check`, `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- Soft cap ~350 lines of production code per file (tests excluded) — that is why the pass logic lives in product `probe.rs`, not in `cron.rs`.

## Review Focus

Inputs/failure modes the spec implies that most deserve a reviewer's eye; each line is pinned by a test in the owning task:

1. **401 must beat the ban-phrase tier** — a Tavily 401 carrying deactivation copy must DELETE with reason `probe_auth_401`, not suspend. Pinned by `probe_401_with_deactivation_body_still_deletes_probe_reason` (Task 4).
2. **Same-day stamp is the idempotence guard** — a key stamped today is never re-probed by a second pass or a restart. Pinned by `due_skips_row_stamped_today` (Task 2) and `second_pass_same_day_probes_nothing` (Task 4).
3. **`REQUIRE_OUTBOUND_PROXY` abort must leave keys due and unstamped** — otherwise a proxied deployment marks every key probed while probing none. Pinned by `require_proxy_without_node_aborts_pass_unstamped` (Task 4).
4. **Bare 403 must never delete** — WAF/proxy middleware copy is not proof of death; it counts one fail only. Pinned by `probe_bare_403_counts_fail_and_keeps_row` (Task 4).
5. **Inactive and cooling keys are excluded from the due set** — probing them wastes vendor requests and can touch keys the operator/429 path already retired. Pinned by `due_skips_inactive_and_live_cooldown_keys` (Task 2).

---

### Task 1: Schema v22 — `api_keys.last_probe_at`

**Files:**
- Create: `crates/serpotter-db/migrations/0022_api_keys_last_probe_at.sql`
- Modify: `crates/serpotter-db/src/lib.rs:29` (`EXPECTED_SCHEMA_VERSION`)
- Modify: `crates/serpotter-db/src/keys/rows.rs` (`ApiKeyRow`)
- Modify: `crates/serpotter-db/src/keys/admin_crud.rs` (insert `RETURNING`, ~line 27)
- Modify: `crates/serpotter-db/src/keys/acquire_report.rs` (three `ApiKeyRow` construction sites: acquire `RETURNING` ~line 212, credit-sync list ~line 450, `get_api_key` ~line 465)
- Modify: `crates/serpotter-db/tests/migrate.rs:5,11,2256,2950`; `crates/serpotter-api/tests/admin_session.rs:23,118`
- Test: inline `#[cfg(test)]` additions in the modified db files where natural

**Interfaces:**
- Consumes: existing `ApiKeyRow` and its four SQL construction sites.
- Produces: `ApiKeyRow.last_probe_at: Option<String>` (all later tasks read this field on rows returned by `due_probe_keys`); `EXPECTED_SCHEMA_VERSION: i64 = 22`.

- [ ] **Step 1: Write the failing version test**

In `crates/serpotter-db/tests/migrate.rs`, rename `migrate_sets_schema_version_21` to `migrate_sets_schema_version_22` and change its pin:

```rust
assert_eq!(v, serpotter_db::EXPECTED_SCHEMA_VERSION);
assert_eq!(v, 22);
```

Also change the two legacy-boot asserts at lines ~2256 and ~2950 from `assert_eq!(db.schema_version().await.unwrap(), 21)` to `… , 22)`, and in `crates/serpotter-api/tests/admin_session.rs` change both `assert_eq!(v["schemaVersion"], 21)` to `22`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-db --locked`
Expected: FAIL — `assertion left == right` (left: 21, right: 22).

- [ ] **Step 3: Create the migration**

`crates/serpotter-db/migrations/0022_api_keys_last_probe_at.sql`:

```sql
-- Daily key health probe: when was this key last probed?
-- NULL = never probed = due on the first pass. TEXT, not DATETIME: the only
-- reader is `last_probe_at < date('now')`, which needs the date string's
-- TEXT affinity to compare losslessly (DATETIME is NUMERIC — see 0021).
ALTER TABLE api_keys ADD COLUMN last_probe_at TEXT;

-- Manual schema_version bump (house pattern; a missed bump 503s /ready).
UPDATE schema_version SET version = 22 WHERE id = 1;
```

- [ ] **Step 4: Bump the const and plumb the field**

In `crates/serpotter-db/src/lib.rs:29`: `pub const EXPECTED_SCHEMA_VERSION: i64 = 22;`
In `crates/serpotter-db/src/keys/rows.rs`, add to `ApiKeyRow`:

```rust
/// Last daily-probe stamp (`date('now')` string), NULL = never probed.
pub last_probe_at: Option<String>,
```

At each of the four `ApiKeyRow { … }` construction sites, add `last_probe_at: r.try_get("last_probe_at")?` AND add `last_probe_at` to that statement's explicit column list / `RETURNING` clause (all four list columns explicitly; the compiler will catch any site missed). For the `insert_api_key` `RETURNING`, add the column to the clause and `try_get` it — a fresh insert is `NULL`, which the mapper reads honestly.

- [ ] **Step 5: Run tests to verify they pass**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-db --locked && env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-api --locked admin_session`
Expected: PASS (schema 22 everywhere; no construction site missed — compile error would fire otherwise).

- [ ] **Step 6: Commit**

Run: `git add -A && git commit -m "add schema 22 last_probe_at column for daily key probes"`

---

### Task 2: Db due-query and stamp

**Files:**
- Create: `crates/serpotter-db/src/keys/probe.rs`
- Modify: `crates/serpotter-db/src/keys/mod.rs` (add `mod probe;`)
- Test: `#[cfg(test)]` in `crates/serpotter-db/src/keys/probe.rs`

**Interfaces:**
- Consumes: `ApiKeyRow` with `last_probe_at` (Task 1); `Db::insert_api_key`, `Db::set_api_key_active`, `Db::set_api_key_credits` (existing).
- Produces:
  - `pub async fn Db::due_probe_keys(&self) -> Result<Vec<ApiKeyRow>, DbError>` — rows eligible for today's probe.
  - `pub async fn Db::stamp_key_probe(&self, id: i64) -> Result<(), DbError>` — sets `last_probe_at = date('now')`.

- [ ] **Step 1: Write the failing due/stamp tests**

In `crates/serpotter-db/src/keys/probe.rs` `#[cfg(test)]` — helper `fn db()` = `connect_and_migrate("sqlite::memory:")`, helper `async fn due_ids(db) -> Vec<i64>` mapping `due_probe_keys()` to ids. Tests:

```rust
async fn due_null_stamp_is_due()          // fresh insert → in due set
async fn due_skips_row_stamped_today()    // stamp_key_probe then due_ids == []
async fn due_yesterday_stamp_is_due()      // SQL: UPDATE api_keys SET last_probe_at = date('now','-1 day') → due again
async fn due_inactive_never_due()          // set_api_key_active(false) → []
async fn due_skips_inactive_and_live_cooldown_keys()
// seed A (active, no stamp), B (active + UPDATE … SET cooldown_until = datetime('now','+1 hour')),
// C (active + cooldown_until = datetime('now','-1 hour'))
// → due_ids contains A and C, never B; after stamp, due_ids empty (stamp is per-row, not per-set)
async fn stamp_writes_server_date()        // stamp → last_probe_at == date('now') string, SELECT via raw query
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-db --locked probe`
Expected: FAIL — compile error, no `due_probe_keys` on `Db`.

- [ ] **Step 3: Implement `due_probe_keys` and `stamp_key_probe`**

In `crates/serpotter-db/src/keys/probe.rs`, one `impl Db` block, raw sqlx. The due predicate (exact — Task 5's Review Focus #3 and #5 depend on the cooldown clause being here, not in Rust):

```sql
SELECT id, service, key, active, consecutive_fails,
       COALESCE(key_fingerprint, '') AS key_fingerprint, last_probe_at
  FROM api_keys
 WHERE active = 1
   AND (cooldown_until IS NULL OR cooldown_until <= datetime('now'))
   AND (last_probe_at IS NULL OR last_probe_at < date('now'))
 ORDER BY id ASC
```

Map with the same field list as `ApiKeyRow`'s other construction sites. Stamp:

```sql
UPDATE api_keys SET last_probe_at = date('now') WHERE id = ?
```

Doc-comment both: the due query is the ONLY eligibility gate (once/day guarantee lives in SQL, not in the loop); the stamp is server-clock (`date('now')`), never the caller's wall clock.

- [ ] **Step 4: Run tests to verify they pass**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-db --locked`
Expected: PASS (all probe tests + existing suite).

- [ ] **Step 5: Commit**

Run: `git add -A && git commit -m "add due_probe_keys and stamp_key_probe for daily probing"`

---

### Task 3: Allowlisted archive reasons

**Files:**
- Modify: `crates/serpotter-db/src/keys/archive.rs`
- Modify: `crates/serpotter-db/src/keys/mod.rs` and `crates/serpotter-db/src/lib.rs` (re-export)
- Modify: `crates/serpotter-keypool/src/lib.rs:402` (`revoke_key_row` — its one caller)
- Test: `#[cfg(test)]` in `crates/serpotter-db/src/keys/archive.rs`

**Interfaces:**
- Consumes: existing `Db::archive_and_delete_api_key(id)` (currently hardcodes `'vendor_banned'`).
- Produces:
  - `pub enum serpotter_db::ApiKeyArchiveReason { VendorBanned, ProbeAuth401 }` with `pub fn as_db_str(self) -> &'static str` (`"vendor_banned"` / `"probe_auth_401"`).
  - `pub async fn Db::archive_and_delete_api_key(&self, id: i64, reason: ApiKeyArchiveReason) -> Result<bool, DbError>` (Task 4 passes `ProbeAuth401`).
  - `KeyPool::revoke_key_row(&self, id)` — signature unchanged, passes `VendorBanned`.

- [ ] **Step 1: Write the failing reason tests**

```rust
#[tokio::test]
async fn archive_stores_probe_auth_401_reason() {
    // insert key → archive_and_delete_api_key(id, ApiKeyArchiveReason::ProbeAuth401)
    // → SELECT reason FROM api_keys_archive WHERE api_key_id = ? == "probe_auth_401"
    // → row gone from api_keys (existing report_banned_deletes_key already pins VendorBanned)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-db --locked archive_stores_probe`
Expected: FAIL — compile error (no enum, no `reason` parameter).

- [ ] **Step 3: Implement the enum and thread the reason**

In `archive.rs`: define `ApiKeyArchiveReason` (`Clone, Copy, Debug, PartialEq, Eq`) with `as_db_str`. Change the signature to take it; in the `INSERT … SELECT`, replace the literal `'vendor_banned'` with a `.bind(reason.as_db_str())` on the statement. Update the doc: the vocabulary is closed by the enum (still never free text from an upstream body). Re-export from `keys/mod.rs` and add to the `pub use keys::{…}` list in `lib.rs`. In `serpotter-keypool/src/lib.rs`, `revoke_key_row` passes `ApiKeyArchiveReason::VendorBanned` (behavior identical — existing `report_banned_deletes_key` must still see `vendor_banned`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-db --locked && env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-keypool --locked`
Expected: PASS (new reason test green; `report_banned_deletes_key` still asserts `vendor_banned`).

- [ ] **Step 5: Commit**

Run: `git add -A && git commit -m "carry archive reason as a closed enum for probe deletes"`

---

### Task 4: Product probe pass (`probe_due_keys`)

**Files:**
- Create: `crates/serpotter-product/src/probe.rs`
- Modify: `crates/serpotter-product/src/lib.rs` (add `mod probe;` + `pub use probe::{probe_due_keys, ProbeStats, PROBE_QUERY};`)

**Interfaces:**
- Consumes: `Db::due_probe_keys` / `Db::stamp_key_probe` (Task 2); `Db::archive_and_delete_api_key(id, ApiKeyArchiveReason::ProbeAuth401)` (Task 3); `crate::lease::verdict_for`, `crate::hold::safe_node_error` (crate-internal, no new re-exports); `serpotter_providers::{ProviderRegistry, ProviderSearchParams, ProviderError, is_tunnel_error, SVC_XAI, SVC_FIRECRAWL}`; `ProxyPool::{acquire, release, report_success, report_failure, require_proxy}`; db `note_key_health_*` methods.
- Produces (Task 5 consumes):
  - `pub const PROBE_QUERY: &str = "health check";`
  - `#[derive(Debug, Default, PartialEq)] pub struct ProbeStats { pub probed: usize, pub ok: usize, pub deleted_401: usize, pub banned_deleted: usize, pub banned_suspended: usize, pub auth_fail: usize, pub rate_limited: usize, pub drained: usize, pub unchanged: usize, pub aborted: bool }`
  - `pub async fn probe_due_keys(db: &Db, providers: &ProviderRegistry, outbound: &ProxyPool, stagger: Duration) -> Result<ProbeStats, DbError>`

**Behavior contract (from the spec's disposition table — order is load-bearing):**

1. Fetch `due_probe_keys()`; empty → return default stats immediately (no network).
2. For each row, sequentially, sleeping `stagger` before every row except the first:
   - Lease a proxy unless `service == SVC_XAI`. `None` while `outbound.require_proxy()` → `tracing::warn!`, set `stats.aborted = true`, `break` (NO stamp — Review Focus #3). `None` otherwise → direct.
   - Call `providers.search(&row.service, params, proxy_url)` with `PROBE_QUERY`, `max_results: 1`, every other field `false`/`None` (mirror the literal shape at `search/run_provider.rs:162`).
   - Finish the proxy hold: `Ok` → `report_success`; `Err` + `is_tunnel_error` → `report_failure(lease, Some(&safe_node_error(…)))`; other `Err` → `release`.
   - Apply the key verdict: `Ok` → `note_key_health_success`, `stats.ok += 1`. `Err(Upstream { status: 401, .. })` → **checked first**, `archive_and_delete_api_key(id, ProbeAuth401)` + `tracing::warn!(key_id, provider, status, …)`, `stats.deleted_401 += 1`. Else `verdict_for(&row.service, e)`: `Banned` → firecrawl archives with `VendorBanned` (`banned_deleted`), others `note_key_health_suspended` (`banned_suspended`); `AuthFailure` (bare 403) → `note_key_health_failure` (`auth_fail`); `Exhausted` → `note_key_health_exhausted` (`rate_limited`, no cooldown stamp); `PaymentRequired` → `note_key_health_payment_required` (`drained`); `Retryable` | `Failure` → nothing (`unchanged`).
   - Any db action error → `tracing::warn!` and `continue` WITHOUT stamping (the row stays due for a restart); every successfully-applied outcome → `db.stamp_key_probe(row.id)`. Always `stats.probed += 1` for a row we called on.

- [ ] **Step 1: Write the failing probe tests**

`crates/serpotter-product/src/probe.rs` `#[cfg(test)]`. Local helpers (product cannot use the api crate's): `fn spawn_scripted(status: u16, body: &str) -> String` (TcpListener on `127.0.0.1:0`, one-connection thread writing `HTTP/1.1 {status}` + `Content-Length` + JSON body — same shape as `tests/common/mod.rs:44`), `async fn db()` = `connect_and_migrate("sqlite::memory:")`, `fn providers_tavily(url) -> ProviderRegistry` = `with_clients(TavilyClient::new(url), FirecrawlClient::new("http://127.0.0.1:9"), ExaClient::new(…:9), XaiClient::new(…:9))`, `async fn stamped(db, id) -> Option<String>` (raw SELECT `last_probe_at`). Use `ProxyPool::new(db.clone())` (no nodes → direct) unless stated. Tests:

```rust
async fn probe_200_stamps_resets_fails_and_counts_ok()
// seed tavily key, note_key_health_failure ×2, scripted 200 {"results":[…]}
// → consecutive_fails == 0, last_probe_at == date('now'), stats == {probed:1, ok:1, ..}

async fn probe_401_deletes_with_probe_reason()          // bare 401 body → row gone,
// api_keys_archive reason == "probe_auth_401", stats.deleted_401 == 1

async fn probe_401_with_deactivation_body_still_deletes_probe_reason()
// 401 + "The account associated with this API key has been deactivated."
// → row GONE (reason probe_auth_401), NOT suspended — Review Focus #1

async fn probe_firecrawl_403_ban_body_deletes_vendor_banned()
// firecrawl key, 403 + {"error":"ACCOUNT HAS BEEN BANNED by ops"}
// → row gone, reason == "vendor_banned", stats.banned_deleted == 1

async fn probe_tavily_403_deactivation_suspends()
// tavily key, 403 + deactivation copy → row KEPT, active == 0,
// disabled_reason == "vendor_suspended", stats.banned_suspended == 1

async fn probe_bare_403_counts_fail_and_keeps_row()     // Review Focus #4
// 403 + {"error":"Forbidden"} → consecutive_fails == 1, active == 1, no archive row

async fn probe_429_zeroes_credits_without_cooldown()
// set_api_key_credits(Some(50)), scripted 429 → credits == 0,
// cooldown_until IS NULL (raw SELECT), stats.rate_limited == 1

async fn probe_402_zeroes_credits()                     // → credits == 0, stats.drained == 1
async fn probe_503_changes_nothing_but_stamps()         // fails/credits untouched, stats.unchanged == 1, stamped
async fn second_pass_same_day_probes_nothing()          // run pass twice → 2nd: probed == 0
async fn inactive_and_stamped_keys_not_touched()        // due-filter respected end-to-end: stats.probed counts only due rows

async fn require_proxy_without_node_aborts_pass_unstamped()
// ProxyPool::with_options(db.clone(), true), no nodes → stats.aborted == true,
// stats.probed == 0, last_probe_at IS NULL, row still returned by due_probe_keys() — Review Focus #3
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-product --locked probe`
Expected: FAIL — compile error, no `probe` module.

- [ ] **Step 3: Implement `probe_due_keys` in `crates/serpotter-product/src/probe.rs`**

Module doc: this is the once-per-day health pass; eligibility lives in `due_probe_keys` SQL, verdicts come from the SAME `verdict_for` classifier the live ladders use, and the ONLY new rule is the order-1 `status == 401` pre-check (operator decision: a vendor 401 is definitive; WAF/proxy middleware emits 403/407). Skeleton:

```rust
pub const PROBE_QUERY: &str = "health check";

#[derive(Debug, Default, PartialEq)]
pub struct ProbeStats { /* exactly the ten fields from the Interfaces block */ }

pub async fn probe_due_keys(
    db: &Db,
    providers: &ProviderRegistry,
    outbound: &ProxyPool,
    stagger: Duration,
) -> Result<ProbeStats, DbError> {
    let due = db.due_probe_keys().await?;
    let mut stats = ProbeStats::default();
    for (i, row) in due.iter().enumerate() {
        if i > 0 { tokio::time::sleep(stagger).await; }
        // 1) lease (skip when SVC_XAI; abort+break when require_proxy && None)
        // 2) providers.search(&row.service, params, proxy_url)  — params literal mirrors
        //    search/run_provider.rs:162 with PROBE_QUERY / max_results: 1 / rest false|None
        // 3) finish proxy per Ok | is_tunnel_error → report_failure | else release
        // 4) match verdict: 401 pre-check FIRST, then verdict_for dispositions (behavior contract)
        //    action Err → warn + continue (no stamp); applied outcome → db.stamp_key_probe(row.id)
        //    destructive arms also tracing::warn!(key_id, provider, status, …)
        // 5) stats.probed += 1 for every row we called on
    }
    Ok(stats)
}
```

Destructive-arm log copy: `"daily probe removed key"` (delete) / `"daily probe suspended key"` (suspend), fields `key_id`, `provider`, `status`. Wire the module: `mod probe;` + `pub use probe::{probe_due_keys, ProbeStats, PROBE_QUERY};` in `lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-product --locked`
Expected: PASS — all probe tests + the existing product suite.

- [ ] **Step 5: Commit**

Run: `git add -A && git commit -m "add daily key probe pass in product"`

---

### Task 5: `spawn_key_probes` loop, env gate, main wiring

**Files:**
- Modify: `crates/serpotter-api/src/cron.rs` (new section after the alert loop; keep the file's production portion under ~350 lines — pass logic must NOT be duplicated here)
- Modify: `crates/serpotter-api/src/main.rs` (spawn ~line 109, abort ~line 188, `AppState` construction)
- Test: `crates/serpotter-api/src/cron.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `serpotter_product::{probe_due_keys, ProbeStats}` (Task 4); `cron::env_i64_or` (existing); `Arc<ProxyPool>` already built in `main.rs`.
- Produces:
  - `pub fn spawn_key_probes(db: Db, providers: ProviderRegistry, outbound: Arc<ProxyPool>) -> JoinHandle<()>` — returns an immediately-completing no-op task when the gate is off.
  - Pure helpers (testable, no env races): `fn probe_cron_enabled_from(raw: Option<&str>) -> bool`, `fn probe_at_hour_from(raw: i64) -> i64`, `fn secs_until_utc_hour(now_secs_of_day: u64, hour: i64) -> Duration`.

- [ ] **Step 1: Write the failing helper tests**

In `cron.rs` tests:

```rust
#[test] fn probe_gate_only_true_for_on_values() {
    // probe_cron_enabled_from(None | Some("") | Some("0") | Some("yes!")) == false
    // probe_cron_enabled_from(Some("1") | Some("true") | Some("YES")) == true
}
#[test] fn probe_at_hour_valid_passthrough_out_of_range_falls_back() {
    // probe_at_hour_from(0) == 0; from(4) == 4; from(23) == 23
    // probe_at_hour_from(-1) == 4; from(25) == 4
    //   (out-of-range falls back to the default 4; the warn-ONCE lives in
    //    validate_probe_at_hour, mirroring validate_reenable_hours' split)
}
#[test] fn secs_until_utc_hour_math() {
    // (3*3600, 4) == 3600 ; (5*3600, 4) == 23*3600 ; (4*3600, 4) == 86400 (full day, no zero-sleep)
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-api --locked cron::tests::probe`
Expected: FAIL — undefined functions.

- [ ] **Step 3: Implement the gate, hour math, and loop in `cron.rs`**

```rust
pub fn spawn_key_probes(db: Db, providers: ProviderRegistry, outbound: Arc<ProxyPool>) -> JoinHandle<()> {
    if !probe_cron_enabled_from(std::env::var("KEY_PROBE_CRON").ok().as_deref()) {
        return tokio::spawn(async {});           // default deploy behaves exactly as today
    }
    let hour = validate_probe_at_hour();          // warn-ONCE if KEY_PROBE_AT_HOUR outside 0..=23, else 4
    let stagger = Duration::from_millis(env_i64_or("KEY_PROBE_STAGGER_MS", 300).max(0) as u64);
    tokio::spawn(probe_loop(db, providers, outbound, hour, stagger))
}

async fn probe_loop(db: Db, providers: ProviderRegistry, outbound: Arc<ProxyPool>, hour: i64, stagger: Duration) {
    loop {
        // FIRST pass runs immediately (boot picks up due keys); then sleep to next hour.
        let started = std::time::Instant::now();
        match serpotter_product::probe_due_keys(&db, &providers, &outbound, stagger).await {
            Ok(stats) => tracing::info!(probed = stats.probed, ok = stats.ok,
                deleted_401 = stats.deleted_401, banned_deleted = stats.banned_deleted,
                banned_suspended = stats.banned_suspended, auth_fail = stats.auth_fail,
                rate_limited = stats.rate_limited, drained = stats.drained,
                unchanged = stats.unchanged, aborted = stats.aborted,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "daily key probe pass complete"),
            Err(e) => tracing::warn!(error = %e, "daily key probe pass failed"),
        }
        tokio::time::sleep(secs_until_utc_hour(now_secs_of_day(), hour)).await;
    }
}
```

`now_secs_of_day()` reads `SystemTime::now()` duration since UNIX epoch mod 86400 (UTC — matches SQLite `datetime('now')`); `secs_until_utc_hour(now, hour)` is the pure math tested in Step 1. REQUIRE-style gate parsing: `matches!(raw.map(str::to_ascii_lowercase).as_deref(), Some("1" | "true" | "yes"))`.

- [ ] **Step 4: Wire `main.rs`**

After the `AppState { … }` literal: bind it to `let state = …`, then
`let probes = serpotter_api::cron::spawn_key_probes(state.db.clone(), state.providers.clone(), state.outbound.clone());`
then `let router = app(state);`. In the shutdown block, next to the `alerts` abort pair:
`probes.abort(); let _ = probes.await;` (same pattern, comment: "daily probe pass — abort with the other loops").

- [ ] **Step 5: Run tests to verify they pass**

Run: `env -u RUSTUP_TOOLCHAIN cargo test -p serpotter-api --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

Run: `git add -A && git commit -m "spawn daily key probe loop behind KEY_PROBE_CRON"`

---

### Task 6: Docs and knowledge-base updates

**Files:**
- Modify: `docs/ops/env.md` (ops-knobs table ~line 72 + a short paragraph in the key-pool section)
- Modify: `AGENTS.md` (project root: `EXPECTED_SCHEMA_VERSION=21` occurrences → 22, "schema v21" → "schema v22", cron row/NOTES mention of the daily probe)
- Modify: `crates/serpotter-db/AGENTS.md` (line 7 "currently **21**" → **22**; the archive-reason vocabulary becomes `vendor_banned` | `probe_auth_401` — DROP the stale never-written `auth_fail` mention from that line rather than appending beside it; add a `last_probe_at` line to the columns/contracts table)

**Interfaces:**
- Consumes: all behavior from Tasks 1–5.
- Produces: operator-facing documentation of the three env vars and schema 22.

- [ ] **Step 1: Add the env rows (exact values)**

| var | default | meaning |
| --- | --- | --- |
| `KEY_PROBE_CRON` | off | set `1`/`true`/`yes` to enable the daily key health probe (one real search per active key per day; `401` → immediate archive+delete) |
| `KEY_PROBE_AT_HOUR` | `4` | UTC hour the daily pass targets; valid `0..=23`, out-of-range warns once at startup and falls back to `4` |
| `KEY_PROBE_STAGGER_MS` | `300` | delay between two probes inside one pass |

Plus one paragraph: keys already `active = 0` or inside a live `cooldown_until` are skipped; every probed row gets `last_probe_at` (schema 22) so a restart never double-probes the same day; between passes the worker makes no network calls.

- [ ] **Step 2: Verify docs render and grep clean**

Run: `grep -rn "EXPECTED_SCHEMA_VERSION\|schema v2" AGENTS.md crates/serpotter-db/AGENTS.md | head -20`
Expected: every mention says 22 (or a historical-schema note that deliberately references its own version, e.g. "schema 21 adds cooldown" stays as history; only CURRENT-version claims move to 22).

- [ ] **Step 3: Commit**

Run: `git add -A && git commit -m "document daily key probe knobs and schema 22"`

---

### Task 7: Full gates + live smoke

**Files:** none (verification only; any failure loops back to its owning task).

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces: evidence that the feature is done per the spec's acceptance criteria.

- [ ] **Step 1: Run the exact CI gates**

Run:
```
env -u RUSTUP_TOOLCHAIN cargo fmt --all --check
env -u RUSTUP_TOOLCHAIN cargo test --workspace --locked
env -u RUSTUP_TOOLCHAIN cargo clippy --workspace --all-targets --locked -- -D warnings
```
Expected: all PASS (fix failures in the owning task, re-run).

- [ ] **Step 2: Admin SPA gates (repo-wide CI parity)**

Run: `cd web && npm ci && npm run check && npm run build`
Expected: PASS (nothing in this plan touches `web/`; this proves it).

- [ ] **Step 3: Smoke — gate off behaves as today**

Run:
```
set -a; source .env; set +a
env -u RUSTUP_TOOLCHAIN KEY_PROBE_CRON= cargo run -p serpotter-api --locked
```
Expected: boots, logs NO `daily key probe pass complete` line; Ctrl+C shuts down cleanly. Paste the observed output.

- [ ] **Step 4: Smoke — one real probe pass**

Seed one real key and run with the gate on:
```
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"
env -u RUSTUP_TOOLCHAIN KEY_PROBE_CRON=1 cargo run -p serpotter-api --locked
```
Expected within a few seconds of boot: log line `daily key probe pass complete` with `probed=1` (and `ok=1` for a healthy key). Then stop the server and confirm the stamp:
```
sqlite3 data/serpotter.db "SELECT id, service, active, last_probe_at FROM api_keys"
```
Expected: `last_probe_at` is today's `YYYY-MM-DD`. Restart the server with the gate on → the pass logs `probed=0` (idempotence, Review Focus #2). Paste all three outputs.

- [ ] **Step 5: Final commit (if smoke forced any fix) and report**

Run: `git status --porcelain` (clean, or commit the fix with a conventional subject), then `git log --oneline -8`.
Report: gate outputs, smoke outputs, and confirm no unintended files in the diff.

---

## Self-Review

- [x] **1. Spec coverage:** every spec section maps to a task — method/scope/verdicts → Task 4; schedule (schema 22) → Tasks 1–2; architecture → Task 5; archive reasons → Task 3; env knobs + observability → Tasks 5–6; testing + acceptance criteria → tasks' test steps + Task 7. **Gap found and fixed:** the spec's due predicate/loop sketch omitted the `cooldown_until` clause that Decision 2 requires (cooling keys skipped) — spec amended inline (decision 3 + loop sketch) before this review.
- [x] **2. Step scan:** every step names one checkable action; code appears only where the signature/tests do not determine it (due SQL, disposition order, gate/hour contracts). No TBDs; no step says "handle edge cases".
- [x] **3. type consistency:** `ApiKeyArchiveReason::{VendorBanned, ProbeAuth401}` (Task 3) is the enum Tasks 4 consumes; `due_probe_keys`/`stamp_key_probe` names identical in Tasks 2/4/5; `ProbeStats` field list identical in Tasks 4 (definition) and 5 (log fields, + `elapsed_ms` from the spec's `elapsed`); `probe_due_keys(&Db, &ProviderRegistry, &ProxyPool, Duration)` — Task 5 passes `&Arc<ProxyPool>` which deref-coerces; `spawn_key_probes(Db, ProviderRegistry, Arc<ProxyPool>)` matches the `main.rs` wiring step.
- [x] **4. Review Focus:** all five lines have owning tests (RF1 → `probe_401_with_deactivation_body_still_deletes_probe_reason`; RF2 → `due_skips_row_stamped_today` + `second_pass_same_day_probes_nothing`; RF3 → `require_proxy_without_node_aborts_pass_unstamped`; RF4 → `probe_bare_403_counts_fail_and_keeps_row`; RF5 → `due_skips_inactive_and_live_cooldown_keys` + `inactive_and_stamped_keys_not_touched`). One deliberate non-test: a db-action failure mid-pass leaves the row unstamped — not CI-forceable without mocking `Db` (banned by house style); pinned by code structure (stamp strictly after the applied action) and documented in Task 4's contract.
- [x] **5. Proportion:** code blocks are contracts (SQL predicates, disposition order, log field names, test assertions), not transcripts; bodies are left to the implementer.
