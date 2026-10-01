# Daily key health probe — design

**Date:** 2026-10-02
**Status:** approved by operator (design stage); awaiting spec review before implementation planning

## Problem

Banned or dead vendor accounts are discovered reactively: live traffic acquires a
key, the call fails, and retries keep landing on keys that can never succeed.
Today a bare `401` never removes anything (see Baseline), so dead keys cycle
through fail@3 and the 24h re-enable cron indefinitely, costing production
retries the whole time.

**Goal:** once per calendar day, every active key is tested exactly once with a
real provider call; a definitive dead-key verdict removes it from rotation
immediately, without any high-frequency polling or long-running chatty loop.

## Baseline: what the current code does on 401

Documented here because the feature exists to change exactly this:

1. Bare `401`/`403` (no vendor ban wording) → `ReportMode::AuthFailure` →
   `consecutive_fails += 1`. At 3 the key flips `active = 0` with
   `disabled_reason = 'auth_fail'` — but the row stays in `api_keys`.
2. `'auth_fail'` is **cron-eligible**: the 15-minute maintenance cron's
   re-enable pass revives it after `KEY_REENABLE_AFTER_HOURS` (default 24).
   A permanently-401ing key therefore cycles disable → revive → 3 fails →
   disable forever, and every cycle spends production attempts.
3. The only hard delete today is `ReportMode::Banned`, which requires
   `401`/`403` **plus** a vendor account-state phrase (`is_account_banned`),
   and even then only firecrawl rows are deleted; Tavily/Exa/xAI ban matches
   take the suspend tier (`disabled_reason = 'vendor_suspended'`, row kept).

Net: an account the vendor has deactivated is never removed by the system
unless its response body happens to contain the exact ban wording and it is a
firecrawl key.

## Approved decisions

| # | Decision |
|---|---|
| 1 | **Method:** one minimal real product call per key (tiny search, `max_results: 1`) — not a billing/identity endpoint. Credit sync stays a billing read, never a health signal. |
| 2 | **Scope:** every **active** key, all four services, no sampling. Keys already `active = 0` (any `disabled_reason`) or inside a live cooldown are skipped — they are out of rotation; hitting them wastes requests. |
| 3 | **Schedule:** fixed daily pass keyed by a per-row `last_probe_at` (schema v22). A row is due while `active = 1 AND (last_probe_at IS NULL OR last_probe_at < date('now'))`. Restart-safe, idempotent: at most one probe per key per calendar day, by construction. |
| 4 | **Architecture:** standalone spawned worker in `cron.rs` (`spawn_key_probes`), alongside `spawn_maintenance` / `spawn_error_rate_alerts`. Between passes it sleeps until the next pass time — zero network traffic. No external cron/systemd unit; same single binary. |
| 5 | **Verdicts are destructive (operator accepted):** definitive dead-key verdicts remove/disable the key unattended, feeding the existing report machinery. |
| 6 | **`401` override (operator decision):** any upstream `401` on a probe → **immediate archive + delete**, no fail@3 accumulation, no 24h revival. Checked before the ban-phrase tier, for all four services. Rationale: `401` is the vendor's own credential rejection; WAF/proxy middleware emits `403`/`407`, not `401`. |
| 7 | **Enablement:** opt-in `KEY_PROBE_CRON=1` (default off), matching the `CREDIT_SYNC_CRON` convention — first deploy does not surprise the operator with ~1,000 probe requests. |
| 8 | **Cadence between probes:** sequential with a small stagger (~300ms, env-tunable), so a ~1,000-key pass spreads over minutes instead of bursting. No per-key retry inside the pass: one attempt, one verdict, one stamp. |

## Non-goals

- No admin API / SPA surface for probe results (not requested; the pass summary
  log line is the observability).
- No external scheduler; no second binary or CLI subcommand.
- No change to **live-path** dispositions — the 401 override is probe-only; the
  search/extract/research ladders keep fail@3 exactly as today.
- No integration with `request_events` (those entries are product/MCP requests).
- No per-vendor billing/identity endpoint probes; no credit writes (credit sync
  remains the separate, optional `CREDIT_SYNC_CRON` pass).

## Design

### Worker loop

```
spawn_key_probes(db, providers, outbound) -> JoinHandle
  gated on KEY_PROBE_CRON=1 (else return an already-finished/no-op task)
  loop {
      t = now
      due = api_keys where active = 1
            and (last_probe_at is null or last_probe_at < date('now'))
      for row in due (sequential):
          probe_one(row)         // verdict applied, last_probe_at stamped
          sleep(KEY_PROBE_STAGGER_MS default 300)
      sleep until next KEY_PROBE_AT_HOUR (default 04:00, validated once at startup)
  }
```

- Boot safety: keys never probed (`NULL`) or from a previous date are due
  immediately, so a restart or a missed pass self-heals on the next tick —
  still never more than once per key per day.
- Shutdown: the handle is aborted after `serve` returns, same as the other two
  loops; `stop_grace_period: 30s` already covers a pass in flight.

### One probe

1. Lease a proxy node from `ProxyPool` (least-inflight, per-probe) — mirroring
   the live attempt shape; **xAI is always direct**. With
   `REQUIRE_OUTBOUND_PROXY=1` and no healthy node: abort the pass with a WARN,
   stamp nothing, keys stay due.
2. Call
   `providers.search(service, ProviderSearchParams { query: PROBE_QUERY,
   max_results: 1, api_key: &row.key, .. }, proxy_url)`
   with `PROBE_QUERY = "health check"` (constant). No `KeyPool` acquire — the
   probe selects its own key; the report side uses the lease-free
   `note_key_health_*` / `Db` methods.
