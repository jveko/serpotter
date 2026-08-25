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
| `GET/POST` | `/api/tokens`, `/api/keys`, `/api/nodes` | admin list/create |
| `DELETE` | `/api/tokens/{id}` | admin delete token (204/404) |
| `PUT/DELETE` | `/api/keys/{id}`, `/api/nodes/{id}` | admin update/delete (see below) |
| `POST` | `/api/keys/{id}/toggle`, `/api/nodes/{id}/toggle`, `/api/keys/sync-credits` | admin actions |
| `GET/PUT` | `/api/settings` · `GET` `/api/stats` · `GET` `/api/request-logs` · `GET` `/api/usage` · `GET` `/api/spend/{keys,services}` | admin views |
| `POST` | `/api/admin/bootstrap` | admin auth — create the argon2 admin user (409 `AlreadyBootstrapped` once one exists; requires `ADMIN_SECRET` when no users) |
| `POST` | `/api/admin/login` | admin auth — password → `adm-` session (7-day TTL) |
| `POST` | `/api/admin/logout` | admin auth — revoke the current `adm-` session |
| `POST` | `/mcp` | MCP Streamable HTTP (also GET SSE / DELETE session) |

- Request/response JSON: **camelCase**
- Domain/auth errors: `application/problem+json` (`type` names such as `NoHealthyKey`, `KeyBusy`, `NoHealthyNode`, `ProviderError`, `SearchError`, `DatabaseError`, `ValidationError`); product-mapped search/extract/research problems carry a machine-readable `retryable` extension member (`true` unless `type` is `ValidationError` — all 5xx/timeout kinds are transient)
- Upstream provider error messages carry **no vendor response text at all** —
  only the provider name, HTTP status, and neutral wording (`temporarily
  unavailable`, `rate-limited`, `upstream error (status N)`) — so agent
  consumers never see vendor wording (e.g. "key banned", account ids) that
  could derail execution or be read as permanent. The verbatim body is logged
  server-side at WARN (`reason=upstream_error` / `firecrawl_banned` /
  `research_poll`) in the JSON log stream — that stream is the only durable
  copy, so diagnose from there, not from client-facing detail.
- Research body uses `webResults` / `scrapedPages` (not `{search, extracts}`)

### Request bodies (product)

All three product endpoints take a **camelCase** JSON object. Fields marked
`list-or-one` accept either `"v"` or `["v1","v2"]`. Every field is optional
except `query` / `url`. Unknown routing values are rejected with `400
ValidationError` when they land outside the documented closed sets.

**`POST /api/search`** — `SearchQuery`:

| Field | Type | Notes |
| --- | --- | --- |
| `query` | string | **required** (non-empty; `"missing_query"` 400 otherwise) |
| `maxResults` | int | default 5, clamped `1..=20` |
| `mode` | string | `auto` (default) \| `web` \| `news` \| `social` \| `docs` \| `research` \| `github` \| `pdf` |
| `intent` | string | `auto` \| `factual` \| `status` \| `comparison` \| `tutorial` \| `exploratory` \| `news` \| `resource` |
| `strategy` | string | `auto` (default) \| `fast` \| `balanced` \| `verify` \| `deep` |
| `provider` | string | `auto` \| `tavily` \| `firecrawl` \| `exa` \| `xai` \| `social` \| `hybrid` |
| `sources` | list-or-one | source names, e.g. `["web","x"]` |
| `includeContent` | bool | request full content from the provider |
| `includeDomains` | list-or-one | web-only domain allowlist |
| `excludeDomains` | list-or-one | web-only domain blocklist |
| `allowedXHandles` | list-or-one | X/Twitter handles to include (social leg) |
| `excludedXHandles` | list-or-one | X/Twitter handles to exclude (social leg) |
| `fromDate` | string | ISO date lower bound |
| `toDate` | string | ISO date upper bound |
| `searchDepth` | string | `basic` \| `advanced` \| `fast` \| `ultra-fast` |
| `timeRange` | string | relative window (e.g. `week`) |
| `country` | string | country code |
| `exactMatch` | bool | exact-phrase match |

**`POST /api/extract`** — `ExtractRequest`:

| Field | Type | Notes |
| --- | --- | --- |
| `url` | string | **required** (non-empty; `"missing_url"` 400 otherwise) |
| `provider` | string | `firecrawl` \| `tavily` (unset/auto picks per routing; unknown value → `400 ValidationError`) |

**`POST /api/research`** — `ResearchRequest` (snake_case aliases accepted):

