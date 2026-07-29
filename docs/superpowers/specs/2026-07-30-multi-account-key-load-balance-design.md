# Multi-Account Key Load-Balance Design

**Date:** 2026-07-30  
**Status:** Approved for implementation planning  
**Scope:** Credit-proportional, concurrency-damped multi-key pick + soft success burn in `api_keys` / KeyPool (keys only; ProxyPool unchanged)

## Problem

Serpotter’s per-service key pool already has shared soft `max_inflight`, wait/notify, hold reclaim, and a binary credit tier:

```sql
ORDER BY
  CASE WHEN credits_remaining IS NULL OR credits_remaining > 0 THEN 1 ELSE 2 END,
  inflight ASC,
  last_used_at IS NOT NULL, last_used_at ASC, id ASC
```

That is **not** continuous credit-proportional drain-even:

1. **Uneven burn** — under light load, one healthy free key can absorb most sequential acquires while richer siblings wait for the next free slot only via LRU, not residual credits.
2. **Quota thrash** — near-empty keys compete equally with fat keys inside the “healthy” tier until they hard-exhaust (402/429/432/433 paths).
3. **Concurrency pile-up** — pure credit-primary without load damping would stampede the richest key until `max_inflight`; pure least-inflight ignores residual budget.
4. **No soft local burn** between rare credit syncs — ranking only moves on sync/exhausted, not on successful use.

Upstream LLM gateways (LiteLLM residual RPM/TPM, Portkey weighted lottery, Helicone P2C+PeakEWMA, Envoy weighted least-request) do **not** ship true vendor-credit multi-key LB. Credit-proportional drain-even is **local accounting** we own. Provider honesty:

| Provider | Live residual on product key? | Soft −1 unit? |
| --- | --- | --- |
| Tavily | Yes `GET /usage` (≤10 calls / 10 min) | ~OK for basic search (1 credit); advanced/research ≠ 1 |
| Firecrawl | Yes **team** `remainingCredits` | Rank heuristic only (costs vary); multi-key same team double-counts residual |
| Exa | No | Wrong unit |
| xAI | Management API only (not inference keys) | Wrong unit (USD ticks) |

## Goals

1. **Credit-proportional drain-even** among known-credit keys (prefer higher remaining so the pool empties more evenly in credit space).
2. **Concurrency-aware** pick: Envoy-style `effective_C / (inflight + 1)` so holds spread before the hard cap.
3. **Soft burn on success**: decrement non-NULL `credits_remaining` by 1 (floor 0); tavily/firecrawl sync remains source of truth when it runs.
4. **NULL mid-weight**: unknown residual (Exa/xAI/unsynced) uses a sentinel, not 0 and not ∞.
5. **Exhausted last**: `credits_remaining = 0` stays eligible as fallback (today parity), demoted tier.
6. Keep twin-pool shape: **keys only**; ProxyPool least-inflight unchanged; wait/notify + shared `max_inflight` + lease reclaim unchanged.

## Non-goals (v1)

- Outbound node LB changes
- Stride / `pass` columns for exact long-run request∝credits (phase-2 if metrics show residual unfairness after A)
- Weighted random / P2C lottery
- Inventing Exa/xAI residual or wiring Management/Admin usage APIs
- Firecrawl team-dedup of residual across keys sharing one team
- Per-request cost models (Tavily advanced=2, Firecrawl multi-credit ops)
- Multi-process lease fencing / Redis shared state
- REST / MCP / product wire changes
- New admin SPA surfaces beyond existing credits/inflight columns
- Schema migration / `EXPECTED_SCHEMA_VERSION` bump

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Scope | **API keys only** (per `service` pool) |
| Success metric | **Credit-proportional drain-even** |
| Approach | **A now** — Envoy-damped continuous weight + soft −1 on success; **B (stride) documented follow-up only** |
| Pick shape | Deterministic SQL `ORDER BY` (no RNG) |
| Soft burn | **−1 on `report_api_key_success` only** when `credits_remaining IS NOT NULL`; never invent for NULL; floor 0 |
| NULL policy | Mid sentinel via `KEY_UNKNOWN_CREDIT_WEIGHT` (default **100**) |
| Exhausted | `credits_remaining = 0` last tier, still acquirable |
| Sync | Unchanged honesty: tavily/firecrawl real; exa/xai soft-error; overwrite remaining/limit |
| Schema | **None** — reuse `credits_remaining` |
| Integer scale | Code const **SCALE = 1000** (not env) |

## Architecture

