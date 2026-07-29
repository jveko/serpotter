# serpotter-keypool

**Updated:** 2026-07-30 · shared-cap acquire + wait/notify + credit-damped pick

## OVERVIEW

In-process key pool over `api_keys` with a **shared soft cap** (`max_inflight` per key). Concurrent holds on the same key are allowed until the cap; waiters park on `Notify` only when active inventory exists but all keys are at cap. Empty/inactive inventory fails fast. Durable holds live in SQLite (`inflight` + `lease_until` hold deadline). **Single-process only** — mutex + Notify are not multi-instance safe.

## STRUCTURE

```
src/
├── lib.rs     # KeyPool, LeasedKey
└── tests.rs   # #[cfg(test)] sibling (notify/race/TTL)
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| Shared-cap acquire + wait | `KeyPool::acquire` (`acquire_api_key_shared`) |
| Release hold (no fail++) | `KeyPool::release` → `Db::release_api_key_inflight` + notify |
| Report outcome | `report_success` / `report_failure` / `report_exhausted` + notify; success soft-burns non-NULL credits via db |
| Env limits | `KEY_MAX_INFLIGHT=3`, `KEY_ACQUIRE_TIMEOUT_SECS=30`, `KEY_HOLD_TTL_SECS=90`, `KEY_UNKNOWN_CREDIT_WEIGHT=100` |
| Hold reclaim SQL | `serpotter-db` (`KEY_HOLD_TTL_SECS`, reclaim on shared acquire path) |

## CONVENTIONS

- Hold `Mutex<()>` **only** around reclaim+pick+bump; **never** across `Notify` wait.
- Pin `notified()` and `enable()` **before** the acquire mutex (reporters do not hold it), then recheck under lock and await outside — covers free+notify before enable via recheck and after enable via ready future.
- After wait **timeout**, run one final critical-section acquire attempt (notify/reclaim race).
- Every `report_*` and `release` must `notify_waiters()`.
- `release` must not increment `consecutive_fails` (tunnel / cancel paths).
- Map `ApiKeyRow` → `LeasedKey { id, service, key }`.
- Empty healthy set → `KeyPoolError::NoHealthyKey` (fail-fast, no full timeout).
- Active inventory all at cap through deadline → `KeyPoolError::AcquireTimeout` (product maps to `KeyBusy` 503).
- Prefer `KeyPool::with_config` in tests over mutating process env.
- Product uses **lease-one** `acquire` only (no public batch API).

## ANTI-PATTERNS

- Do not wait while holding the acquire mutex (deadlock with reporters that need the lock path).
- Do not treat `lease_until` as exclusive mutex — it is a multi-hold reclaim deadline.
- Do not assume multi-process lease safety.
- Do not network from this crate.
- Do not reintroduce product `acquire_batch` that pins unused capacity.
