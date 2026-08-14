# serpotter-api

**Updated:** 2026-08-01 · sole binary + HTTP surface

## OVERVIEW

Axum process: private modules `admin/`, `product/`, `mcp/`, `credit_sync`, `events`; public `trace_layer`, `cron`, `AppState` + `app()` / `app_with_spa()`. Product orchestration lives in `serpotter-product` (handlers only auth/log/map errors).

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
│   ├── errors.rs        # tool_error_structured JSON envelope {kind,message,requestId}
│   ├── params.rs        # snake+camel tool params → core/product
│   └── progress.rs      # McpProgressSink: opt-in notifications/progress (token → SSE; no token → plain JSON)
├── credit_sync.rs       # tavily/firecrawl real usage; exa/xai soft-error only
├── events.rs           # request-events funnel (`events::emit`: log line + ring + error window + metrics + usage writer)
├── trace_layer.rs       # TraceLayer + request-id-aware MakeSpan (method/path/request_id)
└── cron.rs              # 15m re-enable keys/nodes + purge cache/sessions + high-error alert
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
| MCP legacy sessions / SSE / DELETE | `rmcp` `LocalSessionManager` (TTL via `MCP_SESSION_TTL_SECS`; session header opaque UUID) — legacy clients only (≤ 2025-11-25); 2026-07-28 is stateless |
| Admin auth | `admin/mod.rs` `require_admin(&AdminCtx, …)` (session Bearer then ADMIN_SECRET) |
| Trace / request-id | `trace_layer.rs` `build_http_layers` (Set → Trace → Propagate order; Set stores effective id in the `RequestId` extension — inbound header wins, else mints UUID; Propagate copies it to the response header; MakeSpan reads the extension) + `main.rs` assembly |
| Admin sessions | `admin/session.rs` `POST /api/admin/bootstrap\|login\|logout` argon2 + `adm-` tokens |
| Credit sync | `admin/keys.rs` `sync_credits` → `credit_sync` |
| Request logs admin list | `admin/logs.rs` (`ListLogsQuery`: limit default 50 clamp 1..=200, status lenient string → parsed i64, unparseable treated as absent, path prefix, service, requestId)` |
| Request events | `events.rs` `events::emit` from product handlers + MCP tools (funnel: structured log line `target: "request"`, in-memory ring cap 2048 → `admin/logs.rs`, error window → cron alert, metrics, write-time `usage_daily` upsert; token_name via TokenRow extension / `get_token_by_value` fallback) |
| Maintenance cron | `cron.rs` `spawn_maintenance` (env: KEY_REENABLE_AFTER_HOURS, NODE_REENABLE_AFTER_HOURS, ADMIN_ALERT_URL) |
| Boot / ProxyPool / shutdown | `main.rs` — zero key+node inflight; `ProxyPool::with_options(db, require)` nodes-only; graceful shutdown |

## CONVENTIONS

- Handlers: free `async fn` + `State<AppState>` + `HeaderMap`; convert with `state.admin_ctx()` / `product_ctx()` before domain work.
- Admin paths: `let ctx = state.admin_ctx(); require_admin(&ctx, &headers).await`; use `ctx.db` / `ctx.providers` / `ctx.admin_secret` (not `state.db` unless unavoidable).
- Auth REST: `require_api_token` before work; 401 problem+json.
- Admin: valid `admin_sessions` Bearer **or** `ADMIN_SECRET` via Bearer / `X-Admin-Password` (not tok-). Session works without ADMIN_SECRET.
- Product errors: typed `SearchExecError` / `ExtractError` with transparent `Db(DbError)`; map to problem details (`DatabaseError` 500) via `e.to_string()` at the API edge only.
- MCP: **all** `/mcp` methods require tok- Bearer or `x-api-key` (outer middleware). Session id ≠ authentication.
- MCP Streamable HTTP via **rmcp** 3.x (dual-era): protocol **2026-07-28** is served **statelessly** — every POST carries `MCP-Protocol-Version` + `Mcp-Method` (+`Mcp-Name` on `tools/call`) headers and per-request `_meta` (`io.modelcontextprotocol/protocolVersion` + `clientCapabilities`); `server/discover` advertises versions/capabilities; GET/DELETE → 405; cancellation = client disconnect. Older clients (≤ 2025-11-25) keep process-local `LocalSessionManager` sessions (keep-alive default product TTL 1h; no multi-instance HA): `initialize` mints `Mcp-Session-Id`, GET SSE, DELETE → 202. Clients must `Accept: application/json, text/event-stream`. Host allowlist defaults to loopback; set `MCP_ALLOWED_HOSTS=host,host:port` for public binds and `MCP_ALLOWED_ORIGINS` for browser origins.
- Admin credit sync: `service` optional (`tavily`|`firecrawl`|`exa`|`xai`; omit → tavily+firecrawl). Real usage for tavily/firecrawl; exa/xai soft-error only (no credit write). Soft-fail per key (never `active=0` on fetch error). On-demand via `POST /api/keys/sync-credits`; optional 15m cron when `CREDIT_SYNC_CRON=1` (tavily+firecrawl only).
- Integration tests rebuild `AppState` with providers on `127.0.0.1:9` and `ProxyPool::new(db)` via `tests/common`.
- Observability: `events::emit` is **fire-and-forget** — never fails the request path; one event lands a structured log line (`target: "request"`), a ring entry (cap 2,048; admin `GET /api/request-logs`), an error-window update, a metrics observation, and a write-time `usage_daily` delta; `service` stores vendor family (never hybrid/blend), `provider_used` the dial label.

## ANTI-PATTERNS

- Do not `pub mod` private route modules without need.
- Do not parse MCP args camelCase-only — accept `max_results` / `web_max_results` / `scrape_top_n`.
- Do not return research `{search, extracts}`.
- Do not load dotenv in binary (document process env only).
- Do not put product orchestration in `serpotter-api` — keep it in `serpotter-product`.
- Do not pass `&AppState` into `require_admin` / `admin_secret_matches` — use `&AdminCtx`.
