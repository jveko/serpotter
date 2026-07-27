# API contract

Wire surface for product HTTP, admin, and MCP. Paths and JSON shapes are stable — change them only with an intentional client break.

## Auth

| Surface | How |
| --- | --- |
| Product + MCP | `Authorization: Bearer tok-…` or `x-api-key: tok-…` (headers only; no `body.api_key`) |
| Admin API | `ADMIN_SECRET` Bearer / `X-Admin-Password`, or `adm-` session after bootstrap/login |
| Admin SPA playground | product `tok-` for search/extract/research |

## REST

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/live` | process liveness |
| `GET` | `/ready` | schema ≥ expected → `{"status":"ready","schemaVersion":N,"expected":N}` (camelCase). Not ready → **503** `{"status":"not_ready",…}` |
| `POST` | `/api/search` | search |
| `POST` | `/api/extract` | URL extract |
| `POST` | `/api/research` | research (`webResults` / `scrapedPages`) |
| `*` | `/api/tokens`, `/api/keys`, `/api/settings`, `/api/stats`, `/api/nodes`, … | admin CRUD |
| `POST` | `/mcp` | MCP Streamable HTTP (also GET SSE / DELETE session) |

- Request/response JSON: **camelCase**
- Domain/auth errors: `application/problem+json` (`type` names such as `NoHealthyKey`, `KeyBusy`, `NoHealthyNode`, `ProviderError`, `SearchError`, `DatabaseError`, `ValidationError`)
- Research body uses `webResults` / `scrapedPages` (not `{search, extracts}`)

## MCP

| Item | Rule |
| --- | --- |
| Transport | Streamable HTTP (**rmcp**); process-local sessions |
| Auth | tok- on **all** `/mcp` methods |
| Accept | `application/json, text/event-stream` |
| Session | `Mcp-Session-Id` after `initialize` (opaque UUID) |
| Tools | `search`, `extract_url`, `research`, `health` |
| Tool args | **snake_case preferred**, camelCase aliases accepted |
| Host | default loopback allowlist; public bind → set `MCP_ALLOWED_HOSTS` |
| DELETE | **202** (not 204) |

## Outbound / providers

- Proxy: `OUTBOUND_PROXY` → `HTTPS_PROXY`/`HTTP_PROXY` → live enabled `nodes` → direct
- Tunnel: `reqwest::Proxy::all` only (no custom CONNECT dialer)
- **xAI always dials direct**
- Schema readiness: SQLite migrations; `/ready` needs schema version **≥ 10**

## Smoke

Optional host check (not CI — never run live vendor traffic in GitHub Actions):

```bash
export SERPOTTER_TOKEN=tok-...   # required; exit 2 if unset
# optional: BASE_URL=http://127.0.0.1:8080
./scripts/live-smoke.sh
```

Hits `GET /live`, `GET /ready`, `POST /api/search`, `POST /api/extract` (`https://example.com`), a small `POST /api/research`, then MCP `initialize` + `tools/list`. Non-2xx fails the script.

Manual curls:

```bash
curl -fsS "$BASE/live"
curl -fsS "$BASE/ready"
curl -fsS -X POST "$BASE/api/search" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"query":"smoke","maxResults":3}'

INIT=$(curl -fsS -D /tmp/mcp-headers -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1.0"}}}')
SID=$(grep -i '^mcp-session-id:' /tmp/mcp-headers | awk '{print $2}' | tr -d '\r')
curl -fsS -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -H "mcp-session-id: $SID" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

Response body may be SSE (`data: {…}`) rather than bare JSON.

Deploy: [deploy.md](./deploy.md). Env: [env.md](./env.md).
