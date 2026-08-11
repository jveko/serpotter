# serpotter-providers

**Updated:** 2026-07-29 · upstream HTTP adapters

## OVERVIEW

`ProviderRegistry` dispatches search/extract to Tavily, Firecrawl, Exa, xAI. Maps vendor JSON → `serpotter_core` items.

Web providers take a per-call `proxy: Option<&str>`; clients are resolved via `ClientCache` (`HashMap` + `parking_lot`). xAI always dials direct and **ignores** `proxy`.

xAI request shapes the dialect cannot express are refused loudly (see `validate_xai_search_policy` in `xai.rs`): `include_content=true` is unsupported on both paths (results carry title+url only, no page content), the social (X) path refuses non-empty `allowed_domains`/`excluded_domains` (no structured field — tools are empty), and mixed `sources=["web","x"]` on the xAI provider is refused (it cannot serve web sources; use hybrid). `from_date`/`to_date`/`time_range` have no structured `web_search` param and are best-effort NL prose; `search()` logs a one-time warn when set.

## STRUCTURE

```
src/
├── lib.rs        # ProviderRegistry, errors, shared params
├── http.rs       # try_build_http, is_tunnel_error, ClientCache
├── tavily.rs     # body api_key auth; search + extract (+ Client)
├── firecrawl.rs  # Bearer; /v2/search + scrape (+ Client)
├── exa.rs        # Bearer; /search (+ Client)
├── xai.rs        # Bearer; /responses (always direct HTTP client)
└── usage.rs      # parse_tavily_usage / parse_firecrawl_usage fixtures
```

## WHERE TO LOOK

| Task | File |
|------|------|
| Add provider | new module + match arms in `lib.rs` |
| Proxy client cache | `http.rs` `ClientCache` + `search`/`extract` proxy arg |
| Hard proxy build errors | `try_build_http(Some)` — no silent direct fallback |
| Tunnel classification | `is_tunnel_error` |
| xAI social vs web | `xai.rs` tools empty vs `web_search` |
| Extract path | `extract` on Firecrawl/Tavily only |
| Usage parsers | `usage.rs` fixture-tested; no live vendor in unit tests |

## CONVENTIONS

- `search(provider, params, proxy)` / `extract(provider, url, key, proxy)`: web providers use `client_for(proxy)`; invalid proxy URL when `Some` → `ProviderError::Http` (no silent direct).
- Soft cache max ~32 distinct proxy URLs; arbitrary drop when exceeded.
- xAI: own direct client; never `Proxy::all`; never `tools.type=x_search`.
- Credit sync: `fetch_usage(http, key)` with `registry.direct_client()`.
- Vendor field renames stay local (e.g. `publishedDate`, `sourceURL`).
- User-Agent: `Serpotter/0.1`.

## ANTI-PATTERNS

- Do not route xAI through commercial proxy.
- Do not invent a second Tavily crate (orphan `serpotter-tavily` was removed).
- Prefer no network in unit tests — api integration points clients at `127.0.0.1:9`.
- No moka/dashmap for client cache — `HashMap` + `parking_lot` only.
- No silent direct fallback when a leased proxy URL fails to build.
