# serpotter-product

**Generated:** 2026-07-29 · pure orchestration (no HTTP / auth)

## OVERVIEW

Search / extract / research free-fns over `ProductCtx`. Owns DTOs + three thiserror enums. **Never** depends on `serpotter-auth`, `axum`, or `serpotter-api`.

## STRUCTURE

```
src/
├── lib.rs              # ProductCtx + re-exports
├── dto.rs              # Extract*/Research* camelCase wire
├── error.rs            # SearchExecError | ExtractError | ResearchError
├── hold.rs             # KeyHold / ProxyHold RAII
├── report.rs           # classify_proxied_http dual-matrix
├── ssrf.rs             # validate_extract_url
├── search/
│   ├── mod.rs          # search_inner
│   ├── execute.rs      # single | hybrid | blend
│   ├── run_provider.rs # dual-pool attempt loop (max 3)
│   ├── exhausted.rs    # is_exhausted_status
│   └── leg_errors.rs   # multi/hybrid/blend error merge
└── extract/
    ├── extract_url.rs  # Firecrawl↔Tavily chain + dual-pool
    ├── research.rs     # web + scrapes + optional social
    └── helpers.rs      # scrape targets, map_social_leg
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| Entry orchestration | `search_inner`, `extract_url`, `research_inner` |
| Provider attempt + holds | `search/run_provider.rs` |
| Hybrid / blend / single | `search/execute.rs` |
| Exhausted HTTP codes | `search/exhausted.rs` (`tavily` 429/432/433; `firecrawl`/`exa` 402/429; `xai` 429) |
| Dual-pool blame matrix | `report.rs` `classify_proxied_http` |
| Hold finish / Drop | `hold.rs` |
| Research wire shape | `dto.rs` → `webResults` / `scrapedPages` / social |
| SSRF gate | `ssrf.rs` |

## CONVENTIONS

- Free-fns + `ProductCtx`; no `dyn` attempt-loop abstraction.
- Dual-pool: tunnel → key **release** + node **fail**; non-tunnel proxied decode/body → **both release only**; direct → key fail.
- `KeyHold`/`ProxyHold`: `finish_*` + disarm only on `Ok` report; `Drop` spawns release (never `block_on`); `finish_release` = inflight-- without fail++.
- xAI path never acquires outbound; `REQUIRE_OUTBOUND_PROXY` → `NoHealthyNode` when lease is `None`.
- Hybrid **web** leg: `fallback_chain("tavily")` only — never `fallback_chain("hybrid")`.
- Research web `SearchQuery` must **not** carry X handles (Gate 3 would route to xAI); social soft-empty on failure.
- API shells map thiserror → problem+json; this crate stays transport-free.

## ANTI-PATTERNS

- Do not add `serpotter-auth` / `axum` / `serpotter-api` deps (`Cargo.toml` FORBIDDEN).
- Do not return research `{search, extracts}`.
- Do not `block_on` in hold `Drop`.
- Do not fail the key on tunnel errors or burn the node on decode/body errors.
- Do not early-return without reporting a held key/proxy (hold leak).