| Field | Type | Notes |
| --- | --- | --- |
| `query` | string | **required** (non-empty) |
| `webMaxResults` (alias `maxResults`) | int | default 5, clamped `1..=20` |
| `scrapeTopN` (aliases `extractTopN`, `extract_top_n`, `scrape_top_n`) | int | default 2, clamped `0..=10` (0 = no scrapes) |
| `includeContent` | bool | request full content |
| `socialMaxResults` (alias `social_max_results`) | int | default `0` = social leg skipped; when set, clamped `1..=10` |
| `includeDomains` | list-or-one | web-only |
| `excludeDomains` | list-or-one | web-only |
| `allowedXHandles` | list-or-one | social leg |
| `excludedXHandles` | list-or-one | social leg |
| `fromDate` | string | ISO date |
| `toDate` | string | ISO date |
| `timeRange` | string | relative window |
| `country` | string | country code |

### Admin updates (rotate / patch)

- `PUT /api/keys/{id}` — `{service?, key?}`, at least one required. Key rotation resets
  `consecutiveFails`; a `service` change clears the stored credit snapshot (`creditsRemaining` /
  `creditsLimit` / `usageSyncedAt`) so stale vendor numbers are never trusted. Response: the
  updated key row (masked, never the raw secret) or `404 NotFound`.
- `PUT /api/nodes/{id}` — `{host?, port?, protocol?, username?, password?}`, at least one
  required; `protocol` allowlisted to `http|https|socks5`. `username` / `password` are
  tri-state: absent = keep, explicit `null` = clear, string = set. Never touches
  enabled/inflight/failure state. Response: the updated node row or `404 NotFound`.

Validation failures answer `400 ValidationError` (`application/problem+json`); admin auth is
required (`ADMIN_SECRET` bearer or `adm-` session).


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
| Tool errors | one JSON text block `{"kind","message","requestId","retryable"}`; `kind` = stable request-events tag (`ValidationError` for param failures); `retryable` = `true` unless `kind` is `ValidationError` (all 5xx/timeout kinds are transient) |
| Progress | `notifications/progress` on SSE when the client sends `_meta.progressToken` (attempt/retry/fallback/phase lines); no token → plain JSON |
| Results | `structuredContent` carries the typed camelCase response object (plus human text block); `outputSchema` advertised for search/extract_url/research |
| Tool args | **snake_case preferred**, camelCase aliases accepted |
| Host | default loopback allowlist; public bind → set `MCP_ALLOWED_HOSTS` |
| Origin | validated when `MCP_ALLOWED_ORIGINS` set (spec MUST when present); unset = rmcp default (disabled) |
| Cancellation | client disconnect (stream close) cancels in-flight work → `499/Cancelled` request event |

## Outbound / providers

- Proxy: live enabled `nodes` (protocol http|https|socks5) → direct
- Tunnel: `reqwest::Proxy::all` only (no custom CONNECT dialer)
- **xAI always dials direct**
- Schema readiness: SQLite migrations; `/ready` needs schema version **≥ 17**

## Request logs

`GET /api/request-logs` (admin auth) — newest-first page of the **in-memory request-event ring** (cap **2,048**) as a JSON array (camelCase). Same query params/JSON surface as the old SQLite table: `limit` (default 50, clamped 1..=200), `status` (exact), `path` (prefix match), `service` (vendor family), `requestId`, `tokenName`. Entries are **lost on restart** — the durable audit is the stdout JSON log stream (`LOG_FORMAT=json`; one line per request, `target: "request"`).

Row fields (ring rows; nullable fields NULL when unknown):

| Field | Meaning |
| --- | --- |
| `id`, `createdAt`, `path`, `method`, `status` | base row |
| `durationMs` | handler wall-clock time |
| `errorKind` | typed error name when the request failed |
| `queryPreview` | truncated query/URL preview (120 chars) |
| `requestId` | `x-request-id` (inbound, capped at 64 bytes, or server-minted 32-hex) |
| `tokenName` | tok- token name (REST handler; MCP via `TokenRow` extension with DB lookup fallback) |
| `strategy` | raw routing strategy as routed — `auto`/`fast`/`balanced`/`verify`/`deep` (never the execution dial label; dial labels live in `providerUsed`). Matches `RouteDecision.strategy` |
| `providersConsulted` | comma-separated vendor list, first-seen order, no spaces |
| `attemptCount` | outbound provider attempts |
| `keyId` | sticky last **successful** key hold, else last attempt (NULL when none) |
| `nodeId` | sticky last **successful** node lease, else last attempt (NULL when none) |
| `service` | vendor family — first consulted vendor on dial labels, last attempted on bare errors; never `hybrid`/`blend` |
| `providerUsed` | dial label — strategy dial for search (`single` → that vendor) or research with `verify` → `blend-verify`; `hybrid`/`blend`/`verify` for multi |

`GET /api/usage` (`days` query param, default 14, clamped 1..=180 — the dashboard fetches `2×days` for its current+previous windows) and `GET /api/spend/{keys,services}` are populated **at write time** by the events usage writer into `usage_daily` (key/token dimensions via `key_id`/`token_name`, sentinels `0`/`''` when unknown) — there is no rollup job. `GET /api/stats` exposes the live ring length as `recentRequests`.

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
