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

`ProxyPool` decides once per product attempt (not frozen into provider clients at boot).
Reqwest owns the HTTP CONNECT tunnel via `Proxy::all` (no custom dialer).

Priority: non-empty `OUTBOUND_PROXY` → non-empty `HTTPS_PROXY` / `HTTP_PROXY` → least-inflight enabled `nodes` row → direct (unless fail-closed).

| Variable | Default | Notes |
| --- | --- | --- |
| `OUTBOUND_PROXY` | unset | Preferred explicit proxy URL for Tavily / Firecrawl / Exa (**Fixed** mode: never touch `nodes`) |
| `HTTPS_PROXY` / `HTTP_PROXY` | unset | Fallback if `OUTBOUND_PROXY` unset; same Fixed mode when non-empty |
| `REQUIRE_OUTBOUND_PROXY` | off | set `1`/`true`/`yes` → product returns **503 NoHealthyNode** when acquire yields no lease (empty/disabled nodes). Fixed env always has a lease. **xAI still direct**. |

Blank/whitespace env values fall through to live `nodes` / direct. **xAI always dials direct** (no proxy).

Admin can also set nodes via `/api/nodes` (SPA/API). Fixed env mode skips the table entirely.

## Key pool (shared soft cap)

Product acquires one key hold per attempt (`KeyPool::acquire`). Concurrent holds on the same key are allowed up to a soft cap; waiters park until capacity frees or timeout.

| Variable | Default | Notes |
| --- | --- | --- |
| `KEY_MAX_INFLIGHT` | `3` | Soft cap of concurrent holds **per** `api_keys` row |
| `KEY_ACQUIRE_TIMEOUT_SECS` | `30` | Wall-clock wait when active keys exist but all at cap → then `KeyBusy` (503). Empty/inactive inventory fails fast as `NoHealthyKey` (503, no wait) |
| `KEY_HOLD_TTL_SECS` | `90` | Hold reclaim deadline stamped on `lease_until`; expired holds full-zero on next acquire path. Should be ≥ typical HTTP request timeout |
| `NODE_HOLD_TTL_SECS` | `90` | Same multi-hold reclaim for `nodes.lease_until` (outbound ProxyPool Nodes mode). Boot zeros `nodes.inflight` + `lease_until`. |

Boot zeros `api_keys.inflight` / `lease_until` and `nodes.inflight` / `lease_until` so orphan holds from a previous process do not block capacity.

## Provider base URLs / model

| Variable | Default |
| --- | --- |
| `TAVILY_BASE_URL` | `https://api.tavily.com` |
| `FIRECRAWL_BASE_URL` | `https://api.firecrawl.dev` |
| `EXA_BASE_URL` | `https://api.exa.ai` |
| `XAI_BASE_URL` | `https://api.x.ai/v1` |
| `XAI_MODEL` | `grok-4.3` |

## Maintenance / retention

15-minute loop (`spawn_maintenance`): re-enable inactive keys, purge `request_log`, optional credit sync.

| Variable | Default | Notes |
| --- | --- | --- |
| `KEY_REENABLE_AFTER_HOURS` | `24` | re-activate keys after consecutive-failure disable |
| `REQUEST_LOG_RETENTION_DAYS` | `30` | age-based purge |
| `REQUEST_LOG_MAX_ROWS` | `100000` | row-cap purge |
| `CREDIT_SYNC_CRON` | off | set `1` or `true` to sync Tavily/Firecrawl credits each tick (off by default) |

On-demand credit sync (no cron): `POST /api/keys/sync-credits` with admin auth.

**Exa / xAI credits:** Tavily and Firecrawl have product-key usage endpoints and write `credits_*`. Exa usage is under a service/admin key API, not the product search key — Serpotter does not invent a remaining/limit parser. xAI is console billing only; no stable public “remaining credits” API for product keys. Both stay soft-error (`errors++`, keys stay active, no credit write).

## HTTP client timeouts (code constants)

Not env: all provider clients use **connect 10s** and **request 60s** (`serpotter-providers` `HTTP_CONNECT_TIMEOUT` / `HTTP_REQUEST_TIMEOUT`).

## Process / HTTP hygiene

| Variable | Default | Notes |
| --- | --- | --- |
| `LOG_FORMAT` | unset (pretty fmt) | set `json` for structured JSON logs via `tracing_subscriber` |
| `ADMIN_SPA_DIR` | unset | if set to a directory of built SPA assets, serves under `/admin/*` via `ServeDir`. **Build with Vite+ `npm run build`** (`base: '/admin/'`; engines Node **22.18+** or ≥24.11). **Container image default:** `/admin-dist` (SPA baked in multi-stage build via same `npm run build`). Host/dev: unset, or point at `apps/admin/dist` after build. Override bind-mount still supported. |

Inbound body limit is a **code constant** `BODY_LIMIT_BYTES` = 2 MiB (`DefaultBodyLimit`). Request ids: `x-request-id` set + propagated (`SetRequestIdLayer` / `PropagateRequestIdLayer` + UUID).

## MCP (rmcp Streamable HTTP)

| Variable | Default | Notes |
| --- | --- | --- |
| `MCP_ALLOWED_HOSTS` | unset | Comma-separated host or `host:port` for inbound `Host` allowlist. Unset = **loopback only** (`localhost`, `127.0.0.1`, `::1`). Set to empty string to disable allowlist (not recommended). Public VPS must list the public hostname. |

Code constants (not env): process-local `LocalSessionManager` keep-alive **1h** (`MCP_SESSION_TTL_SECS`); session IDs are opaque UUIDs (not multi-instance). Clients must send `Accept: application/json, text/event-stream` on POST; tok- auth on **all** `/mcp` methods.

## CLI (not env)

```text
serpotter-api                         # serve
serpotter-api seed-token [--name N]   # print tok- secret once
serpotter-api seed-key --key K [--service tavily|firecrawl|exa|xai]
```
