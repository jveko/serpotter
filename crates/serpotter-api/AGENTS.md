# serpotter-api

**Generated:** 2026-07-23 · sole binary + HTTP surface

## OVERVIEW

Axum process: private modules `admin/`, `product/`, `mcp/`, `credit_sync`, `log_request`; public `cron`, `AppState` + `app()`. Product orchestration lives in `serpotter-product` (handlers only auth/log/map errors).

## STRUCTURE

```
src/
├── lib.rs               # Router, live/ready, require_api_token; AdminCtx/ProductCtx helpers on AppState
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
│   └── mod.rs           # rmcp StreamableHttpService nest_service("/mcp"); tools + tok auth layer
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
| MCP tools / Streamable HTTP | `mcp/mod.rs` (`rmcp` `StreamableHttpService`, `#[tool]`, snake+camel params) |
| MCP sessions / SSE / DELETE | `rmcp` `LocalSessionManager` (TTL via `MCP_SESSION_TTL_SECS`; session header opaque UUID) |
| Admin auth | `admin/mod.rs` `require_admin(&AdminCtx, …)` (session Bearer then ADMIN_SECRET) |
| Admin sessions | `admin/session.rs` `POST /api/admin/bootstrap\|login\|logout` argon2 + `adm-` tokens |
| Credit sync | `admin/keys.rs` `sync_credits` → `credit_sync` |
| Request log | `log_request.rs` from product handlers |
| Maintenance cron | `cron.rs` `spawn_maintenance` (env: KEY_REENABLE_AFTER_HOURS, REQUEST_LOG_*) |
| Boot proxy / shutdown | `main.rs` `resolve_outbound_proxy_url`, graceful shutdown |

## CONVENTIONS

- Handlers: free `async fn` + `State<AppState>` + `HeaderMap`; convert with `state.admin_ctx()` / `product_ctx()` before domain work.
- Admin paths: `let ctx = state.admin_ctx(); require_admin(&ctx, &headers).await`; use `ctx.db` / `ctx.providers` / `ctx.admin_secret` (not `state.db` unless unavoidable).
- Auth REST: `require_api_token` before work; 401 problem+json.
- Admin: valid `admin_sessions` Bearer **or** `ADMIN_SECRET` via Bearer / `X-Admin-Password` (not tok-). Session works without ADMIN_SECRET.
- Product errors: typed `SearchExecError` / `ExtractError` with transparent `Db(DbError)`; map to problem details (`DatabaseError` 500) via `e.to_string()` at the API edge only.
- MCP: **all** `/mcp` methods require tok- Bearer or `x-api-key` (outer middleware). Session id ≠ authentication.
- MCP Streamable HTTP via **rmcp**: process-local `LocalSessionManager`; keep-alive default product TTL 1h; no multi-instance HA. Clients must `Accept: application/json, text/event-stream`. Stateful sessions mint `Mcp-Session-Id` on initialize; GET SSE; DELETE → 202. Host allowlist defaults to loopback; set `MCP_ALLOWED_HOSTS=host,host:port` for public binds.
- Admin credit sync: `service` optional (`tavily`|`firecrawl`|omit both); soft-fail per key; on-demand only (re-enable/purge is separate 15m cron).
- Integration tests rebuild `AppState` with providers on `127.0.0.1:9` via `tests/common`.

## ANTI-PATTERNS

- Do not `pub mod` private route modules without need.
- Do not parse MCP args camelCase-only — accept `max_results` / `web_max_results` / `scrape_top_n`.
- Do not return research `{search, extracts}`.
- Do not load dotenv in binary (document process env only).
- Do not put product orchestration in `serpotter-api` — keep it in `serpotter-product`.
- Do not pass `&AppState` into `require_admin` / `admin_secret_matches` — use `&AdminCtx`.
