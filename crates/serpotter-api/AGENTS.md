# serpotter-api

**Generated:** 2026-07-22 · sole binary + HTTP surface

## OVERVIEW

Axum process: private modules `admin` / `extract` / `mcp` / `search`; public `AppState` + `app()`.

## STRUCTURE

```
src/
├── main.rs      # seed-token | seed-key | serve + proxy resolve
├── lib.rs       # Router, live/ready, require_api_token
├── search.rs    # POST /api/search + search_inner (shared)
├── extract.rs   # /api/extract, /api/research
├── mcp.rs       # POST /mcp JSON-RPC tools
└── admin.rs     # tokens/keys/settings/stats/nodes
tests/health.rs  # oneshot integration suite
```

## WHERE TO LOOK

| Task | File |
|------|------|
| Add route | `lib.rs` `app()` |
| Change search/hybrid/blend | `search.rs` |
| Research wire shape | `extract.rs` (`webResults`/`scrapedPages`) |
| MCP tool args | `mcp.rs` (`arg_u32` snake then camel) |
| Admin auth | `admin.rs` `require_admin` |
| Boot proxy | `main.rs` `resolve_outbound_proxy_url` |

## CONVENTIONS

- Handlers: free `async fn` + `State<AppState>` + `HeaderMap`.
- Auth REST: `require_api_token` before work; 401 problem+json.
- Admin: `ADMIN_SECRET` via Bearer **or** `X-Admin-Password` (not tok-).
- MCP: `initialize`/`ping` may skip auth; `tools/call` requires token.
- Integration tests rebuild `AppState` with providers on `127.0.0.1:9`.

## ANTI-PATTERNS

- Do not `pub mod` private route modules without need.
- Do not parse MCP args camelCase-only — accept `max_results` / `web_max_results` / `scrape_top_n`.
- Do not return research `{search, extracts}`.
- Do not load dotenv in binary (document process env only).
