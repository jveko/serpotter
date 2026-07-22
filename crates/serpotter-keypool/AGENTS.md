# serpotter-keypool

**Generated:** 2026-07-22 · in-process acquire/report

## OVERVIEW

Thin mutex + `Db` key leasing. Durable soft lease lives in SQLite (`lease_until`); this crate serializes concurrent acquire so LRU stamps do not race. **Single-process only** — no multi-instance coordination.

## STRUCTURE

```
src/
└── lib.rs   # KeyPool, LeasedKey, MAX_BATCH=10, tests
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| Acquire one / batch | `KeyPool::acquire` / `acquire_batch` (clamps 1..=10) |
| Report outcome | `report_success` / `report_failure` / `report_exhausted` |
| Soft lease SQL | `serpotter-db` (`LEASE_TTL_SECS=20`, clear on report) |

## CONVENTIONS

- Hold `Mutex<()>` only around acquire paths; reports hit DB without the mutex.
- Map `ApiKeyRow` → `LeasedKey { id, service, key }`.
- Empty healthy set → `KeyPoolError::NoHealthyKey`.

## ANTI-PATTERNS

- Do not assume multi-process lease safety — mutex is in-process only.
- Do not invent a second TTL; use `serpotter_db::LEASE_TTL_SECS`.
- Do not network from this crate.
