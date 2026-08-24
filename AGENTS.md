# PROJECT KNOWLEDGE BASE

**Updated:** 2026-08-09
**Branch:** main

## OVERVIEW

Rust crates-only workspace: multi-provider search proxy (Tavily/Firecrawl/Exa/xAI) + extract/research + lean MCP + admin API/SPA on a single VPS binary (`serpotter-api`), SQLite via sqlx.

## STRUCTURE

```
serpotter/
├── Cargo.toml              # workspace only (no root package)
├── Dockerfile              # multi-stage SPA + cargo-chef; non-root uid 10001; HEALTHCHECK /ready; VOLUME /data
├── docker-compose.yml      # api + named volume + healthcheck (local build)
├── docker-compose.prod.yml # standalone GHCR pull stack (no base compose)
├── crates/
│   ├── serpotter-api/      # sole binary + thin axum shells (admin/ mcp/ product/)
│   ├── serpotter-product/  # pure orchestration: search/extract/research + DTOs + thiserror
│   ├── serpotter-core/     # pure: routing, RRF, types, URL normalize
│   ├── serpotter-db/       # sqlx pool + migrations (schema v14) multi-module
│   ├── serpotter-auth/     # tok-, extract, problem+json
│   ├── serpotter-keypool/  # shared-cap acquire/report + wait/notify
│   ├── serpotter-providers/# Tavily/Firecrawl/Exa/xAI HTTP (connect 10s / timeout 60s)
│   └── serpotter-outbound/ # ProxyPool + URL helpers (reqwest Proxy::all)
├── web/                    # Vite+ React SPA (strict TS; NOT a Cargo member)
├── docs/ops/               # deploy, env, API contract
└── data/                   # gitignored SQLite default path (host)
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| HTTP routes / AppState | `crates/serpotter-api/src/lib.rs` | `app()` registers all routes; thin shells only |
| Product REST handlers | `crates/serpotter-api/src/product/` | `search.rs`, `extract.rs` → `serpotter_product::*` |
| Search / extract / research logic | `crates/serpotter-product/` | `ProductCtx`, DTOs, three thiserror enums; **no** auth/axum |
| MCP Streamable HTTP (rmcp) | `crates/serpotter-api/src/mcp/mod.rs` | `StreamableHttpService` + tok middleware; tools call product free-fns |
| Admin CRUD / sessions | `crates/serpotter-api/src/admin/` | keys, nodes, settings, tokens, stats, session |
| Admin SPA | `web/` (+ `AGENTS.md`) | Vite+; TanStack Router/Query; Base UI; `adm-` session; playground `tok-`; `/dashboard` default landing |
| Process entry / CLI / shutdown | `crates/serpotter-api/src/main.rs` | seed-token, seed-key, serve + `with_graceful_shutdown` |
| Maintenance cron | `crates/serpotter-api/src/cron.rs` | 15m re-enable / purge / optional credit sync |
| 6-gate routing | `crates/serpotter-core/src/routing/` | free-fn `route_search` |
| RRF / dedupe | `crates/serpotter-core/src/pipeline.rs` | k=60, normalizeUrl keys |
| Wire DTOs (core search types) | `crates/serpotter-core/src/types.rs` | REST camelCase |
| Product DTOs / errors | `crates/serpotter-product/src/` | extract/research shapes + SearchExec/Extract/Research errors |
| Migrations / schema | `crates/serpotter-db/migrations/` | SoT; `EXPECTED_SCHEMA_VERSION=17` |
| Provider HTTP + timeouts | `crates/serpotter-providers/src/http.rs` | `HTTP_CONNECT_TIMEOUT=10s`, `HTTP_REQUEST_TIMEOUT=60s` |
| Outbound ProxyPool | `crates/serpotter-outbound/` (+ `AGENTS.md`) | nodes-only least-inflight / direct; env Fixed removed |
| Integration tests | `crates/serpotter-api/tests/` | `common` fixture + split suites; providers → `:9` |
| Tracing / request events | `crates/serpotter-api/src/{trace_layer,events}.rs` | TraceLayer; `events::emit` funnel (log + ring + metrics + usage) |
| Ops | `docs/ops/` | deploy, env, api |

## CODE MAP

| Symbol | Type | Location | Role |
|--------|------|----------|------|
| `app` | fn | `serpotter-api/src/lib.rs` | Router assembly + state |
| `AppState` | struct | same | db, keys, outbound, providers, admin_secret |
| `ProductCtx` | struct | `serpotter-product` | db + keys + outbound + providers for product free-fns |
| `search_inner` / `extract_url` / `research_inner` | fn | `serpotter-product` | orchestration (REST + MCP) |
| `mcp::service` | fn | `api/src/mcp/mod.rs` | rmcp StreamableHttpService + tok middleware |
| `route_search` | fn | `core/src/routing/` | 6-gate provider decision |
| `reciprocal_rank_fusion` | fn | `core/src/pipeline.rs` | hybrid/blend merge |
| `connect_and_migrate` | fn | `db/src/lib.rs` | pool + embed migrations |
| `KeyPool` | struct | `keypool/src/lib.rs` | shared-cap acquire + wait/notify; env `KEY_*` |
| `ProxyPool` | struct | `outbound/src/lib.rs` | nodes-only least-inflight enabled row or direct; per-attempt lease |
| `ProviderRegistry` | struct | `providers/src/lib.rs` | search/extract dispatch; per-call proxy cache |
| `build_http` | fn | `providers/src/http.rs` | reqwest + 10s/60s (+ optional proxy) |
| `generate_token` / `extract_token` | fn | `auth/src/lib.rs` | tok- + Bearer/x-api-key |
| `proxy_url_from_node` | fn | `outbound/src/lib.rs` | build node URL for ProxyPool |
| `shutdown_signal` | fn | `api/src/main.rs` | Ctrl+C / SIGTERM → graceful serve stop |

## CONVENTIONS

- **Crates-only Rust:** binary under `crates/serpotter-api` (lib+bin); never reintroduce `apps/*` Cargo packages.
- **Product purity:** `serpotter-product` depends on core/db/keypool/outbound/providers only — **never** `serpotter-auth` or `axum`. Problem+json mapping stays in api shells.
- **Workspace deps only:** versions in root `[workspace.dependencies]`; members use `{ workspace = true }`.
- **REST/admin JSON:** `#[serde(rename_all = "camelCase")]`. MCP tool args: **snake_case preferred**, camelCase aliases.
- **Free-fns for pure logic** (routing, RRF, auth, outbound URL, product orchestration); stateful types: `Db`, `KeyPool`, `ProxyPool`, `*Client`, `AppState`. No `dyn` trait objects in product path.
- **sqlx:** raw `query` + binds; migrations in `serpotter-db/migrations`; in-memory tests use `sqlite::memory:` with `max_connections=1`.
- **Errors:** REST auth/domain → `application/problem+json` (`serpotter-auth`); product returns thiserror; MCP tool body is JSON-RPC after auth.
- **Env:** cargo does **not** load `.env` — `set -a; source .env; set +a`.

## ANTI-PATTERNS (THIS PROJECT)

- **Never** `git commit --no-verify` / hook bypass.
- **Never** emit xAI `tools.type=x_search` (grok2api rejects); social = empty tools + X-oriented prompt.
- **xAI always direct** — outbound proxy must not wrap xAI clients.
- **No custom CONNECT dialer** — only `reqwest::Proxy::all` + URL helpers.
- **No body.api_key** auth (headers only: Bearer then x-api-key).
- **No plaintext “production hardening” assumption** — keys/tokens plaintext at rest (personal-use threat model).
- **No CF Workers / Nitro / Agents SDK** patterns.
- Do not return research shape `{search, extracts}` — use `webResults` / `scrapedPages`.
- Do not put product Rust under `apps/`.
- Do not put auth/axum deps into `serpotter-product`.

## UNIQUE STYLES

- Brand strings: Serpotter in logs/docs; MCP health tool name is `health`.
- Dual CLI: `cargo run -p serpotter-api -- seed-token|seed-key` (no clap).
- Test providers pointed at `http://127.0.0.1:9` so auth/key-pool paths never hit network.
- Fixed test token literal `tok-validtokenfortest0000000000000000`.

## COMMANDS

```bash
# quality (matches CI rust job)
cargo test --workspace
cargo clippy --workspace -- -D warnings

# run
set -a; source .env; set +a
export ADMIN_SECRET=dev-admin
cargo run -p serpotter-api -- seed-token --name local
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"
cargo run -p serpotter-api

# admin SPA (Node 22.18+; Vite+ scripts — same path as CI/Docker)
cd web && npm i
npm run dev         # http://localhost:5173/
npm run typecheck   # tsc -b
npm run check       # vp check
npm run build       # tsc -b && vp build → dist/ (base '/', served at site root)

# container / compose
docker build -t serpotter .
docker compose up -d --build                                    # local build
export ADMIN_SECRET=change-me
docker compose -f docker-compose.prod.yml pull && \
  docker compose -f docker-compose.prod.yml up -d               # GHCR prod (amd64)
docker compose -f docker-compose.prod.yml run --rm --entrypoint serpotter-api \
  api seed-token --name local
```

## NOTES

- Schema readiness: `/ready` requires `schema_version >= EXPECTED_SCHEMA_VERSION` (**17**). v9 adds `api_keys.inflight` + `nodes.consecutive_fails`; v10 adds `nodes.lease_until` multi-hold reclaim; v11 adds `nodes.protocol` (http|https|socks5); v12 adds request_log observability columns (`request_id`, `token_name`, `strategy`, `providers_consulted`, `attempt_count`, `key_id`, `node_id`) + `idx_request_log_request_id`; v13 adds `idx_request_log_status` + `idx_request_log_path` for admin log filters; v14 adds `nodes.disabled_at` for auto-re-enable; v15 adds the B1 exact-query cache, B2 request_log token/cost columns (`input_tokens`/`output_tokens`/`total_tokens`/`cost_est`), and the B6 `usage_daily` rollup; v16 drops the dead `ttft_ms`/`request_mode` columns (the streaming /v1 wave they were placeholders for was never built); v17 drops `request_log` (raw events live only in the JSON log stream; admin browsing reads an in-memory 2,048-entry ring) and widens `usage_daily` with `key_id`/`token_name` (sentinels `0`/`''`) upserted at write time. Outbound Fixed env removed.
- Key pool: shared soft cap via `KEY_MAX_INFLIGHT` (3), wait `KEY_ACQUIRE_TIMEOUT_SECS` (30), hold reclaim `KEY_HOLD_TTL_SECS` (90). Pick: exhausted-last, score `(effective_C * 1000)/(inflight+1)` (`KEY_CREDIT_SCORE_SCALE`); NULL `credits_remaining` uses mid-weight `KEY_UNKNOWN_CREDIT_WEIGHT` (default 100). Success soft-burns non-NULL credits −1 (rank heuristic); credit sync overwrites SoT. Boot zeros key+node inflight (+ lease). `lease_until` is multi-hold reclaim deadline (not exclusive mutex). Empty/inactive inventory → fail-fast `NoHealthyKey` 503; active inventory all at cap through deadline → `KeyPoolError::AcquireTimeout` → product/API `KeyBusy` 503 (not the same tag as empty). Exclusive `acquire_api_key` / batch / `LEASE_TTL_SECS` removed — shared path only. Nodes: `NODE_HOLD_TTL_SECS` (90) stamps `nodes.lease_until` on acquire; reclaim expired on next acquire.
- Lease refresh (schema 16): long polls (structured extract, tavily research) re-stamp the held key + node `lease_until` every poll tick via `KeyHold::refresh` / `ProxyHold::refresh` → `KeyPool::refresh_hold` / `ProxyPool::refresh` (best-effort, no-op on released/absent ids), so multi-hold leases outlive the 90s hold TTL without re-acquire.
- Credit sync: admin `POST /api/keys/sync-credits` allowlist `tavily|firecrawl|exa|xai` (default both tavily+firecrawl); exa/xai honest soft-error only (no credit write). Optional cron when `CREDIT_SYNC_CRON=1` (off by default; tavily+firecrawl). Soft-fail (never deactivates on fetch error).
- Maintenance cron (15m): re-enable inactive keys after `KEY_REENABLE_AFTER_HOURS` (default 24) and disabled nodes after `NODE_REENABLE_AFTER_HOURS` (default 24); purge expired query-cache + `admin_sessions` rows; fire the high-error-rate alert from the in-memory events window (`ADMIN_ALERT_URL` webhook, optional); optional credit sync (above).
- Outbound: `ProxyPool` is **nodes-only** (least-inflight enabled `nodes` → direct); per product attempt; **xAI always direct**. Reqwest `Proxy::all` owns CONNECT tunnel from `nodes.protocol`. `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` **ignored**. `REQUIRE_OUTBOUND_PROXY=1` → 503 `NoHealthyNode` when no lease.
- Provider HTTP: connect **10s**, request **60s** on all clients (including xAI); proxy only on non-xAI.
- Request events: every product/MCP request funnels through `events::emit` (never fails the request path) — a structured tracing log line (`target: "request"`, the durable audit on stdout JSON logs), an in-memory ring entry (cap **2,048**; backs the admin `GET /api/request-logs` browser, newest-first, lost on restart), an error-window update (5-minute sliding window feeding the cron high-error-rate alert; in-memory, so a cold start never false-fires before `ALERT_MIN_TOTAL` requests accumulate), a metrics observation, and a usage delta to the single usage-writer task that upserts `usage_daily` at write time (drained on shutdown). Field semantics carried over from the old table: `request_id` (`x-request-id` inbound or minted), `token_name` (REST token row; MCP via `TokenRow` extension with `get_token_by_value` fallback), `strategy` (raw routed strategy), `providers_consulted` (comma-separated, first-seen, no spaces), `attempt_count`, `key_id` / `node_id` (sticky last **success** else last attempt); **`service` = vendor family** (first consulted vendor on dial labels, last attempted on bare errors; never hybrid/blend); **`provider_used` = dial label**. Admin filters: `limit` (default 50, clamp 1..=200), `status`, `path` (prefix), `service`, `requestId`, `token_name`. Ring/usage drops count via `serpotter_events_dropped_total` (loud — the audit line still landed).
- HTTP tracing: layer order (outermost first) `bound_request_id` (truncates inbound `x-request-id` to 64 bytes, pre-sets the extension) → `SetRequestIdLayer` (inbound wins else mints a 32-hex id) → `TraceLayer` (MakeSpan reads the extension; span fields `method`, `path`, `request_id`; headers never logged) → `PropagateRequestIdLayer` (copies the extension onto the response header). MakeSpan never mints a second ID. Details `docs/ops/env.md` / `docs/ops/api.md`.
- Ops knobs: env `LOG_FORMAT` (json|text), `ADMIN_SPA_DIR` (ServeDir at `/` + index.html fallback); code const `BODY_LIMIT_BYTES` = 2 MiB (not env); request id header `x-request-id` — details `docs/ops/env.md`.
- **Admin SPA:** Vite+ (`vite-plus` / `vp`); engines Node **22.18+** or ≥24.11; scripts `dev` / `typecheck` / `check` / `build` / `preview`. Image `admin-build` + CI admin job both use `npm run build` (no dual plain-vite path). Strict TS — zero `src/**/*.{js,jsx}`.
- Graceful shutdown: `axum::serve(...).with_graceful_shutdown(shutdown_signal())` on SIGINT/SIGTERM; maintenance task aborted after serve returns.
- Graceful shutdown is a **two-stage drain**: a ~20s cap is armed only after the signal fires (long-lived MCP SSE streams cannot stall exit); compose services set `stop_grace_period: 30s`.
- MCP tool errors: every tool failure returns ONE JSON text block `{"kind","message","requestId"}`; `kind` is the stable request-events tag (`ValidationError` for param failures).
- Provider allowlist: `serpotter_providers::PROVIDER_SERVICES` gates `seed-key` and admin create-key (unknown services → clear error).
- xAI domain filters: `allowed_domains`/`excluded_domains` max 5 per upstream docs; more → `ProviderError::Unsupported` (loud, never silent truncation).
- Credit sync: per-service DB errors are soft (warn + one error in the report) — never aborts the batch.
- **CI:** `.github/workflows/ci.yml` — rust (`test` + `clippy --locked`) + admin (Node 22.18, `npm ci` + `npm run build`); PR `docker-smoke`; main `publish` → `ghcr.io/jveko/serpotter` (`needs: [rust, admin]`).
- **Tags/dispatch:** `.github/workflows/docker-publish.yml` (no re-test); semver/`workflow_dispatch` only.
- **Docker:** multi-stage SPA + cargo-chef; runtime **serpotter uid 10001**; `ADMIN_SPA_DIR=/admin-dist` baked via `npm run build`; HEALTHCHECK `curl` `/ready`; default `DATABASE_URL=sqlite:/data/serpotter.db?mode=rwc`. Bind-mount hosts must allow uid 10001. See `docs/ops/deploy.md`.
- **Prod:** `docker compose -f docker-compose.prod.yml up -d` (standalone GHCR pull; no base `docker-compose.yml`).
- **MinHash deferred (D-f YAGNI):** URL-normalize + RRF only.
- Admin sessions (D3): argon2 in `admin_users`; `admin_sessions` 7d TTL (`adm-`). Bootstrap/login/logout; `require_admin`: session then ADMIN_SECRET.
- MCP Streamable HTTP via **rmcp** 3.x (dual-era): protocol **2026-07-28** is served **statelessly** (per-request `_meta` protocolVersion/clientCapabilities + `MCP-Protocol-Version`/`Mcp-Method`/`Mcp-Name` headers, `server/discover`; GET/DELETE → 405); older clients (≤ 2025-11-25) keep the legacy `initialize` → `Mcp-Session-Id` → GET SSE / DELETE → 202 session path via `LocalSessionManager` (keep-alive 1h). All `/mcp` methods require tok- auth; clients need `Accept: application/json, text/event-stream`; Host allowlist defaults loopback (`MCP_ALLOWED_HOSTS` for public; `MCP_ALLOWED_ORIGINS` for browser origins). Progress: MCP tools emit live `notifications/progress` (attempt/retry/fallback/phase) when the client sends `_meta.progressToken`; without a token responses stay plain JSON. Tools return typed `structuredContent` (camelCase) plus human text; `tools/list` advertises `outputSchema`; `health` stays text-only.
- Layout: product crate + api `admin/` `mcp/` `product/` modules; ops docs under `docs/ops/`.
