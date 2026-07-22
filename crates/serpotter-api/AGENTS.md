# serpotter-api

**Generated:** 2026-07-22 · sole binary + HTTP surface

## OVERVIEW

Axum process: private modules `admin` / `extract` / `mcp` / `mcp_session` / `mcp_stream` / `search` / `log_request`; public `cron`, `AppState` + `app()`.

## STRUCTURE

```
src/
├── main.rs        # seed-token | seed-key | serve + proxy resolve + spawn_maintenance
├── lib.rs         # Router, live/ready, require_api_token
├── search.rs      # POST /api/search + search_inner (shared)
├── extract.rs     # /api/extract, /api/research
├── log_request.rs # fire-and-forget request_log inserts
├── cron.rs        # 15m re-enable keys + purge request_log
├── mcp.rs         # POST /mcp JSON-RPC tools (+ optional session mint)
├── mcp_session.rs # process-local McpSessionStore (TTL 1h)
├── mcp_stream.rs  # GET /mcp SSE KeepAlive + DELETE /mcp terminate
└── admin.rs       # tokens/keys/settings/stats/nodes + sync-credits + bootstrap/login/logout
tests/health.rs    # oneshot integration suite
```

## WHERE TO LOOK

| Task | File |
|------|------|
| Add route | `lib.rs` `app()` |
| Change search/hybrid/blend | `search.rs` |
| Research wire shape | `extract.rs` (`webResults`/`scrapedPages`) |
| MCP tool args | `mcp.rs` (`arg_u32` snake then camel) |
| MCP sessions / SSE | `mcp_session.rs` + `mcp_stream.rs` (`Mcp-Session-Id`) |
| Admin auth | `admin.rs` `require_admin` (session Bearer then ADMIN_SECRET) |
| Admin sessions | `POST /api/admin/bootstrap|login|logout` argon2 + `adm-` tokens |
| Credit sync | `admin.rs` `sync_credits` → `POST /api/keys/sync-credits` |
| Request log | `log_request.rs` from search/extract/research handlers |
| Maintenance cron | `cron.rs` `spawn_maintenance` (env: KEY_REENABLE_AFTER_HOURS, REQUEST_LOG_*) |
| Boot proxy | `main.rs` `resolve_outbound_proxy_url` |

## CONVENTIONS

- Handlers: free `async fn` + `State<AppState>` + `HeaderMap`.
- Auth REST: `require_api_token` before work; 401 problem+json.
- Admin: valid `admin_sessions` Bearer **or** `ADMIN_SECRET` via Bearer / `X-Admin-Password` (not tok-). Session works without ADMIN_SECRET.
- MCP: `initialize`/`ping` may skip auth; `tools/call` requires token.
- MCP Streamable subset: process-local sessions; TTL 1h; no multi-instance. POST mints/validates `mcp-session-id`; GET SSE KeepAlive; DELETE terminates (204).
- Admin credit sync: `service` optional (`tavily`|`firecrawl`|omit both); soft-fail per key; on-demand only (re-enable/purge is separate 15m cron).
- Integration tests rebuild `AppState` with providers on `127.0.0.1:9`.

## ANTI-PATTERNS

- Do not `pub mod` private route modules without need.
- Do not parse MCP args camelCase-only — accept `max_results` / `web_max_results` / `scrape_top_n`.
- Do not return research `{search, extracts}`.
- Do not load dotenv in binary (document process env only).
