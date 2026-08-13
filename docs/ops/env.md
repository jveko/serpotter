# Environment

Cargo does **not** load `.env`. Export into the process:

```bash
set -a; source .env; set +a
```

Starter template: root [`.env.example`](../../.env.example).

## Core

| Variable | Default | Notes |
| --- | --- | --- |
| `DATABASE_URL` | host: `sqlite:data/serpotter.db?mode=rwc` · container: `sqlite:/data/serpotter.db?mode=rwc` | sqlx SQLite URL; `mode=rwc` creates file |
| `PORT` | `8080` | bind `0.0.0.0:PORT` |
| `RUST_LOG` | binary default `info,serpotter_api=debug` if unset; image `info,serpotter_api=info` | `tracing_subscriber` EnvFilter |
| `ENVIRONMENT` | `development` | logged at startup only |
| `ADMIN_SECRET` | unset | enables admin API via Bearer / `X-Admin-Password`; required for `POST /api/admin/bootstrap` when no users; session Bearer works without it after login |

## Outbound proxy (web providers only)

`ProxyPool` is **nodes-only**: each non-xAI product attempt acquires the least-inflight **enabled** `nodes` row (or dials direct when none). Reqwest owns the tunnel via `Proxy::all` (HTTP/HTTPS/SOCKS5 URLs from `nodes.protocol`). **No Fixed env mode** — `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` are **ignored** for Serpotter egress (breaking vs pre-v11); put proxies in admin **Nodes**.

| Variable | Default | Notes |
| --- | --- | --- |
| `REQUIRE_OUTBOUND_PROXY` | off | `1`/`true`/`yes` → **503 NoHealthyNode** when no enabled node lease. **xAI still direct**. |
| `NODE_HOLD_TTL_SECS` | `90` | Multi-hold reclaim for `nodes.lease_until`. Boot zeros inflight + lease. |


## Key pool (shared soft cap)

Product acquires one key hold per attempt (`KeyPool::acquire`). Concurrent holds on the same key are allowed up to a soft cap; waiters park until capacity frees or timeout.

| Variable | Default | Notes |
| --- | --- | --- |
| `KEY_MAX_INFLIGHT` | `3` | Soft cap of concurrent holds **per** `api_keys` row |
| `KEY_ACQUIRE_TIMEOUT_SECS` | `30` | Wall-clock wait when active keys exist but all at cap → then `KeyBusy` (503). Empty/inactive inventory fails fast as `NoHealthyKey` (503, no wait) |
| `KEY_HOLD_TTL_SECS` | `90` | Hold reclaim deadline stamped on `lease_until`; expired holds full-zero on next acquire path. Should be ≥ typical HTTP request timeout |
| `KEY_UNKNOWN_CREDIT_WEIGHT` | `100` | effective credit weight when `credits_remaining IS NULL` (Exa/xAI/unsynced). Used in pick score `(C * 1000) / (inflight + 1)`. Clamp ≥ 1. |

Boot zeros `api_keys.inflight` / `lease_until` and `nodes.inflight` / `lease_until` so orphan holds from a previous process do not block capacity.

Firecrawl upstream responses whose body matches permanent ban copy (`account has been banned`) cause an immediate hard DELETE of that `api_keys` row on search/extract (not fail@3 disable). Deleted keys cannot be selected by `KEY_REENABLE_AFTER_HOURS` re-enable.

**Multi-key pick:** active keys under cap are ordered exhausted-last, then `(effective_credits * 1000) / (inflight + 1)` DESC, then LRU. Successful holds soft-decrement non-NULL `credits_remaining` by 1 (rank heuristic; Tavily/Firecrawl sync overwrites). Soft −1 is not billing truth (Tavily advanced/research and Firecrawl multi-credit ops differ). Firecrawl usage residual is **team-wide** — multiple keys on one team each storing full remaining can overstate capacity. Tavily `GET /usage` is limited to **10 calls / 10 minutes** — avoid thrashing multi-key credit sync.

## Provider base URLs / model

| Variable | Default |
| --- | --- |
| `TAVILY_BASE_URL` | `https://api.tavily.com` |
| `FIRECRAWL_BASE_URL` | `https://api.firecrawl.dev` |
| `EXA_BASE_URL` | `https://api.exa.ai` |
| `XAI_BASE_URL` | `https://api.x.ai/v1` |
| `XAI_MODEL` | `grok-4.5` |

## Maintenance / retention

15-minute loop (`spawn_maintenance`): re-enable inactive keys, re-enable disabled outbound nodes, purge `request_log`, purge expired `admin_sessions`, optional credit sync.

| Variable | Default | Notes |
| --- | --- | --- |
| `KEY_REENABLE_AFTER_HOURS` | `24` | re-activate keys after consecutive-failure disable (does not apply to ban hard-deletes) |
| `NODE_REENABLE_AFTER_HOURS` | `24` | re-activate disabled outbound nodes (`nodes.disabled_at` stamp; clears fails/last_error) |
| `REQUEST_LOG_RETENTION_DAYS` | `30` | age-based purge |
| `REQUEST_LOG_MAX_ROWS` | `100000` | row-cap purge |
| `CREDIT_SYNC_CRON` | off | set `1` or `true` to sync Tavily/Firecrawl credits each tick (off by default) |

