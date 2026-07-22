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
| Errors | Auth/domain → `application/problem+json` (`type` names such as `NoHealthyKey`, `ProviderError`, `SearchError`, `DatabaseError`, `ValidationError`) |
| Client auth | `Authorization: Bearer tok-…` then `x-api-key`; **no** `body.api_key` |
| Research shape | `webResults` / `scrapedPages` (not `{search, extracts}`) |
| MCP tools | `search`, `extract_url`, `research`, `mysearch_health` (legacy health name kept on purpose) |
| MCP args | **snake_case preferred**, camelCase aliases accepted |
| Outbound | `OUTBOUND_PROXY` / env proxies / `nodes` → `reqwest::Proxy::all`; **xAI always direct**; no custom CONNECT dialer |
| Schema | SQLite migrations; readiness requires schema version **≥ 8** |

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
curl -fsS -X POST "$BASE/mcp" \
  -H "Authorization: Bearer $TOKEN" \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

Deploy steps: [deploy.md](./deploy.md). Env knobs: [env.md](./env.md).
