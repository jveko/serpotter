# serpotter-api

**Updated:** 2026-07-29 · sole binary + HTTP surface

## OVERVIEW

Axum process: private modules `admin/`, `product/`, `mcp/`, `credit_sync`, `log_request`; public `cron`, `AppState` + `app()` / `app_with_spa()`. Product orchestration lives in `serpotter-product` (handlers only auth/log/map errors).

## STRUCTURE

```
src/
├── lib.rs               # Router, live/ready, require_api_token; AdminCtx/ProductCtx helpers on AppState
├── main.rs              # seed-token | seed-key | serve + graceful shutdown
├── product/
│   ├── mod.rs
│   ├── search.rs        # POST /api/search
│   ├── extract.rs       # POST /api/extract, /api/research
│   └── errors.rs        # thiserror → problem+json map
├── admin/
│   ├── mod.rs           # AdminCtx, require_admin, admin_secret_matches, mask_*
│   ├── session.rs       # bootstrap | login | logout
│   ├── tokens.rs        # /api/tokens
│   ├── keys.rs          # /api/keys + sync-credits
│   ├── nodes.rs         # /api/nodes
│   ├── settings.rs      # /api/settings
│   ├── stats.rs         # /api/stats
│   └── logs.rs          # /api/request-logs
├── mcp/
│   ├── mod.rs           # rmcp StreamableHttpService + SerpotterMcp tools
│   ├── auth.rs          # outer tok- middleware
│   ├── params.rs        # snake+camel tool params → core/product
│   └── progress.rs      # best-effort soft_progress
├── credit_sync.rs       # tavily/firecrawl real usage; exa/xai soft-error only
├── log_request.rs       # fire-and-forget request_log inserts
└── cron.rs              # 15m re-enable keys + purge request_log
tests/
├── common/              # shared AppState / oneshot helpers (:9 providers, fixed tok-)
├── health.rs
├── search_auth.rs
├── extract_research.rs
├── admin_session.rs
├── admin_keys_credits.rs
├── admin_nodes_logs.rs
├── mcp_tools.rs
└── mcp_session.rs
```

## WHERE TO LOOK

| Task | File |
|------|------|
| Add route | `lib.rs` `app_with_spa()` (declare **before** the SPA fallback) |
| SPA at site root | `lib.rs` `ADMIN_SPA_DIR` → `ServeDir` + `index.html` fallback as `fallback_service`; tests `tests/spa_serving.rs` |
| Search/extract/research handlers | `product/search.rs`, `product/extract.rs` |
| Problem map (product errors) | `product/errors.rs` |
| Search/hybrid/blend orchestration | `serpotter-product` (`search_inner`, `extract_url`, `research_inner`) |
| Research wire shape | `serpotter-product` DTOs (`webResults`/`scrapedPages`) |
| MCP tools / Streamable HTTP | `mcp/mod.rs` (`rmcp` `StreamableHttpService`, `#[tool]`, snake+camel params) |
| MCP sessions / SSE / DELETE | `rmcp` `LocalSessionManager` (TTL via `MCP_SESSION_TTL_SECS`; session header opaque UUID) |
| Admin auth | `admin/mod.rs` `require_admin(&AdminCtx, …)` (session Bearer then ADMIN_SECRET) |
| Admin sessions | `admin/session.rs` `POST /api/admin/bootstrap\|login\|logout` argon2 + `adm-` tokens |
| Credit sync | `admin/keys.rs` `sync_credits` → `credit_sync` |
| Request logs admin list | `admin/logs.rs` |
| Request log | `log_request.rs` from product handlers |
| Maintenance cron | `cron.rs` `spawn_maintenance` (env: KEY_REENABLE_AFTER_HOURS, REQUEST_LOG_*) |
| Boot / ProxyPool / shutdown | `main.rs` — zero key+node inflight; `ProxyPool::with_options` (env + `REQUIRE_OUTBOUND_PROXY`); graceful shutdown |

## CONVENTIONS

- Handlers: free `async fn` + `State<AppState>` + `HeaderMap`; convert with `state.admin_ctx()` / `product_ctx()` before domain work.
- Admin paths: `let ctx = state.admin_ctx(); require_admin(&ctx, &headers).await`; use `ctx.db` / `ctx.providers` / `ctx.admin_secret` (not `state.db` unless unavoidable).
- Auth REST: `require_api_token` before work; 401 problem+json.
- Admin: valid `admin_sessions` Bearer **or** `ADMIN_SECRET` via Bearer / `X-Admin-Password` (not tok-). Session works without ADMIN_SECRET.
- Product errors: typed `SearchExecError` / `ExtractError` with transparent `Db(DbError)`; map to problem details (`DatabaseError` 500) via `e.to_string()` at the API edge only.
- MCP: **all** `/mcp` methods require tok- Bearer or `x-api-key` (outer middleware). Session id ≠ authentication.
- MCP Streamable HTTP via **rmcp**: process-local `LocalSessionManager`; keep-alive default product TTL 1h; no multi-instance HA. Clients must `Accept: application/json, text/event-stream`. Stateful sessions mint `Mcp-Session-Id` on initialize; GET SSE; DELETE → 202. Host allowlist defaults to loopback; set `MCP_ALLOWED_HOSTS=host,host:port` for public binds.
- Admin credit sync: `service` optional (`tavily`|`firecrawl`|`exa`|`xai`; omit → tavily+firecrawl). Real usage for tavily/firecrawl; exa/xai soft-error only (no credit write). Soft-fail per key (never `active=0` on fetch error). On-demand via `POST /api/keys/sync-credits`; optional 15m cron when `CREDIT_SYNC_CRON=1` (tavily+firecrawl only).
- Integration tests rebuild `AppState` with providers on `127.0.0.1:9` and `ProxyPool::from_env_and_db(None, db)` via `tests/common`.

## ANTI-PATTERNS

- Do not `pub mod` private route modules without need.
- Do not parse MCP args camelCase-only — accept `max_results` / `web_max_results` / `scrape_top_n`.
- Do not return research `{search, extracts}`.
- Do not load dotenv in binary (document process env only).
- Do not put product orchestration in `serpotter-api` — keep it in `serpotter-product`.
- Do not pass `&AppState` into `require_admin` / `admin_secret_matches` — use `&AdminCtx`.
