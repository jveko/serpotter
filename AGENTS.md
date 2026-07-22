# PROJECT KNOWLEDGE BASE

**Generated:** 2026-07-22  
**Commit:** 95ace51  
**Branch:** main

## OVERVIEW

Rust crates-only workspace rebrand of mysearch: multi-provider search proxy (Tavily/Firecrawl/Exa/xAI) + extract/research + lean MCP + admin API/SPA on a single VPS binary (`serpotter-api`), SQLite via sqlx.

## STRUCTURE

```
serpotter/
├── Cargo.toml              # workspace only (no root package)
├── crates/
│   ├── serpotter-api/      # sole binary + HTTP (axum)
│   ├── serpotter-core/     # pure: routing, RRF, types, URL normalize
│   ├── serpotter-db/       # sqlx pool + migrations (schema v8)
│   ├── serpotter-auth/     # tok-, extract, problem+json
│   ├── serpotter-keypool/  # in-process acquire/report over api_keys
│   ├── serpotter-providers/# Tavily/Firecrawl/Exa/xAI HTTP clients
│   └── serpotter-outbound/ # proxy URL helpers only (reqwest Proxy::all)
├── apps/admin/             # Vite React SPA (NOT a Cargo member)
├── docs/superpowers/       # SDD specs/plans
└── data/                   # gitignored SQLite default path
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| HTTP routes / AppState | `crates/serpotter-api/src/lib.rs` | `app()` registers all routes |
| Search + hybrid/blend | `crates/serpotter-api/src/search.rs` | uses core routing + providers |
| Extract / research REST | `crates/serpotter-api/src/extract.rs` | Research → `webResults`/`scrapedPages` |
| MCP JSON-RPC + Streamable subset | `crates/serpotter-api/src/mcp*.rs` | POST JSON-RPC default; `Mcp-Session-Id`; GET SSE; DELETE session |
| Admin CRUD | `crates/serpotter-api/src/admin.rs` | session Bearer (`adm-`) or `ADMIN_SECRET` / X-Admin-Password |
| Admin SPA (settings/nodes/playground) | `apps/admin/src/App.jsx` | prefers `serpotter_admin_session`; ADMIN_SECRET path kept; playground uses `tok-` |
| Process entry / CLI | `crates/serpotter-api/src/main.rs` | seed-token, seed-key, serve |
| 6-gate routing | `crates/serpotter-core/src/routing.rs` | free-fn `route_search` |
| RRF / dedupe | `crates/serpotter-core/src/pipeline.rs` | k=60, normalizeUrl keys |
| Wire DTOs | `crates/serpotter-core/src/types.rs` | REST camelCase |
| Migrations / schema | `crates/serpotter-db/migrations/` | SoT; `EXPECTED_SCHEMA_VERSION=8` |
| Provider HTTP | `crates/serpotter-providers/src/` | registry `with_proxy_url` |
| Outbound proxy URL | `crates/serpotter-outbound/src/lib.rs` | env then nodes table |
| Integration tests | `crates/serpotter-api/tests/health.rs` | axum oneshot, :9 providers |
| Design / plans | `docs/superpowers/` | foundation + wave plans |

## CODE MAP

| Symbol | Type | Location | Role |
|--------|------|----------|------|
| `app` | fn | `serpotter-api/src/lib.rs` | Router assembly + state |
| `AppState` | struct | same | db, keys, providers, admin_secret, mcp_sessions |
| `McpSessionStore` | struct | `api/src/mcp_session.rs` | process-local sessions; TTL 1h |
| `search_inner` / `run_provider` | fn | `api/src/search.rs` | shared by REST/MCP/research |
| `route_search` | fn | `core/src/routing.rs` | 6-gate provider decision |
| `reciprocal_rank_fusion` | fn | `core/src/pipeline.rs` | hybrid/blend merge |
| `connect_and_migrate` | fn | `db/src/lib.rs` | pool + embed migrations |
| `KeyPool` | struct | `keypool/src/lib.rs` | mutex + soft lease acquire_batch ≤10 |
| `ProviderRegistry` | struct | `providers/src/lib.rs` | search/extract dispatch |
| `generate_token` / `extract_token` | fn | `auth/src/lib.rs` | tok- + Bearer/x-api-key |
| `resolve_outbound_proxy_url` | fn | `outbound/src/lib.rs` | OUTBOUND_PROXY → URL |

Centrality unmeasured (no LSP/codegraph in session).

## CONVENTIONS

- **Crates-only Rust:** binary lives under `crates/serpotter-api` (lib+bin); never reintroduce `apps/*` Cargo packages.
- **Workspace deps only:** versions in root `[workspace.dependencies]`; members use `{ workspace = true }`.
- **REST/admin JSON:** `#[serde(rename_all = "camelCase")]`. MCP tool args: **snake_case preferred**, camelCase aliases.
- **Free-fns for pure logic** (routing, RRF, auth, outbound URL); stateful types: `Db`, `KeyPool`, `*Client`, `AppState`. No `dyn` trait objects in product path.
- **sqlx:** raw `query` + binds; migrations in `serpotter-db/migrations`; in-memory tests use `sqlite::memory:` with `max_connections=1`.
- **Errors:** REST auth/domain → `application/problem+json` (`serpotter-auth`); MCP tool body is JSON-RPC after auth.
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

## UNIQUE STYLES

- Brand strings: Serpotter in logs/docs; MCP tool still named `mysearch_health` for wire cutover.
- Dual CLI: `cargo run -p serpotter-api -- seed-token|seed-key` (no clap).
- Test providers pointed at `http://127.0.0.1:9` so auth/key-pool paths never hit network.
- Fixed test token literal `tok-validtokenfortest0000000000000000`.

## COMMANDS

```bash
# quality (manual — no CI yet)
cargo test --workspace
cargo clippy --workspace -- -D warnings

# run
set -a; source .env; set +a
export ADMIN_SECRET=dev-admin
cargo run -p serpotter-api -- seed-token --name local
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"
cargo run -p serpotter-api

# admin SPA
cd apps/admin && npm i && npm run dev
```

## NOTES

- Schema readiness: `/ready` requires `schema_version >= EXPECTED_SCHEMA_VERSION` (**8**).
- Soft lease: `api_keys.lease_until` + `LEASE_TTL_SECS=20`; acquire skips unexpired leases; report clears. Single-process mutex only (no multi-instance lease coordination).
- Credit sync: admin `POST /api/keys/sync-credits` (tavily/firecrawl) updates `credits_*`; soft-fail (never deactivates on fetch error).
- Maintenance cron (15m): re-enable inactive keys after `KEY_REENABLE_AFTER_HOURS` (default 24); purge `request_log` by `REQUEST_LOG_RETENTION_DAYS` (30) + `REQUEST_LOG_MAX_ROWS` (100000).
- Outbound priority: `OUTBOUND_PROXY` → `HTTPS_PROXY`/`HTTP_PROXY` → enabled `nodes` row → direct.
- No `.github` CI, justfile, or rust-toolchain pin yet — intentional greenfield.
- Deferred product depth: MinHash fuzzy dedupe (URL-normalize + RRF only — intentional YAGNI), full MCP progress/notifications (beyond Streamable subset).
- Admin sessions (D3): argon2 password hash in `admin_users`; sessions in `admin_sessions` (7d TTL, `adm-` tokens). `POST /api/admin/bootstrap` (empty users + ADMIN_SECRET), `/api/admin/login`, `/api/admin/logout`. `require_admin`: valid session Bearer first, then ADMIN_SECRET Bearer / X-Admin-Password. SPA: `serpotter_admin_session` preferred over `serpotter_admin_secret`.
- MCP Streamable HTTP subset: process-local sessions (`McpSessionStore`); TTL 1h; no multi-instance / Durable Objects. Dual-mode POST: session header optional for lean clients.
