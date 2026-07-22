# serpotter-api

**Generated:** 2026-07-23 · sole binary + HTTP surface

## OVERVIEW

Axum process: private modules `admin/`, `product/`, `mcp/`, `credit_sync`, `log_request`; public `cron`, `AppState` + `app()`. Product orchestration lives in `serpotter-product` (handlers only auth/log/map errors).

## STRUCTURE

```
src/
├── main.rs              # seed-token | seed-key | serve + proxy resolve + spawn_maintenance + graceful shutdown
├── lib.rs               # Router, live/ready, require_api_token; AdminCtx/McpCtx/ProductCtx helpers on AppState
├── product/
│   ├── mod.rs
│   ├── search.rs        # POST /api/search
│   └── extract.rs       # POST /api/extract, /api/research
├── admin/
│   ├── mod.rs           # AdminCtx, require_admin, admin_secret_matches, mask_*
│   ├── session.rs       # bootstrap | login | logout
│   ├── tokens.rs        # /api/tokens
│   ├── keys.rs          # /api/keys + sync-credits
│   ├── nodes.rs         # /api/nodes
│   ├── settings.rs      # /api/settings
│   └── stats.rs         # /api/stats
├── mcp/
│   ├── mod.rs           # POST /mcp JSON-RPC tools (+ optional session mint)
│   ├── session.rs       # process-local McpSessionStore (TTL 1h)
│   └── stream.rs        # GET /mcp SSE KeepAlive + DELETE /mcp terminate
├── credit_sync.rs       # tavily/firecrawl credit fetch + soft-fail report
├── log_request.rs       # fire-and-forget request_log inserts
└── cron.rs              # 15m re-enable keys + purge request_log
tests/
├── common/              # shared AppState / oneshot helpers
├── health.rs
├── search_auth.rs
├── extract_research.rs
├── admin_session.rs
├── admin_keys_credits.rs
├── mcp_tools.rs
└── mcp_session.rs
```

## WHERE TO LOOK

| Task | File |
|------|------|
| Add route | `lib.rs` `app()` |
| Search/extract/research handlers | `product/search.rs`, `product/extract.rs` |
| Search/hybrid/blend orchestration | `serpotter-product` (`search_inner`, `extract_url`, `research_inner`) |
| Research wire shape | `serpotter-product` DTOs (`webResults`/`scrapedPages`) |
| MCP tool args | `mcp/mod.rs` (`arg_u32` snake then camel) |
| MCP sessions / SSE | `mcp/session.rs` + `mcp/stream.rs` (`Mcp-Session-Id`) |
| Admin auth | `admin/mod.rs` `require_admin(&AdminCtx, …)` (session Bearer then ADMIN_SECRET) |
| Admin sessions | `admin/session.rs` `POST /api/admin/bootstrap\|login\|logout` argon2 + `adm-` tokens |
| Credit sync | `admin/keys.rs` `sync_credits` → `credit_sync` |
| Request log | `log_request.rs` from product handlers |
| Maintenance cron | `cron.rs` `spawn_maintenance` (env: KEY_REENABLE_AFTER_HOURS, REQUEST_LOG_*) |
| Boot proxy / shutdown | `main.rs` `resolve_outbound_proxy_url`, graceful shutdown |

## CONVENTIONS

- Handlers: free `async fn` + `State<AppState>` + `HeaderMap`; convert with `state.admin_ctx()` / `product_ctx()` / `mcp_ctx()` before domain work.
- Admin paths: `let ctx = state.admin_ctx(); require_admin(&ctx, &headers).await`; use `ctx.db` / `ctx.providers` / `ctx.admin_secret` (not `state.db` unless unavoidable).
- Auth REST: `require_api_token` before work; 401 problem+json.
- Admin: valid `admin_sessions` Bearer **or** `ADMIN_SECRET` via Bearer / `X-Admin-Password` (not tok-). Session works without ADMIN_SECRET.
- Product errors: typed `SearchExecError` / `ExtractError` with transparent `Db(DbError)`; map to problem details (`DatabaseError` 500) via `e.to_string()` at the API edge only.
- MCP: `initialize`/`ping` may skip auth; `tools/call` requires token.
- MCP Streamable subset: process-local sessions; TTL 1h; no multi-instance. POST mints/validates `mcp-session-id`; GET SSE KeepAlive; DELETE terminates (204).
- Admin credit sync: `service` optional (`tavily`|`firecrawl`|omit both); soft-fail per key; on-demand only (re-enable/purge is separate 15m cron).
- Integration tests rebuild `AppState` with providers on `127.0.0.1:9` via `tests/common`.

## ANTI-PATTERNS

- Do not `pub mod` private route modules without need.
- Do not parse MCP args camelCase-only — accept `max_results` / `web_max_results` / `scrape_top_n`.
- Do not return research `{search, extracts}`.
- Do not load dotenv in binary (document process env only).
- Do not put product orchestration in `serpotter-api` — keep it in `serpotter-product`.
- Do not pass `&AppState` into `require_admin` / `admin_secret_matches` — use `&AdminCtx`.
