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

Priority: `OUTBOUND_PROXY` → `HTTPS_PROXY` / `HTTP_PROXY` → first enabled `nodes` row → direct.

| Variable | Notes |
| --- | --- |
| `OUTBOUND_PROXY` | Preferred explicit proxy URL for Tavily / Firecrawl / Exa |
| `HTTPS_PROXY` / `HTTP_PROXY` | Fallback if `OUTBOUND_PROXY` unset |
| — | **xAI always dials direct** (no proxy) |

Admin can also set nodes via `/api/nodes` (SPA/API).

## Provider base URLs / model

| Variable | Default |
| --- | --- |
| `TAVILY_BASE_URL` | `https://api.tavily.com` |
| `FIRECRAWL_BASE_URL` | `https://api.firecrawl.dev` |
| `EXA_BASE_URL` | `https://api.exa.ai` |
| `XAI_BASE_URL` | `https://api.x.ai/v1` |
| `XAI_MODEL` | `grok-4.3` |

## Maintenance / lease / retention

15-minute loop (`spawn_maintenance`): re-enable inactive keys, purge `request_log`, optional credit sync.

| Variable | Default | Notes |
| --- | --- | --- |
| `KEY_REENABLE_AFTER_HOURS` | `24` | re-activate keys after consecutive-failure disable |
| `REQUEST_LOG_RETENTION_DAYS` | `30` | age-based purge |
| `REQUEST_LOG_MAX_ROWS` | `100000` | row-cap purge |
| `CREDIT_SYNC_CRON` | off | set `1` or `true` to sync Tavily/Firecrawl credits each tick (off by default) |

Soft key lease is **not** env-tunable: `LEASE_TTL_SECS = 20` in `serpotter-db` (clear on report). Single-process mutex only.

On-demand credit sync (no cron): `POST /api/keys/sync-credits` with admin auth.

## HTTP client timeouts (code constants)

Not env: all provider clients use **connect 10s** and **request 60s** (`serpotter-providers` `HTTP_CONNECT_TIMEOUT` / `HTTP_REQUEST_TIMEOUT`).

## MCP sessions (code constants)

Process-local: TTL **1h**, max **10_000** sessions, reap on create. Not multi-instance safe.

## CLI (not env)

```text
serpotter-api                         # serve
serpotter-api seed-token [--name N]   # print tok- secret once
serpotter-api seed-key --key K [--service tavily|firecrawl|exa|xai]
```