3. Transport/tunnel errors: same split as live — a tunnel error blames the
   leased node (`report_node_failure`-equivalent note), any other transport
   error is `Retryable` for the key (no state change).
4. Finish the proxy hold (`release`, or node-failure on tunnel errors).
5. Apply the verdict table, then stamp `last_probe_at = date('now')` on
   **every** outcome (the once-per-day guarantee — even a failed probe or an
   ambiguous 5xx must not re-fire the same day).

### Verdict → action (probe disposition table)

Classification order is significant:

| order | condition (probe result) | action |
|---|---|---|
| 1 | upstream status **401** (any body) | **immediate archive + delete** via the ban revoke path, tombstone reason `probe_auth_401` — probe-only override (decision 6) |
| 2 | `verdict_for` → `Banned` (403 + account-state phrase) | live two-tier, verbatim: firecrawl → `revoke_key_row` (tombstone `vendor_banned` + delete); Tavily/Exa/xAI → `note_key_health_suspended` (`vendor_suspended`, permanent; re-enable cron already skips it) |
| 3 | `verdict_for` → `AuthFailure` (bare 403, no phrase) | `note_key_health_failure` — counts toward fail@3, live parity (proxy middleware emits bare 403s; one is not proof of death) |
| 4 | `verdict_for` → `Exhausted` (429/432/433 per provider) | `note_key_health_exhausted` — zeroes non-NULL credits, **no** `cooldown_until` stamp (no observed `Retry-After` on the probe) |
| 5 | `verdict_for` → `PaymentRequired` (402) | `note_key_health_payment_required` — zeroes credits, row stays active but demoted by the exhausted-last tier |
| 6 | `verdict_for` → `Retryable` (5xx, transport) or `Failure` (misc statuses) | **no state change**, `warn`/`info` log only |
| 7 | call succeeded | `note_key_health_success` — resets `consecutive_fails`, honest 1-credit soft-burn (the probe really did spend a search credit) |
| always | every row touched | `last_probe_at = date('now')` |

`verdict_for` (the shared classifier in `serpotter-product`) is reused, not
re-implemented; only order-1 is new probe logic, expressed as an explicit
`status == 401` pre-check on the `ProviderError::Upstream` before delegating.

### Schema (v22)

- Migration `0022_api_keys_last_probe_at.sql`: `ALTER TABLE api_keys ADD COLUMN
  last_probe_at TEXT` (no default → `NULL` = never probed = due).
- `EXPECTED_SCHEMA_VERSION = 22`; `ApiKeyRow` gains
  `last_probe_at: Option<String>`.
- No other schema objects. `/ready` gate moves with the constant.

### Environment knobs

| var | default | meaning |
|---|---|---|
| `KEY_PROBE_CRON` | off (`1` enables) | master switch for the worker |
| `KEY_PROBE_AT_HOUR` | `4` | hour-of-day (UTC, SQLite `datetime('now')` basis) the daily pass targets; validated once at startup with the same warn-once pattern as `validate_reenable_hours` |
| `KEY_PROBE_STAGGER_MS` | `300` | delay between two probes in one pass |

### Observability

- One `tracing::info!` per pass:
  `probed, ok, deleted_401, banned_deleted, banned_suspended, auth_fail,
  rate_limited, drained, unchanged, elapsed`.
- `tracing::warn!` on every destructive verdict with `key_id`, `service`,
  `status`.
- No metrics, no `events::emit`, no request-log entries (probe is not a
  product/MCP request).

## Testing

Follows existing patterns (`mock_upstream(status)` spin-up servers,
`spawn_*_with_period` explicit-cadence constructors, in-memory `sqlite::memory:`
with `max_connections=1`):

1. **DB:** due-query semantics — `NULL` due, same-date stamped skipped,
   yesterday stamped due, `active = 0` never selected; migration v22 bumps
   `schema_version`; `ApiKeyRow` round-trips the new column.
2. **Worker, driven per-probe against mock upstreams:**
   - `200` → `note_key_health_success` effects (fails reset) + stamp.
   - `401` (bare body) → row hard-deleted, tombstone reason `probe_auth_401`,
     regardless of service.
   - `403` + firecrawl ban body → row deleted, tombstone `vendor_banned`.
   - `403` + Tavily deactivation body → row kept, `vendor_suspended`.
   - bare `403` → `consecutive_fails` +1, row active.
   - `429` → non-NULL credits zeroed, no `cooldown_until`.
   - `402` → credits zeroed, row active.
   - `503` → no state change at all (beyond the stamp).
3. **Idempotence:** run a pass twice within the same date → second pass probes
   zero rows (probe counter unchanged).
4. **Gate:** `KEY_PROBE_CRON` unset → no probes issued.
5. **Stagger/period:** explicit-period constructor keeps unit tests fast; the
   stagger itself is env-defaulted and not asserted beyond the sleep helper.
6. Full CI gates: `cargo fmt --all --check`,
   `cargo test --workspace --locked`, `cargo clippy --workspace
   --all-targets --locked -- -D warnings`, all with
   `env -u RUSTUP_TOOLCHAIN`.

## Acceptance criteria

- With `KEY_PROBE_CRON=1`, every active key receives exactly one probe per
  calendar day; restarts and double ticks never double-probe a key (stamp is
  the guard).
- A key whose probe answers `401` is gone from `api_keys` that same pass, with
  an `api_keys_archive` tombstone recording the probe reason — no fail@3 wait,
  no 24h revival.
- Ban-phrase and 429/402 verdicts land through the existing report methods
  with the same semantics as the live path.
- Ambiguous outcomes (5xx/transport) change nothing except the stamp.
- Between passes the worker makes no network calls; within a pass, calls are
  sequential with the configured stagger.
- Default deploy (`KEY_PROBE_CRON` unset) behaves exactly as today.
