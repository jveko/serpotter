# serpotter-core

**Updated:** 2026-07-29 · pure domain (no I/O)

## OVERVIEW

Shared search types, 6-gate routing, RRF merge, URL normalize. No sqlx/reqwest/axum.

## STRUCTURE

```
src/
├── lib.rs            # re-exports only
├── types.rs          # SearchQuery/Item/Response (camelCase)
├── routing/          # route_search, RULES, resolve helpers (mod/rules/resolve)
├── pipeline.rs       # RRF k=60 + URL-normalize dedupe
└── url_normalize.rs  # tracking-strip keys for RRF
```

## WHERE TO LOOK

| Task | File |
|------|------|
| New route rule / gate | `routing/` `RULES` + `route_search` |
| Fallback provider order | `routing/` `fallback_chain` |
| Merge ranked lists | `pipeline.rs` `reciprocal_rank_fusion` |
| Wire field names | `types.rs` |
| Dedupe key | `url_normalize.rs` `normalize_url` |
| Near-dupe suppression | `minhash.rs` `dedupe_near_duplicates` |

## CONVENTIONS

- Free-fns only at public surface; no services/traits.
- REST DTOs: `rename_all = "camelCase"`; `Sources`/`VecOrOne` untagged string|array.
- Intent/strategy resolution lives here; execution (HTTP keys) stays in api/providers.
- Unit tests are pure `#[test]` (no tokio).
- `minhash.rs` stays pure and dependency-free — fixed-seed hashing only, deterministic across runs.

## ANTI-PATTERNS

- Do not add network or DB deps to this crate.
- Do not encode snake_case at this layer — HTTP boundary owns wire casing.
