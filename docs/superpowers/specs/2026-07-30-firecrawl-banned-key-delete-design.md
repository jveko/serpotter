# Firecrawl Banned-Key Auto-Delete Design

**Date:** 2026-07-30  
**Status:** Approved for implementation planning  
**Scope:** On-path detection of Firecrawl banned API keys and hard DELETE from `api_keys` (search + extract attempt loops only)

## Problem

Prod Serpotter holds a large Firecrawl key pool. Many keys return permanent ban responses:

```json
{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}
```

(HTTP **403**; same class expected for some **401**s.)

Today product maps Firecrawl `401`/`403` to `finish_failure` → `consecutive_fails++` → `active=0` at fail@3. Inactive keys can return via `reenable_stale_keys` (15m cron, `KEY_REENABLE_AFTER_HOURS`). Banned inventory therefore:

1. Burns three acquires before disable  
2. Stays in the table  
3. Can be re-activated and fail again  

Operator pain: thrash, noisy 502s, and dead weight in `api_keys` (thousands of rows after bulk seed).

## Goals

1. Detect Firecrawl **ban body** on search/extract upstream errors.  
2. **Hard DELETE** that key row on **first** confirmed ban match.  
3. Continue the existing attempt loop with another key (or exhaust cleanly).  
4. Never log full API key material.  
5. Leave non-ban `401`/`403` on the existing fail@3 path.

## Non-goals

- Tavily / Exa / xAI ban or revoke handling  
- Credit-sync or admin bulk purge of banned keys  
- New schema columns, migrations, or admin UI  
- `ProviderError::Banned` type peel  
- Changing fail@3 threshold or re-enable cron for non-ban failures  
- Backfill DELETE of already-inactive keys that never get acquired again  

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Provider | **Firecrawl only** |
| Signal | **Body match only**: status ∈ {401, 403} **and** ban markers in body |
| Action | **Hard DELETE** `api_keys` row |
| Timing | **Immediate** on first ban match (no fail@3 wait) |
| Approach | **A — on-path** in product attempt loops |
| Credit-sync sweep | **Not v1** |
| Schema | **None** |

## Ban detection

Pure function in product (alongside `is_exhausted_status`), e.g.:

```text
is_firecrawl_banned(status: u16, body: &str) -> bool
```

**True** when all hold:

1. `status == 401 || status == 403`  
2. Body matches (case-insensitive) at least one marker derived from the live fixture:

| Marker | Rationale |
| --- | --- |
| `account has been banned` | Exact phrase from Firecrawl error string |
| `has been banned` | Slightly shorter stable core (still specific) |

**Canonical fixture** (captured 2026-07-30 against a known-dead key via `GET /v1/team/credit-usage` and consistent with product search/extract upstream bodies):

```json
{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}
```

**False** for:

- `402` / `429` (remain `is_exhausted_status` → `report_exhausted`)  
- `5xx`, timeouts, tunnel/proxy errors  
- `401`/`403` **without** ban markers (generic auth / WAF → existing `finish_failure` / fail@3)  
- Other providers (caller must only invoke for `firecrawl`)

Do **not** treat bare `"Unauthorized"` alone as ban — too broad.

## Data flow

### Search (`run_provider`)

On `ProviderError::Upstream { status, body, .. }` for `provider == "firecrawl"`:

1. If `is_exhausted_status` → unchanged (`finish_exhausted`, continue).  
2. **New:** else if `is_firecrawl_banned(status, body)` →  
   - `key_hold.finish_banned().await`  
   - proxy hold: **release only** (ban is key-class, not node-class)  
   - set `last_err` with status/body summary (no key)  
   - **`continue`** attempt loop  
3. Else existing `401`/`403` / `429` / `5xx` / other branches unchanged.

### Extract (`extract_url` / `try_extract_provider`)

Same ban branch ordering relative to exhausted and generic `401`/`403` failure reporting.

### Attempt budget

Ban delete consumes one attempt slot like today’s failure path (max 3 attempts per provider leg). Deleting the key ensures the next acquire cannot return the same id.

## Keypool / DB

| Layer | Change |
| --- | --- |
| `Db` | Reuse `delete_api_key(id)` (already used by admin). No new SQL required for v1. |
| `KeyPool` | Add `report_banned(id)` → `delete_api_key(id)` + `notify_waiters()`. |
| `KeyHold` | Add `finish_banned()` → `report_banned`, disarm drop guard (same pattern as `finish_failure` / `finish_exhausted`). |

**Multi-hold:** DELETE removes the row. Subsequent `report_*` / `release` on the same id against missing rows must remain safe no-ops (current `UPDATE`/`DELETE … WHERE id = ?` behavior). Document that concurrent holds on a banned key end with “row gone”; drop guards must not panic if report/delete already ran.

**Inflight:** Prefer delete-only (row gone ⇒ inflight irrelevant). Do not fail++ then delete.

## Cron / re-enable

Deleted keys cannot be selected by `reenable_stale_keys`. No cron code change in v1. Pre-existing `active=0` ban victims remain until touched by traffic that still returns ban body (then delete) or manual admin delete — **no backfill job in v1**.

## Observability

- `tracing` warn/info once per ban delete: service, key **id**, status, short body snippet or static reason `firecrawl_banned` — **never** full key.  
- Client-visible error shape unchanged if the whole attempt loop fails (still provider / NoHealthyKey problem+json). Ban handling is a pool side-effect.  
- No new admin endpoint or stats counter required for v1 (optional follow-up).

## Tests

1. **Unit — `is_firecrawl_banned`**  
   - Fixture JSON body + 403 → true  
   - Same body + 401 → true  
   - Case variation → true  
   - 403 with unrelated body → false  
   - 402 / 429 / 500 → false  

2. **Keypool / db**  
   - Insert firecrawl key → `report_banned(id)` → `get_api_key` none / acquire skips it  
   - `report_banned` on unknown id does not error hard (or maps cleanly — match existing delete semantics)

3. **Product path** (match existing harness style; providers at `:9` or injected error)  
   - Prefer focused unit/integration around hold + classification if full HTTP loop is heavy  
   - At minimum: classification + pool delete covered; wire path covered if cheap

## Files (expected touch list)

- `crates/serpotter-product/src/search/exhausted.rs` or sibling `banned.rs` + re-export  
- `crates/serpotter-product/src/search/run_provider.rs`  
- `crates/serpotter-product/src/extract/extract_url.rs`  
- `crates/serpotter-product/src/hold.rs`  
- `crates/serpotter-keypool/src/lib.rs` (+ tests)  
- Docs only if ops mention key lifecycle (`docs/ops/env.md` one-liner optional)

No intentional REST/MCP wire change.

## Rollout

1. Implement + workspace test/clippy.  
2. Deploy image to prod.  
3. Natural purge: banned keys disappear as search/extract hit them.  
4. Optional later: credit-sync ban sweep, admin “delete inactive”, other providers.

## Open follow-ups (explicitly out of v1)

- Bulk inactive purge / credit-usage ban sweep for idle banned rows  
- Persist ban reason before delete (audit table)  
- Extend markers if Firecrawl changes copy  
- Tavily revoke / Exa invalid key parallels  

## Implementation next step

After written-spec approval: **writing-plans** → `docs/superpowers/plans/2026-07-30-firecrawl-banned-key-delete.md`, then implement.
