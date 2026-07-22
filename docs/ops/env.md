# Environment

Cargo does **not** load `.env`. Export into the process:

```bash
set -a; source .env; set +a
```

See root `.env.example` for a starter template.

## Core

| Variable | Default | Notes |
| --- | --- | --- |
| `DATABASE_URL` | host: `sqlite:data/serpotter.db?mode=rwc` · container: `sqlite:/data/serpotter.db?mode=rwc` | sqlx SQLite URL; `mode=rwc` creates file |
| `PORT` | `8080` | bind `0.0.0.0:PORT` |
| `RUST_LOG` | binary default filter `info,serpotter_api=debug` if unset; image `info,serpotter_api=info` | `tracing_subscriber` EnvFilter |
| `ENVIRONMENT` | `development` | logged at startup only |
| `ADMIN_SECRET` | unset | enables admin API via Bearer / `X-Admin-Password`; required for `POST /api/admin/bootstrap` when no users; session Bearer works without it after login |

## Outbound proxy (web providers only)

`ProxyPool` decides once per product attempt (not frozen into provider clients at boot).

Priority: non-empty `OUTBOUND_PROXY` → non-empty `HTTPS_PROXY` / `HTTP_PROXY` → least-inflight enabled `nodes` row → direct.

| Variable | Default | Notes |
| --- | --- | --- |
| `OUTBOUND_PROXY` | unset | Preferred explicit proxy URL for Tavily / Firecrawl / Exa (**Fixed** mode: never touch `nodes`) |
| `HTTPS_PROXY` / `HTTP_PROXY` | unset | Fallback if `OUTBOUND_PROXY` unset; same Fixed mode when non-empty |
| — | — | Blank/whitespace env values fall through to live `nodes` / direct |
| — | — | **xAI always dials direct** (no proxy) |

Admin can also set nodes via `/api/nodes` (SPA/API). Fixed env mode skips the table entirely.

## Key pool (shared soft cap)

Product acquires one key hold per attempt (`KeyPool::acquire`). Concurrent holds on the same key are allowed up to a soft cap; waiters park until capacity frees or timeout.

| Variable | Default | Notes |
| --- | --- | --- |
| `KEY_MAX_INFLIGHT` | `3` | Soft cap of concurrent holds **per** `api_keys` row |
| `KEY_ACQUIRE_TIMEOUT_SECS` | `30` | Wall-clock wait when active keys exist but all at cap → then `NoHealthyKey` (503). Empty/inactive inventory fails fast (no wait) |
| `KEY_HOLD_TTL_SECS` | `90` | Hold reclaim deadline stamped on `lease_until`; expired holds full-zero on next acquire path. Should be ≥ typical HTTP request timeout |

Boot zeros `api_keys.inflight` / `lease_until` and `nodes.inflight` so orphan holds from a previous process do not block capacity.

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

Shared key holds and outbound node inflight are env-tunable above (`KEY_*`). Legacy exclusive `LEASE_TTL_SECS = 20` remains only for non-product `acquire_api_key` paths in `serpotter-db`.

On-demand credit sync (no cron): `POST /api/keys/sync-credits` with admin auth.

## HTTP client timeouts (code constants)

Not env: all provider clients use **connect 10s** and **request 60s** (`serpotter-providers` `HTTP_CONNECT_TIMEOUT` / `HTTP_REQUEST_TIMEOUT`).

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
