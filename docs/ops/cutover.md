# Cutover (mysearch → serpotter)

For most clients, cutover is **base URL + existing token** only. Wire contracts stay frozen.

## Client change checklist

1. Point the HTTP base at serpotter (e.g. `https://search.example.com` → new host, or same host after swap).
2. Keep the same client token (`tok-…`) in `Authorization: Bearer` or `x-api-key`.
3. Keep JSON shapes and paths below — do not rename fields or MCP tools for “cleanup”.

No new auth scheme is required for product APIs. Admin may use `ADMIN_SECRET` or post-bootstrap `adm-` session tokens (optional SPA path).

## Frozen wire rules

| Surface | Rule |
| --- | --- |
| REST paths | `GET /live`, `GET /ready`, `POST /api/search`, `/api/extract`, `/api/research`, admin `/api/tokens|keys|settings|stats|nodes|…`, `POST /mcp` (+ GET SSE / DELETE session) |
| REST JSON | **camelCase** request/response bodies |
| Errors | Auth/domain → `application/problem+json` (`type` names such as `NoHealthyKey`, `KeyBusy`, `NoHealthyNode`, `ProviderError`, `SearchError`, `DatabaseError`, `ValidationError`) |
| Client auth | `Authorization: Bearer tok-…` then `x-api-key`; **no** `body.api_key` |
| Research shape | `webResults` / `scrapedPages` (not `{search, extracts}`) |
| MCP tools | `search`, `extract_url`, `research`, `mysearch_health` (legacy health name kept on purpose) |
| MCP transport | Streamable HTTP via **rmcp**; **all** `/mcp` methods need tok-; `Accept: application/json, text/event-stream`; stateful `Mcp-Session-Id` after initialize |
| MCP args | **snake_case preferred**, camelCase aliases accepted |
| Outbound | `OUTBOUND_PROXY` / env proxies / live `nodes` via `ProxyPool` → `reqwest::Proxy::all`; **xAI always direct**; no custom CONNECT dialer |
| Schema | SQLite migrations; readiness requires schema version **≥ 9** |
| `GET /ready` | **200** when ready: `{"status":"ready","schemaVersion":9,"expected":9}` (camelCase). **Not** mysearch snake_case `schema_version` or status `"ok"`. **503** uses `"status":"not_ready"` |

## What not to change during cutover

- Do not re-mint every client token unless rotating for security — existing `tokens` rows work if the DB volume is preserved.
- Do not require admin session for product traffic.
- Do not introduce multi-instance SQLite sharing without a separate storage plan (not in this stack).

## Smoke after swap

```bash
curl -fsS "$BASE/live"
curl -fsS "$BASE/ready"
curl -fsS -X POST "$BASE/api/search" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"query":"smoke","maxResults":3}'

# MCP (rmcp Streamable HTTP): Accept both JSON + SSE; initialize mints session
INIT=$(curl -fsS -D /tmp/mcp-headers -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cutover","version":"0.1.0"}}}')
SID=$(grep -i '^mcp-session-id:' /tmp/mcp-headers | awk '{print $2}' | tr -d '\r')
curl -fsS -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -H "mcp-session-id: $SID" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

Notes: response body may be SSE (`data: {…}`) rather than bare JSON. Public hosts must set `MCP_ALLOWED_HOSTS` (see [env.md](./env.md)).

Deploy steps: [deploy.md](./deploy.md). Env knobs: [env.md](./env.md).
