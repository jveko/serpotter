# PROJECT KNOWLEDGE BASE

**Generated:** 2026-07-22  
**Branch:** main

## OVERVIEW

Rust crates-only workspace rebrand of mysearch: multi-provider search proxy (Tavily/Firecrawl/Exa/xAI) + extract/research + lean MCP + admin API/SPA on a single VPS binary (`serpotter-api`), SQLite via sqlx.

## STRUCTURE

```
serpotter/
├── Cargo.toml              # workspace only (no root package)
├── Dockerfile              # multi-stage; non-root uid 10001; HEALTHCHECK /ready; VOLUME /data
├── docker-compose.yml      # api + named volume + healthcheck
├── crates/
│   ├── serpotter-api/      # sole binary + thin axum shells (admin/ mcp/ product/)
│   ├── serpotter-product/  # pure orchestration: search/extract/research + DTOs + thiserror
│   ├── serpotter-core/     # pure: routing, RRF, types, URL normalize
│   ├── serpotter-db/       # sqlx pool + migrations (schema v8) multi-module
│   ├── serpotter-auth/     # tok-, extract, problem+json
│   ├── serpotter-keypool/  # in-process acquire/report over api_keys
│   ├── serpotter-providers/# Tavily/Firecrawl/Exa/xAI HTTP (connect 10s / timeout 60s)
│   └── serpotter-outbound/ # proxy URL helpers only (reqwest Proxy::all)
├── apps/admin/             # Vite React SPA (NOT a Cargo member)
├── docs/ops/               # deploy, env, cutover
├── docs/superpowers/       # SDD specs/plans
└── data/                   # gitignored SQLite default path (host)
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| HTTP routes / AppState | `crates/serpotter-api/src/lib.rs` | `app()` registers all routes; thin shells only |
| Product REST handlers | `crates/serpotter-api/src/product/` | `search.rs`, `extract.rs` → `serpotter_product::*` |
| Search / extract / research logic | `crates/serpotter-product/` | `ProductCtx`, DTOs, three thiserror enums; **no** auth/axum |
| MCP JSON-RPC + Streamable | `crates/serpotter-api/src/mcp/` | `mod`, `session`, `stream`; tools call product free-fns |
| Admin CRUD / sessions | `crates/serpotter-api/src/admin/` | keys, nodes, settings, tokens, stats, session |
| Admin SPA | `apps/admin/src/App.jsx` | `serpotter_admin_session` preferred; playground uses `tok-` |
| Process entry / CLI / shutdown | `crates/serpotter-api/src/main.rs` | seed-token, seed-key, serve + `with_graceful_shutdown` |
| Maintenance cron | `crates/serpotter-api/src/cron.rs` | 15m re-enable / purge / optional credit sync |
| 6-gate routing | `crates/serpotter-core/src/routing.rs` | free-fn `route_search` |
| RRF / dedupe | `crates/serpotter-core/src/pipeline.rs` | k=60, normalizeUrl keys |
| Wire DTOs (core search types) | `crates/serpotter-core/src/types.rs` | REST camelCase |
| Product DTOs / errors | `crates/serpotter-product/src/` | extract/research shapes + SearchExec/Extract/Research errors |
| Migrations / schema | `crates/serpotter-db/migrations/` | SoT; `EXPECTED_SCHEMA_VERSION=8` |
| Provider HTTP + timeouts | `crates/serpotter-providers/src/http.rs` | `HTTP_CONNECT_TIMEOUT=10s`, `HTTP_REQUEST_TIMEOUT=60s` |
| Outbound proxy URL | `crates/serpotter-outbound/src/lib.rs` | env then nodes table |
| Integration tests | `crates/serpotter-api/tests/` | `common` fixture + split suites; providers → `:9` |
| Ops | `docs/ops/` | deploy, env, cutover |
| Design / plans | `docs/superpowers/` | foundation + roadmap + restructure |

## CODE MAP

| Symbol | Type | Location | Role |
|--------|------|----------|------|
| `app` | fn | `serpotter-api/src/lib.rs` | Router assembly + state |
| `AppState` | struct | same | db, keys, providers, admin_secret, mcp_sessions |
| `ProductCtx` | struct | `serpotter-product` | db + keys + providers for product free-fns |
| `search_inner` / `extract_url` / `research_inner` | fn | `serpotter-product` | orchestration (REST + MCP) |
| `McpSessionStore` | struct | `api/src/mcp/session.rs` | process-local sessions; TTL 1h; max 10k |
| `route_search` | fn | `core/src/routing.rs` | 6-gate provider decision |
| `reciprocal_rank_fusion` | fn | `core/src/pipeline.rs` | hybrid/blend merge |
| `connect_and_migrate` | fn | `db/src/lib.rs` | pool + embed migrations |
| `KeyPool` | struct | `keypool/src/lib.rs` | mutex + soft lease acquire_batch ≤10 |
| `ProviderRegistry` | struct | `providers/src/lib.rs` | search/extract dispatch |
| `build_http` | fn | `providers/src/http.rs` | reqwest + 10s/60s (+ optional proxy) |
| `generate_token` / `extract_token` | fn | `auth/src/lib.rs` | tok- + Bearer/x-api-key |
| `resolve_outbound_proxy_url` | fn | `outbound/src/lib.rs` | OUTBOUND_PROXY → URL |
| `shutdown_signal` | fn | `api/src/main.rs` | Ctrl+C / SIGTERM → graceful serve stop |

## CONVENTIONS

- **Crates-only Rust:** binary under `crates/serpotter-api` (lib+bin); never reintroduce `apps/*` Cargo packages.
- **Product purity:** `serpotter-product` depends on core/db/keypool/providers only — **never** `serpotter-auth` or `axum`. Problem+json mapping stays in api shells.
- **Workspace deps only:** versions in root `[workspace.dependencies]`; members use `{ workspace = true }`.
- **REST/admin JSON:** `#[serde(rename_all = "camelCase")]`. MCP tool args: **snake_case preferred**, camelCase aliases.
- **Free-fns for pure logic** (routing, RRF, auth, outbound URL, product orchestration); stateful types: `Db`, `KeyPool`, `*Client`, `AppState`. No `dyn` trait objects in product path.
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

- Brand strings: Serpotter in logs/docs; MCP tool still named `mysearch_health` for wire cutover.
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

# admin SPA
cd apps/admin && npm i && npm run build   # CI admin job
cd apps/admin && npm i && npm run dev

# container / compose
docker build -t serpotter-api .
docker compose up -d --build
docker compose run --rm --entrypoint serpotter-api api seed-token --name local
```

## NOTES

- Schema readiness: `/ready` requires `schema_version >= EXPECTED_SCHEMA_VERSION` (**8**). Schema still **v8** after restructure (no new migrations).
- Soft lease: `api_keys.lease_until` + `LEASE_TTL_SECS=20` (constant); acquire skips unexpired leases; report clears. Single-process mutex only.
- Credit sync: admin `POST /api/keys/sync-credits` (tavily/firecrawl); optional cron when `CREDIT_SYNC_CRON=1` (off by default). Soft-fail (never deactivates on fetch error).
- Maintenance cron (15m): re-enable inactive keys after `KEY_REENABLE_AFTER_HOURS` (default 24); purge `request_log` by `REQUEST_LOG_RETENTION_DAYS` (30) + `REQUEST_LOG_MAX_ROWS` (100000); optional credit sync (above).
- Outbound priority: `OUTBOUND_PROXY` → `HTTPS_PROXY`/`HTTP_PROXY` → enabled `nodes` row → direct.
- Provider HTTP: connect **10s**, request **60s** on all clients (including xAI); proxy only on non-xAI.
- Graceful shutdown: `axum::serve(...).with_graceful_shutdown(shutdown_signal())` on SIGINT/SIGTERM; maintenance task aborted after serve returns.
- **CI:** `.github/workflows/ci.yml` — rust job (`test` + `clippy -D warnings`) and admin job (`npm ci` + `build`).
- **Docker:** multi-stage; runtime user **serpotter uid 10001**; `chown /data` before `USER`; HEALTHCHECK `curl` `/ready`; default `DATABASE_URL=sqlite:/data/serpotter.db?mode=rwc`. Bind-mount hosts must allow uid 10001. See `docs/ops/deploy.md`.
- **MinHash deferred (D-f YAGNI):** URL-normalize + RRF only.
- Admin sessions (D3): argon2 in `admin_users`; `admin_sessions` 7d TTL (`adm-`). Bootstrap/login/logout; `require_admin`: session then ADMIN_SECRET.
- MCP Streamable HTTP subset: process-local sessions (TTL 1h, max 10k); dual-mode POST; GET SSE; DELETE terminates.
- Restructure (2026-07-22): product crate + api `admin/` `mcp/` `product/` modules; ops docs under `docs/ops/`. Roadmap product waves R1–R3 + D1–D4 remain landed; restructure is layout/ops (see restructure design/plan).
