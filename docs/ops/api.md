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
| `*` | `/api/tokens`, `/api/keys`, `/api/settings`, `/api/stats`, `/api/nodes`, `/api/request-logs`, … | admin CRUD |
| `POST` | `/mcp` | MCP Streamable HTTP (also GET SSE / DELETE session) |

- Request/response JSON: **camelCase**
- Domain/auth errors: `application/problem+json` (`type` names such as `NoHealthyKey`, `KeyBusy`, `NoHealthyNode`, `ProviderError`, `SearchError`, `DatabaseError`, `ValidationError`)
- Research body uses `webResults` / `scrapedPages` (not `{search, extracts}`)

## MCP

Dual-era Streamable HTTP (**rmcp** 3.x): protocol **2026-07-28** is served
**statelessly** (per-request `_meta` + headers, `server/discover`); older
clients (≤ 2025-11-25) keep the legacy `initialize` → `Mcp-Session-Id` session
path on the same endpoint.

| Item | Rule |
| --- | --- |
| Transport | Streamable HTTP (**rmcp**); 2026-07-28 stateless + legacy sessions |
| Auth | tok- on **all** `/mcp` methods |
| Accept | `application/json, text/event-stream` |
| 2026-07-28 requests | every POST self-contained; `MCP-Protocol-Version` header + `_meta.io.modelcontextprotocol/protocolVersion` + `clientCapabilities` required; `Mcp-Method` on all, `Mcp-Name` on `tools/call` |
| Legacy requests | `initialize` → `Mcp-Session-Id` (opaque UUID); GET SSE stream + DELETE session (→ **202**) |
| Discovery | `server/discover` advertises `supportedVersions` + `capabilities.tools` |
| Tools | `search`, `extract_url`, `research`, `health` |
| Tool errors | one JSON text block `{"kind","message","requestId"}`; `kind` = stable request_log tag (`ValidationError` for param failures) |
| Progress | `notifications/progress` on SSE when the client sends `_meta.progressToken` (attempt/retry/fallback/phase lines); no token → plain JSON |
| Tool args | **snake_case preferred**, camelCase aliases accepted |
| Host | default loopback allowlist; public bind → set `MCP_ALLOWED_HOSTS` |
| Origin | validated when `MCP_ALLOWED_ORIGINS` set (spec MUST when present); unset = rmcp default (disabled) |
| Cancellation | client disconnect (stream close) cancels in-flight work → `499/Cancelled` log row |

## Outbound / providers

- Proxy: live enabled `nodes` (protocol http|https|socks5) → direct
- Tunnel: `reqwest::Proxy::all` only (no custom CONNECT dialer)
- **xAI always dials direct**
- Schema readiness: SQLite migrations; `/ready` needs schema version **≥ 12**

## Request logs

`GET /api/request-logs` (admin auth) — newest-first page of `request_log` as a JSON array (camelCase). Query params: `limit` (default 50, clamped 1..=200), `status` (exact), `path` (prefix match), `service` (vendor family), `requestId`.

Row fields (schema v12; new observability fields NULL when unknown):

| Field | Meaning |
| --- | --- |
| `id`, `createdAt`, `path`, `method`, `status` | base row |
| `durationMs` | handler wall-clock time |
| `errorKind` | typed error name when the request failed |
| `queryPreview` | truncated query/URL preview (120 chars) |
| `requestId` | `x-request-id` (inbound, capped at 64 bytes, or server-minted 32-hex) |
| `tokenName` | tok- token name (REST handler; MCP via `TokenRow` extension with DB lookup fallback) |
| `strategy` | raw routing strategy |
| `providersConsulted` | comma-separated vendor list, first-seen order, no spaces |
| `attemptCount` | outbound provider attempts |
| `keyId` | sticky last **successful** key hold, else last attempt (NULL when none) |
| `nodeId` | sticky last **successful** node lease, else last attempt (NULL when none) |
| `service` | vendor family — first consulted vendor on dial labels, last attempted on bare errors; never `hybrid`/`blend` |
| `providerUsed` | dial label — strategy dial for search (`single` → that vendor) or research with `verify` → `blend-verify`; `hybrid`/`blend`/`verify` for multi |

## Smoke

Optional host check (not CI — never run live vendor traffic in GitHub Actions):

```bash
export SERPOTTER_TOKEN=tok-...   # required; exit 2 if unset
# optional: BASE_URL=http://127.0.0.1:8080
./scripts/live-smoke.sh
```

Hits `GET /live`, `GET /ready`, `POST /api/search`, `POST /api/extract` (`https://example.com`), a small `POST /api/research`, then MCP `server/discover` + `tools/list`. Non-2xx fails the script.

Manual curls:

```bash
curl -fsS "$BASE/live"
curl -fsS "$BASE/ready"
curl -fsS -X POST "$BASE/api/search" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"query":"smoke","maxResults":3}'

# 2026-07-28 stateless: server/discover, then tools/list (no session).
curl -fsS -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -H "MCP-Protocol-Version: 2026-07-28" \
  -H "Mcp-Method: server/discover" \
  -d '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'
curl -fsS -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -H "MCP-Protocol-Version: 2026-07-28" \
  -H "Mcp-Method: tools/list" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'
```

Response body may be SSE (`data: {…}`) rather than bare JSON.

Legacy (≤ 2025-11-25) clients keep the session flow — `initialize` returns
`Mcp-Session-Id`, subsequent POSTs repeat it, GET opens an SSE stream, DELETE
terminates (202).

Deploy: [deploy.md](./deploy.md). Env: [env.md](./env.md).