The loop also purges expired `admin_sessions` rows (`purge_expired_admin_sessions`) on the same
15-minute cadence — adm- sessions expire 7 days after login (no retention knob; the purge is
unconditional).

On-demand credit sync (no cron): `POST /api/keys/sync-credits` with admin auth.

**Exa / xAI credits:** Tavily and Firecrawl have product-key usage endpoints and write `credits_*`. Exa usage is under a service/admin key API, not the product search key — Serpotter does not invent a remaining/limit parser. xAI is console billing only; no stable public “remaining credits” API for product keys. Both stay soft-error (`errors++`, keys stay active, no credit write).

## HTTP client timeouts (code constants)

Not env: all provider clients use **connect 10s** and **request 60s** (`serpotter-providers` `HTTP_CONNECT_TIMEOUT` / `HTTP_REQUEST_TIMEOUT`).

Overall request deadline (env):

| Variable | Default | Notes |
| --- | --- | --- |
| `REQUEST_TIMEOUT_SECS` | `120` | wall-clock cap on each search/extract/research product call (REST 504 `RequestTimeout` / MCP `Timeout` envelope). Invalid value → warn + default |
| `CACHE_TTL_SECS` | `300` | B1 exact-query TTL response cache in seconds; `0` disables. Expired rows purged by the maintenance cron |
| `ADMIN_ALERT_URL` | unset | B15 optional webhook: POSTs `{errorRate, total, errors, ts}` when the 5-minute request-log error rate exceeds 50% with ≥ 20 requests |

## Process / HTTP hygiene

| Variable | Default | Notes |
| --- | --- | --- |
| `LOG_FORMAT` | unset (single-line text fmt) | `tracing_subscriber::fmt()` default (one line per event, no pretty-printing). Set `json` for structured JSON logs |
| `ADMIN_SPA_DIR` | unset | if set to a directory of built SPA assets, serves the console at the **site root** (`/`) via `ServeDir` registered as the router **fallback**. Real files (`/assets/*`) are served directly; anything else falls back to `index.html`, so refreshing a client route (`/stats`, `/keys`, …) boots the app instead of 404ing. Declared routes always win — `/api`, `/mcp`, `/live`, `/ready` are never shadowed, and unknown `/api` paths answer a JSON 404 rather than HTML. **Build with Vite+ `npm run build`** (default `base: '/'` — do not set a sub-path base, it breaks the fallback; engines Node **22.18+** or ≥24.11). **Container image default:** `/admin-dist` (SPA baked in multi-stage build via same `npm run build`). Host/dev: unset, or point at `apps/admin/dist` after build. Override bind-mount still supported. |

Inbound body limit is a **code constant** `BODY_LIMIT_BYTES` = 2 MiB (`DefaultBodyLimit`). Request ids: `x-request-id` set + propagated (`SetRequestIdLayer` / `PropagateRequestIdLayer`); the trace layer mints a **32-char lowercase hex id** from 16 random bytes when no inbound header exists, and bounded inbound values are truncated to 64 bytes (details in [api.md](./api.md) — tracing).

## Request log (admin)

`GET /api/request-logs` (admin auth, newest-first) accepts optional query filters:

| Param | Meaning |
| --- | --- |
| `limit` | max rows (default 50, clamped 1..=200) |
| `status` | exact HTTP status |
| `path` | path prefix (`path LIKE prefix%`) |
| `service` | vendor family (`tavily`/`firecrawl`/`exa`/`xai`; never hybrid/blend) |
| `requestId` | `x-request-id` value |

Retention is the maintenance cron above (`REQUEST_LOG_RETENTION_DAYS` / `REQUEST_LOG_MAX_ROWS`). Row fields and the metric matrix: [api.md](./api.md).

## MCP (rmcp Streamable HTTP)

| Variable | Default | Notes |
| --- | --- | --- |
| `MCP_ALLOWED_HOSTS` | unset | Comma-separated host or `host:port` for inbound `Host` allowlist. Unset = **loopback only** (`localhost`, `127.0.0.1`, `::1`). Set to empty string to disable allowlist (not recommended). Public VPS must list the public hostname. |
| `MCP_ALLOWED_ORIGINS` | unset | Comma-separated origins (scheme + host + port) for inbound `Origin` validation (2026-07-28 spec MUST when the header is present). Unset = rmcp default (validation disabled). Set for browser-origin clients, e.g. `https://app.example.com,http://localhost:5173`. |

Code constants (not env): legacy-session keep-alive **1h** (`MCP_SESSION_TTL_SECS`, `LocalSessionManager` — legacy clients ≤ 2025-11-25 only); session IDs are opaque UUIDs (not multi-instance). 2026-07-28 requests are **stateless** (no session): they must send `Accept: application/json, text/event-stream`, the `MCP-Protocol-Version` header, `Mcp-Method` (all) / `Mcp-Name` (`tools/call`), and per-request `_meta` with `io.modelcontextprotocol/protocolVersion` + `clientCapabilities`. tok- auth on **all** `/mcp` methods.

## CLI (not env)

```text
serpotter-api                         # serve
serpotter-api seed-token [--name N]   # print tok- secret once
serpotter-api seed-key --key K [--service tavily|firecrawl|exa|xai]
```
