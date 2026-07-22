# serpotter-providers

**Generated:** 2026-07-22 · upstream HTTP adapters

## OVERVIEW

`ProviderRegistry` dispatches search/extract to Tavily, Firecrawl, Exa, xAI. Maps vendor JSON → `serpotter_core` items.

## STRUCTURE

```
src/
├── lib.rs        # ProviderRegistry, errors, shared params
├── tavily.rs     # body api_key auth; search + extract
├── firecrawl.rs  # Bearer; /v2/search + scrape
├── exa.rs        # Bearer; /search
└── xai.rs        # Bearer; /responses (always direct HTTP client)
```

## WHERE TO LOOK

| Task | File |
|------|------|
| Add provider | new module + match arms in `lib.rs` |
| Proxy-aware client | `new_with_proxy` on Tavily/Firecrawl/Exa |
| xAI social vs web | `xai.rs` tools empty vs `web_search` |
| Extract path | `extract` on Firecrawl/Tavily only |

## CONVENTIONS

- `ProviderRegistry::with_proxy_url(Some(url))` applies **reqwest Proxy::all** to web providers only.
- xAI: `new` ignores proxy; never `tools.type=x_search`.
- Vendor field renames stay local (e.g. `publishedDate`, `sourceURL`).
- User-Agent: `Serpotter/0.1`.

## ANTI-PATTERNS

- Do not route xAI through commercial proxy.
- Do not invent a second Tavily crate (orphan `serpotter-tavily` was removed).
- Prefer no network in unit tests — api integration points clients at `127.0.0.1:9`.