```text
acquire(service)
  → reclaim expired holds
  → SELECT eligible row by score ORDER BY (below)
  → optimistic inflight++ + lease stamp (CAS)
  → provider call
  → success: soft burn −1 (if non-NULL) + inflight−−
  → failure: inflight−−, fails++ (no burn)
  → exhausted: credits=0 + inflight−− (no −1 needed)
  → release: inflight−− only (no burn, no fails++)
  → optional admin/cron sync: overwrite remaining/limit (SoT)
```

**Change surface**

| Layer | Change |
| --- | --- |
| `Db::acquire_api_key_shared` | New `ORDER BY` + bind unknown weight / scale |
| `Db::report_api_key_success` | Soft burn CASE on `credits_remaining` |
| `KeyPool` | Read `KEY_UNKNOWN_CREDIT_WEIGHT`; pass into acquire (`from_env` / `with_config`) |
| Tests | db + keypool multi-key score, NULL mid, burn, regression |
| Ops docs | `KEY_UNKNOWN_CREDIT_WEIGHT`; honesty notes (Tavily usage RPM, Firecrawl team residual, soft −1 heuristic) |

**Unchanged:** KeyPool wait/notify loop, `NoHealthyKey` vs `AcquireTimeout`, release/fail/exhausted contracts, product dual-pool matrix, ProxyPool, credit_sync provider allowlist, wire paths.

## Pick formula

### Eligibility (unchanged)

```text
service = ? AND active = 1 AND inflight < max_inflight
```

after reclaim of expired `lease_until` holds.

### effective_C

| `credits_remaining` | Role in score |
| --- | --- |
| `NULL` | `KEY_UNKNOWN_CREDIT_WEIGHT` (default 100, min 1) |
| `> 0` | that value |
| `= 0` | **not scored** — exhausted tier only |

### ORDER BY (deterministic, integer SQLite)

```sql
ORDER BY
  CASE WHEN credits_remaining = 0 THEN 1 ELSE 0 END,  -- exhausted last; NULL stays tier 0
  (CASE
     WHEN credits_remaining IS NULL THEN :unknown_weight
     ELSE credits_remaining
   END * :scale) / (inflight + 1) DESC,
  last_used_at IS NOT NULL, last_used_at ASC,
  id ASC
LIMIT 1
```

- `:scale` = **1000** (const) so integer division keeps discrimination at low C.
- `:unknown_weight` from env / `KeyPool` config (default 100).
- Optimistic bump remains `UPDATE … WHERE id = ? AND active = 1 AND inflight < ?` (CAS); retry/None if race.

### Why not pure `C DESC`

Under free capacity every waiter would pick the same richest row until cap → provider 429 while other keys idle. `(inflight + 1)` damping matches Envoy unequal-weight least-request with `bias = 1`. Single-process mutex already serializes pick+bump, so P2C is unnecessary for v1.

### Why not pure least-inflight + credit tie-break

That is today’s spirit with continuous credit only as tie-break: under light load the richest free key still wins **every** free slot, so drain-even stays weak. Continuous C in the numerator is required for the locked success metric.

## Soft burn

On **`report_api_key_success` only**:

```sql
credits_remaining = CASE
  WHEN credits_remaining IS NULL THEN NULL
  WHEN credits_remaining <= 0 THEN 0
  ELSE credits_remaining - 1
END
```

plus existing multi-hold-safe `inflight−−`, clear `lease_until` when last hold ends, and `last_used_at` touch (match current success SQL shape).

| Path | Burn? |
| --- | --- |
| `report_success` | Yes (−1 if non-NULL) |
| `report_failure` | No |
| `release` | No |
| `report_exhausted` | Forces `0` (unchanged) |
| credit sync `update_api_key_usage` | Overwrites absolute remaining/limit (SoT) |

**Honesty:** soft −1 is a **rank heuristic**, not billing truth. Tavily basic search ≈ 1 credit; advanced/research and Firecrawl multi-credit ops differ. Sync corrects drift when available.

## Config

| Knob | Default | Notes |
| --- | --- | --- |
| `KEY_UNKNOWN_CREDIT_WEIGHT` | `100` | effective_C when `credits_remaining IS NULL`; clamp ≥ 1 |
| `KEY_MAX_INFLIGHT` | `3` | unchanged |
| `KEY_ACQUIRE_TIMEOUT_SECS` | `30` | unchanged |
| `KEY_HOLD_TTL_SECS` | `90` | unchanged |
| scale | `1000` (code const) | not env |

Document in `docs/ops/env.md` and keypool/db AGENTS one-liners.

