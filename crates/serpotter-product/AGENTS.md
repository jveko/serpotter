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
├── meta.rs             # ExecMeta / ExecMetaSink + AttemptRecord / TransitionRecord
├── lease.rs            # ReportMode, verdict_for, outcome_label, cooldown consts, with_key_proxy
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
| Exhausted HTTP codes | `search/exhausted.rs` (`tavily` 429/432/433; `firecrawl`/`exa` 402/429; `xai` 429; unknown provider defaults to 402). `verdict_for` checks `is_payment_required_status` (402) FIRST, so a 402 never reaches the exhausted arm — it becomes `PaymentRequired` and the key's credits are zeroed (a `NULL`-credit exa/xai key included), which is what stops it re-serving 402 |
| Dual-pool blame matrix | `lease.rs` `with_key_proxy` (tunnel → key release + node fail) |
| Hold finish / Drop | `hold.rs` |
| Research wire shape | `dto.rs` → `webResults` / `scrapedPages` / social |
| SSRF gate | `ssrf.rs` |
| Verdict → outcome label | `lease.rs` `verdict_for` (ProviderError → `ReportMode`) + `outcome_label` (ReportMode + optional status → the closed 8-value string `ok`/`payment_required`/`rate_limited`/`auth_invalid`/`forbidden`/`banned`/`retryable`/`failure`; `AuthFailure` splits on 403 → `forbidden`, else `auth_invalid`) |
| Attempt / transition records | `meta.rs` `ExecMeta::note_attempt` / `note_transition` push `AttemptRecord` / `TransitionRecord` (`transition` is already the CSV-safe `disabled`\|`credits_zeroed`\|`suspended`\|`deleted`; `KeyTransition::None` is never recorded). This crate only RECORDS — `serpotter-api` observes them into counters and ring rows, so the crate stays free of prometheus |
| Cooldown | `lease.rs` `cooldown_secs_for` (vendor `Retry-After` clamped to `MAX_COOLDOWN_SECS` = 3600, else `DEFAULT_COOLDOWN_SECS` = 60 — a `Retry-After` ≥ 2^63 must not wrap negative and bypass the clamp). Stamped by `finish_exhausted`; the pool DEMOTES a cooling key, it never filters it |
| Drained-credit class | `error.rs` `SearchExecError::CreditsExhausted` / `ExtractError::CreditsExhausted` — upstream `402`, its own class (not `Provider`/502). Reached on the FINAL ladder attempt, by which point every `402` key has been zeroed by its `PaymentRequired` report, so the pool is drained. The merge (`search/execute.rs` `leg_aggregate_err`, `search/chain.rs`, `extract/extract_url.rs`) scans for it and lets it WIN over any other provider-side error |

## CONVENTIONS

- Free-fns + `ProductCtx`; no `dyn` attempt-loop abstraction.
- Dual-pool: the PROXY is blamed in exactly two cases — a tunnel/connect-class `Http` error, and a `client_for` build failure (the node's own proxy URL did not parse); both do node `consecutive_fails`++ → disable at 3. Everything else releases the node untouched. The KEY is finished by the verdict alone (`finish_success`/`failure`/`exhausted`/`payment_required`/`banned`/`suspended`/`release`), proxy or not — including `Retryable`, which releases the key even on a confirmed tunnel error.
- `KeyHold`/`ProxyHold`: `finish_*` + disarm only on `Ok` report; `Drop` spawns release (never `block_on`); `finish_release` = inflight-- without fail++.
- xAI path never acquires outbound; `REQUIRE_OUTBOUND_PROXY` → `NoHealthyNode` when lease is `None`.
- Hybrid **web** leg: `fallback_chain("tavily")` only — never `fallback_chain("hybrid")`.
- Research web `SearchQuery` must **not** carry X handles (Gate 3 would route to xAI); social soft-empty on failure.
- API shells map thiserror → problem+json; this crate stays transport-free.
- Structured-extract legs run `extract_url::structured_leg_verdict`: `verdict_for` with ONE remap, `Banned → AuthFailure`. Those bodies carry vendor-produced text (the least reliable ban signal in the system) and the firecrawl ban tier hard-DELETEs the row, so a structured leg demotes (fail@3) and never deletes. Every other leg uses `verdict_for` directly.
- Dispositions: `report_suspended` (a proven vendor deactivation) stamps `disabled_reason = 'vendor_suspended'`, which the re-enable cron skips; the fail@3 path stamps `'auth_fail'` in the same UPDATE that clears `active`, which the cron DOES revive. The vocabulary is documented in `serpotter-db/AGENTS.md` and on `api/admin/keys.rs`'s `disabledReason`.

## ANTI-PATTERNS

- Do not add `serpotter-auth` / `axum` / `serpotter-api` deps (`Cargo.toml` FORBIDDEN).
- Do not return research `{search, extracts}`.
- Do not `block_on` in hold `Drop`.
- Do not fail the key on tunnel errors or burn the node on decode/body errors.
- Do not early-return without reporting a held key/proxy (hold leak).
