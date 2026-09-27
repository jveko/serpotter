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
| `POST` | `/api/nodes/{id}/test` | admin — live connectivity probe through the node (10s budget); always **200** when the node exists (`{"ok":true,"latencyMs":N}` or `{"ok":false,"error":…}`), 404 when it does not |
| `GET/PUT` | `/api/settings` · `GET` `/api/stats` · `GET` `/api/request-logs` · `GET` `/api/usage` · `GET` `/api/spend/{keys,services}` | admin views |
| `POST` | `/api/admin/bootstrap` | admin auth — create the argon2 admin user (400 when `password` is shorter than 8 characters; 409 `AlreadyBootstrapped` once one exists; requires `ADMIN_SECRET` when no users) |
| `POST` | `/api/admin/login` | admin auth — password → `adm-` session (7-day TTL) |
| `POST` | `/api/admin/logout` | admin auth — revoke the current `adm-` session |
| `POST` | `/api/admin/change-password` | admin auth — `{currentPassword, newPassword}`; verifies the current password, stores the new argon2 hash and revokes every OTHER `adm-` session (the caller's survives). 401 wrong current; 400 blank/short (`< 8`) or same-as-current |
| `GET` | `/api/admin/sessions` | admin auth — active `adm-` sessions newest-first. Fields: `token` (**the full session token** — the only stable id; the SPA masks it via `tokenPreview` and revokes by this value), `tokenPreview`, `userId`, `expiresAt`, `createdAt`, `current` (true marks the caller's own when authz was a session, not an `ADMIN_SECRET` bearer). Password hashes are never returned |
| `DELETE` | `/api/admin/sessions/{id}` | admin auth — revoke one session by its raw token: **204** when revoked, **404 `NotFound`** for an unknown or blank id (a revoke that found nothing is a 404, not an idempotent 204). The trace span records the axum route template (or redacts the segment to `/api/admin/sessions/[REDACTED]` for an unmatched path), so the session token never reaches the durable log stream |
| `GET` | `/metrics` | Prometheus text exposition — **behind the same admin gate** as every `/api` admin route (`ADMIN_SECRET` or `adm-` session); request counters by service/status class, duration histogram, in-flight gauge, key-pool depth, cache hit/miss |
| `POST` | `/mcp` | MCP Streamable HTTP (also GET SSE / DELETE session) |

- Request/response JSON: **camelCase**
- Domain/auth errors: `application/problem+json` (`type` names such as `NoHealthyKey`, `KeyBusy`, `NoHealthyNode`, `ProviderError`, `SearchError`, `DatabaseError`, `ValidationError`); product-mapped search/extract/research problems carry a machine-readable `retryable` extension member (`false` for exactly two kinds — `ValidationError`, a client-side shape failure, and `DatabaseError`, our own storage fault; every vendor/capacity/timeout kind is transient). A `DatabaseError` `detail` is the fixed string `internal storage error`; the driver text is logged server-side and never returned.
- Upstream provider error messages carry **no vendor response text at all** —
  only the provider name, HTTP status, and neutral wording (`temporarily
  unavailable`, `rate-limited`, `upstream error (status N)`) — so agent
  consumers never see vendor wording (e.g. "key banned", account ids) that
  could derail execution or be read as permanent. The verbatim body is logged
  server-side at WARN (`reason=upstream_error` / `account_banned` /
  `research_poll`) in the JSON log stream — that stream is the only durable
  copy, so diagnose from there, not from client-facing detail. Vendor bans:
  a Firecrawl signature match hard-deletes the key row (`disposition=deleted`);
  a Tavily exact deactivation match suspends it (`disposition=suspended`);
  generic ban-wording on any other provider also suspends the row and stamps
  `disabled_reason = 'vendor_suspended'`, which the `KEY_REENABLE_AFTER_HOURS`
  cron skips (schema 18) — a vendor-deactivated account stays out of rotation
  until an operator re-enables it.
- Research body uses `webResults` / `scrapedPages` (not `{search, extracts}`)

### Request bodies (product)

All three product endpoints take a **camelCase** JSON object; every multi-word
field additionally accepts its `snake_case` spelling (both surfaces take either,
so a mistyped case can no longer drop a filter silently). Responses are always
camelCase. Fields marked
`list-or-one` accept either `"v"` or `["v1","v2"]`. Every field is optional
except `query` / `url`. Unknown routing values are rejected with `400
ValidationError` when they land outside the documented closed sets. Domain
filters must be bare hostnames: a client that sends the whole list as one string
(`"[\"a.com\",\"b.com\"]"`) is split into real entries, a `"https://x.com/p"` is
coerced to `x.com`, and anything that cannot become a hostname is a
`400 ValidationError` raised **before any key is leased**. `country` is
validated per vendor — Tavily takes only its documented country names (an ISO-2
code like `ID` is rewritten to `indonesia`; a name it does not support is
refused locally instead of drawing a vendor `400`), Firecrawl takes the
uppercase code form and forwards anything unmappable untouched. A provider that
will not accept a vendor-specific knob is skipped, and the chain continues.

**`POST /api/search`** — `SearchQuery`:

| Field | Type | Notes |
| --- | --- | --- |
| `query` | string | **required** (non-empty; `"missing_query"` 400 otherwise) |
| `maxResults` | int | default 5, clamped `1..=20` |
| `mode` | string | `auto` (default) \| `web` \| `news` \| `social` \| `docs` \| `research` \| `github` \| `pdf` |
| `intent` | string | `auto` \| `factual` \| `status` \| `comparison` \| `tutorial` \| `exploratory` \| `news` \| `resource` |
| `strategy` | string | `auto` (default) \| `fast` \| `balanced` \| `verify` \| `deep` |
| `provider` | string | `auto` \| `tavily` \| `firecrawl` \| `exa` \| `xai` \| `social` \| `hybrid` — heads the routing chain, it does not pin it: if that provider fails or refuses the request the chain still falls back to others (`providerUsed` reports who actually served it) |
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
| `provider` | string | `firecrawl` (default) \| `tavily` \| `exa` (auto = firecrawl first, then tavily; unknown value → `400 ValidationError`) |
| `urls` | list | **B26 batch**: when non-empty, `url` is ignored and every entry is extracted in one vendor call (tavily/exa backends). Response gains `pages[]`; top-level `url`/`content` mirror the first page |
| `format` | string | `markdown` \| `text` \| `question` \| `highlights`; absent = plain scrape/chain. Folded case/space-insensitively; unknown value → `400 ValidationError`. **Batch-only vs single-URL split below.** |
| ↳ `question` / `highlights` | string | single-URL modes — `question` = firecrawl (needs the `question` field), `highlights` = exa. Refused `400 ValidationError` alongside `urls` (batch) |
| ↳ `markdown` / `text` | string | select Tavily's `/extract` wire format on the **batch** path only (`TavilyClient::extract_batch`; exa batch ignores them, `provider=firecrawl` + `urls` is refused). On a **single URL** they are accepted but not dispatched on — the plain chain runs, so a single-URL `markdown` still dials firecrawl first by default |
| `question` | string | the question to answer from the single `url`; requires `format=question` (firecrawl) |
| `prompt` | string | structured extraction: natural-language instruction for what to extract. Needs firecrawl (or auto → firecrawl), the only structured backend |
| `schema` | JSON | structured extraction: the schema the result must conform to. Same provider rule as `prompt` |
| `outputSchema` (alias `output_schema`) | JSON | alias of `schema` on the extract surface |

**`POST /api/research`** — `ResearchRequest` (snake_case aliases accepted):

| Field | Type | Notes |
| --- | --- | --- |
| `query` | string | **required** (non-empty) |
| `webMaxResults` (alias `maxResults`) | int | default 5, clamped `1..=20` |
| `scrapeTopN` (aliases `extractTopN`, `extract_top_n`, `scrape_top_n`) | int | default 2, clamped `0..=10` (0 = no scrapes). `deep: true` runs a smaller per-pass budget: values above 6 are clamped to 6 and the clamp is reported as a note in `evidence.webLegErrors`; 0 and `1..=6` are honored exactly |
| `includeContent` | bool | request full content. `deep: true` **refuses** it with `400 ValidationError` (the deep loop's search legs are always contentless and its scrapes always full, so neither value can be honored) |
| `socialMaxResults` (alias `social_max_results`) | int | default `0` = social leg skipped; when set, clamped `1..=10` |
| `includeDomains` | list-or-one | web-only |
| `excludeDomains` | list-or-one | web-only |
| `allowedXHandles` | list-or-one | social leg |
| `excludedXHandles` | list-or-one | social leg |
| `fromDate` | string | ISO date |
| `toDate` | string | ISO date |
| `timeRange` | string | relative window |
| `country` | string | country code |
| `deep` | bool | run the iterative deep-research loop (2-pass search → scrape → xAI synthesis, bounded by the request deadline). Never cached. `deep: true` **refuses** `researchBackend`, `citationFormat`, `socialMaxResults > 0` and `includeContent` with `400 ValidationError` naming the dropped knob; `scrapeTopN > 6` is clamped (see its row), and dropped social/handle input plus the clamp are reported as notes in `evidence.webLegErrors` |
| `researchBackend` (alias `research_backend`) | string | `serpotter` (default; the multi-leg web+scrape+social / deep loop) \| `tavily` (one Tavily `/research` job polled synchronously — answer + citations in `evidence`/`citations`). Closed set; unknown value → `400 ValidationError` |
| `citationFormat` (alias `citation_format`) | string | Tavily research citations: `numbered` \| `mla` \| `apa` \| `chicago`. **Absent = Tavily's own default** (nothing is sent). Forwarded to Tavily only on the `researchBackend=tavily` path; cosmetic on the serpotter path (its citations already exist and are not reformatted). Unknown value → `400 ValidationError` |
| `outputSchema` (alias `output_schema`) | JSON | schema the synthesized answer should conform to. Best-effort: consumed by the deep-research xAI synthesis; standard research leaves existing answers as-is |

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
| Tool errors | one JSON text block in `content` `{"kind","message","requestId","retryable"}`, `isError: true`, **no `structuredContent`** (the advertised `outputSchema` is the success response type, so an envelope there would fail client-side schema validation); `kind` = stable request-events tag (`ValidationError` for param failures); `retryable` = `false` for the faults a retry cannot fix — `ValidationError` (client shape), `DatabaseError` (our storage), `NotReady` (deployment: only a migration fixes a schema-behind server) — and `true` for every other kind (`KeyBusy`, `Timeout`, `Cancelled`, and the vendor/capacity kinds) |
| Progress | `notifications/progress` on SSE when the client sends `_meta.progressToken` (attempt/retry/fallback/phase lines); no token → plain JSON |
| Results | success: `structuredContent` carries the typed camelCase response object matching the advertised `outputSchema` (plus a human text block); `outputSchema` advertised for search/extract_url/research. Failure: the envelope text block in `content` only, no `structuredContent` — see *Tool errors* |
| Tool args | **snake_case preferred**, camelCase aliases accepted |
| Host | default loopback allowlist; public bind → set `MCP_ALLOWED_HOSTS` |
| Origin | validated when `MCP_ALLOWED_ORIGINS` set (spec MUST when present); unset = rmcp default (disabled) |
| CORS | `MCP_ALLOWED_ORIGINS` also drives the `Access-Control-*` headers, so a browser preflight (`OPTIONS`) is answered from the allowlist **without a token** (never a 401) and returns 2xx. An allowlisted origin gets `Access-Control-Allow-Origin` on real responses plus `Access-Control-Expose-Headers` for `Mcp-Session-Id`, `MCP-Protocol-Version`, `Last-Event-Id`, `x-request-id` — a browser must be able to *read* the session id it then echoes on every session-scoped call. A foreign origin gets no `Access-Control-Allow-Origin`. `Access-Control-Allow-Headers` mirrors what the request asked for, since `Mcp-Param-*` is a prefix family no list can express; the origin allowlist is the security boundary. Entries are lowercased and default ports stripped so they match what a browser sends — **write non-default ports exactly (`http://localhost:5173`) and omit `:80`/`:443`**: a configured port must match exactly, and browsers omit default ports. `*` is rejected with a warning, never treated as allow-any. Unset/empty allowlist → no `Access-Control-*` header at all (browsers still cannot call `/mcp`); `Vary: origin, access-control-request-method, access-control-request-headers` is present on every `/mcp` response either way |
| Cancellation | client disconnect (stream close) cancels in-flight work → `499/Cancelled` request event |
| Session ownership | a legacy `Mcp-Session-Id` is bound to the token that created it, for as long as the session lives (rmcp's keep-alive is a sliding 1 h idle window). POST/GET/DELETE from a different token answers **404**, byte-identical to the response an unknown id gets (no `Content-Type`), so the header is not an existence oracle for other tenants' handles |
| Per-token cap | `search`/`extract_url`/`research` allow **8 concurrent in-flight calls per token**. Over the cap the call is **refused, never queued**: a retryable `KeyBusy` envelope (`retryable: true`) plus a 503 request row, decided *before* any progress delivery task is spawned and before any vendor spend. Free-form `KeyBusy` from the key pool (a different condition) shares the kind but not the admission message |
| `health` | ready → success body `{status, schemaVersion, expected}`. A schema behind the build → `NotReady` envelope (`retryable: false`, 503 row): only a migration fixes it. A storage fault → `DatabaseError` envelope with a generic detail (`retryable: false`, 500 row; driver text stays server-side). Every path emits a `/mcp/health` request row, so the one tool an operator uses to detect an outage is never the one that leaves no trace |

## Outbound / providers

- Proxy: live enabled `nodes` (protocol http|https|socks5) → direct
- Tunnel: `reqwest::Proxy::all` only (no custom CONNECT dialer)
- **xAI always dials direct**
- Schema readiness: SQLite migrations; `/ready` needs schema version **≥ 20**

## Query operators

`query` is forwarded to the search vendor verbatim; serpotter implements no
operator parsing. Measured 2026-09-09 with a `site:` control (a domain with no
indexable content, versus the same query without the operator):

- **Tavily — honors `site:`.** `site:pythonguis.com …` returned 3/3 on that host,
  `site:example.org …` returned 0 results, the bare query returned 3. The echoed
  `query` comes back with the token stripped, and `tavily.rs` takes `query` from
  the vendor's own response body — so the parse is Tavily's.
- **Exa — honors `site:`.** Bare control returned results, the same text with
  `site:example.org` returned 0. (Exa keeps the token in the echoed query, so the
  differential, not the echo, is the evidence.)
- **Firecrawl — unverified.** Both pinned attempts answered from another provider
  (`providerUsed` was `tavily`/`exa` under `reason: "single firecrawl"`), so this
  says nothing about Firecrawl's behavior — and it is a reminder that pinning
  heads the chain rather than isolating a vendor.

`include_domains` is the supported, enforced-by-serpotter form: it is applied on
every leg, so it is the only spelling that constrains a hybrid/blend merge. A
`site:` token is only as strong as the vendor that happens to answer.

## Request logs

`GET /api/request-logs` (admin auth) — newest-first page of the **in-memory request-event ring** (cap **2,048**) as a JSON array (camelCase). Query params: `limit` (default 50, clamped 1..=200), `offset` (default 0, floored at 0 — row-skip for paging; the response is a bare array with no total count, so page until a short page comes back), `status` (numeric; a non-numeric value such as `"2xx"` is treated as absent rather than a 400, so a dashboard can pass raw input through), `path` (prefix match), `service` (vendor family), `requestId`, `tokenName`, `errorKind` (exact; only failed rows carry a value). Entries are **lost on restart** — the durable audit is the stdout JSON log stream (`LOG_FORMAT=json`; one line per request, `target: "request"`).

Per-attempt evidence (failure funnel) — the four fields below are derived in the API layer from the product's `ExecMeta` attempt/transition records and serialized into ring rows ONLY (the durable audit line is unchanged: it already carries `error_kind` and `attempt_count`). `attemptOutcomes` is `service:outcome[:upstreamStatus]` per **completed** attempt, comma-joined, the status segment omitted when the attempt saw none (`"tavily:auth_invalid:401,firecrawl:ok"`). `lastUpstreamStatus` is the upstream status of the last attempt that reported one. `keyIds` lists every distinct key id the request attempted (first-seen order), falling back to the single sticky `keyId` when no attempt completed. `keyTransitions` is `service:transition:keyId` per key-state transition (`"tavily:disabled:9046"`) — it, not `keyId`, names the key the pool actually dropped. All four are omitted when empty; the `lastUpstreamStatus` query param (numeric, lenient like `status`; while set it EXCLUDES rows that never reached an upstream — transport failure, cache hit) filters on the same value.

Every admin body/query/path rejection answers the same RFC 9457 `application/problem+json` as the handlers: malformed body → 400 `InvalidJson`, wrong shape → 422 `InvalidJson`, missing/non-JSON `Content-Type` → 415 `InvalidContentType`, body over `BODY_LIMIT_BYTES` → 413 `BodyTooLarge`, unparseable query → 400 `InvalidQuery`, unparseable path segment → 400 `InvalidPath`.

Row fields (ring rows; nullable fields NULL when unknown):

| Field | Meaning |
| --- | --- |
| `id`, `createdAt`, `path`, `method`, `status` | base row |
| `durationMs` | handler wall-clock time |
| `errorKind` | typed error name when the request failed |
| `queryPreview` | truncated query/URL preview (120 chars) |
| `requestId` | `x-request-id` (inbound, capped at 64 bytes, or server-minted 32-hex) |
| `tokenName` | tok- token name (REST handler; MCP via `TokenRow` extension with DB lookup fallback) |
| `inputTokens` / `outputTokens` / `totalTokens` | provider-reported token counts (NULL when unknown) |
| `costEst` | estimated request cost |
| `cacheHit` | whether the response came from the in-process response cache (always present) |
| `strategy` | raw routing strategy as routed — `auto`/`fast`/`balanced`/`verify`/`deep` (never the execution dial label; dial labels live in `providerUsed`). Matches `RouteDecision.strategy` |
| `providersConsulted` | comma-separated vendor list, first-seen order, no spaces |
| `attemptCount` | outbound provider attempts |
| `keyId` | sticky last **successful** key hold, else last attempt (NULL when none) |
| `nodeId` | sticky last **successful** node lease, else last attempt (NULL when none) |
| `service` | vendor family — first consulted vendor on dial labels, last attempted on bare errors; never `hybrid`/`blend` |
| `providerUsed` | dial label — strategy dial for search (`single` → that vendor) or research with `verify` → `blend-verify`; `hybrid`/`blend`/`verify` for multi |
| `attemptOutcomes` | `service:outcome[:upstreamStatus]` per completed attempt, comma-joined (NULL when none) |
| `lastUpstreamStatus` | upstream status of the last attempt that reported one (NULL when no attempt did) |
| `keyIds` | distinct key ids attempted, first-seen order, comma-joined (falls back to `keyId`) |
| `keyTransitions` | `service:transition:keyId` per key-state transition, comma-joined (NULL when none) |

`GET /api/usage` (`days` query param, default 14) and `GET /api/spend/{keys,services}` (`days`, default 90) share one bound: `days` is clamped to `1..=180` (`serpotter_db::USAGE_MAX_DAYS`) in both the API handlers and the DB layer, so a requested window is never silently truncated — the dashboard fetches `2×days` for its current+previous windows and the 90d setting genuinely reaches day 180. The spend endpoints are additionally capped at `SPEND_MAX_ROWS` grouped rows (top spenders first) because `usage_daily` has no retention job. All three are populated **at write time** by the events usage writer into `usage_daily` (key/token dimensions via `key_id`/`token_name`, sentinels `0`/`''` when unknown) — there is no rollup job. `GET /api/stats` exposes the live ring length as `recentRequests`.

Admin write inputs are bounded: `name` / `key` / `host` / node credentials over **256 characters** are rejected with 400 `ValidationError` rather than stored, and a node `host` must be a DNS name (single-label names like `localhost` included), an IPv4 literal, or a bracketed IPv6 literal — it is interpolated raw into the `{protocol}://[user:pass@]host:port` proxy URL, so a scheme, port, path, or credential smuggled in there is now a 400 at create time instead of a per-request dial failure. Admin `DatabaseError` responses carry the fixed detail `internal storage error` (the real driver text is logged server-side), exactly like the product path.

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