## Error / pool contracts (unchanged)

- Empty / inactive inventory → fail-fast `NoHealthyKey` (no full timeout wait)
- Active inventory all at cap through deadline → `AcquireTimeout`
- Exhausted-only inventory still acquirable (tier 1)
- `release` must not increment `consecutive_fails` and must not burn credits
- Failure: inflight−−, `consecutive_fails++`, disable at max fails — no soft burn
- Exhausted: `credits_remaining = 0`, keep `active = 1`, inflight−−
- Every report/release still `notify_waiters()`

## Ops caveats (document, not extra code in v1)

1. **Tavily `/usage`:** 10 requests / 10 minutes — multi-key aggressive sync can 429 the usage API itself; keep cron/admin cadence conservative.
2. **Firecrawl residual is team-wide:** N keys on the same team each storing full `remainingCredits` overstates pool capacity for proportional drain. Accept in v1; optional later honesty pass.
3. **Exa / xAI:** NULL residual is correct; mid-weight + load damping only; 402 (Exa) / depleted-reject (xAI) and existing exhausted/fail paths remain ground truth.
4. Soft burn drift is expected until sync or exhausted.

## Phase-2 (explicit non-implement)

**Stride / virtual pass:** tickets ∝ effective_C, pick `min(pass)`, `pass += stride` after grant. Use only if post-A metrics show residual **request-count** unfairness after C-normalized drain. Needs migration + join init policy for new/synced keys — YAGNI for v1.

## Tests

Must defend observable contracts:

1. Two keys `C=100` vs `C=10`, both `inflight=0` → acquire prefers richer.
2. Load damping: rich near cap vs poor free → score can prefer poor (prove `(inflight+1)` term).
3. `NULL` vs `C=0` → NULL before exhausted; NULL uses mid weight vs high known C.
4. `report_success` decrements non-NULL by 1; NULL stays NULL; never negative.
5. Sync overwrite still sets absolute remaining after burns.
6. Regressions: empty fail-fast; acquire timeout; lost-wakeup release; exhausted keeps active; positive credits still beat pure LRU favoring exhausted.

## File map

| File | Role |
| --- | --- |
| `crates/serpotter-db/src/keys/acquire_report.rs` | ORDER BY + success soft burn |
| `crates/serpotter-keypool/src/lib.rs` | env/`with_config` unknown weight → acquire |
| `crates/serpotter-keypool/src/tests.rs` | pool-level multi-key + burn tests |
| `crates/serpotter-db/tests/…` | SQL-level preference + burn tests (extend existing) |
| `docs/ops/env.md` | `KEY_UNKNOWN_CREDIT_WEIGHT` + honesty notes |
| `crates/serpotter-keypool/AGENTS.md` | pick policy one-liner |
| `crates/serpotter-db/AGENTS.md` | acquire ORDER BY one-liner |

No intentional REST/MCP wire change. No migration.

## Risks

| Risk | Mitigation |
| --- | --- |
| Soft −1 drifts from billing | Sync SoT; document heuristic; exhausted forces 0 |
| Firecrawl team double-count | Non-goal v1; ops note |
| Huge unknown weight monopolizes | Default 100; env tunable; clamp ≥ 1 |
| Integer division flattens low C | SCALE=1000 |
| Richest-key stampede | `(inflight+1)` damp + existing mutex + CAS bump |
| Test expectation drift on old “credit bucket then LRU” cases | Update assertions to continuous score |

## Acceptance

1. Multi-key free capacity prefers higher remaining credits with inflight damping.
2. Soft burn on success only; sync SoT; exhausted → 0 unchanged.
3. NULL mid-weight; `C=0` last tier still eligible.
4. KeyPool wait/notify + shared-cap behavior remains green.
5. No wire change; no schema version bump; `cargo test`/`clippy` green for touched crates (workspace as needed).

## Research anchors (non-normative)

- Envoy weighted least-request: `weight / (active_requests + 1)^bias`
- LiteLLM: residual RPM/TPM soft local burn on success; cooldown on 429 — analogue for soft burn, not vendor credits
- Portkey: static weighted lottery — rejected (non-deterministic, no residual)
- Provider docs 2026-07-30: Tavily usage/credits/rate limits; Firecrawl team credit-usage + 402/429; Exa 402 tags / no residual; xAI management prepaid + cost ticks

## Implementation next step

After written-spec approval: **writing-plans** → `docs/superpowers/plans/2026-07-30-multi-account-key-load-balance.md`, then implement.
