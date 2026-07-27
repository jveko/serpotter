# Serpotter

Multi-provider search proxy in Rust: search, extract, research, MCP tools, and an admin API + SPA. One binary (`serpotter-api`), SQLite via sqlx.

Providers: **Tavily**, **Firecrawl**, **Exa**, **xAI**.

## Features

- **Search** with 6-gate routing, hybrid RRF merge, and fallback chains
- **Extract** URL content across providers
- **Research** (web scrape + optional social/xAI)
- **MCP** Streamable HTTP (`search`, `extract_url`, `research`, `mysearch_health`)
- **Key pool** with shared soft-cap concurrency and credit-aware selection
- **Outbound proxy pool** (env fixed, live nodes, or direct; xAI always direct)
- **Admin API + SPA** for tokens, keys, nodes, settings, stats, and a tok- playground

## Quick start

```bash
cp .env.example .env
# cargo does not load .env — export into the process:
set -a; source .env; set +a

# mint a client token (secret printed once)
cargo run -p serpotter-api -- seed-token --name local

# seed an upstream provider key
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"

export ADMIN_SECRET=dev-admin
cargo run -p serpotter-api
```

Health:

```bash
curl -fsS localhost:8080/live
# ready → {"status":"ready","schemaVersion":10,"expected":10}
curl -fsS localhost:8080/ready
```

Product:

```bash
curl -fsS -X POST localhost:8080/api/search \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"query":"rust axum","maxResults":5}'

curl -fsS -X POST localhost:8080/api/extract \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"url":"https://example.com"}'

curl -fsS -X POST localhost:8080/api/research \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"query":"axum middleware","extractTopN":2}'
```

Auth: `Authorization: Bearer tok-…` or `x-api-key` (headers only).

Optional live smoke (not CI):

```bash
SERPOTTER_TOKEN=tok-... ./scripts/live-smoke.sh
```

## Admin

Sync provider credits (admin secret or `adm-` session):

```bash
curl -fsS -X POST localhost:8080/api/keys/sync-credits \
  -H "Authorization: Bearer $ADMIN_SECRET" \
  -H "content-type: application/json" \
  -d '{"service":"tavily"}'
```

SPA (dev):

```bash
cd apps/admin && npm i && npm run dev
# http://localhost:5173/admin/  — login with ADMIN_SECRET
# playground uses a client tok- token
```

## MCP

Streamable HTTP via **rmcp**. All `/mcp` methods need a client token. Clients must send
`Accept: application/json, text/event-stream`. Public hosts need `MCP_ALLOWED_HOSTS`.

```bash
# initialize mints Mcp-Session-Id; then tools/list / tools/call
curl -fsS -D /tmp/mcp-headers -X POST localhost:8080/mcp \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"readme","version":"0.1.0"}}}'
SID=$(grep -i '^mcp-session-id:' /tmp/mcp-headers | awk '{print $2}' | tr -d '\r')
curl -fsS -X POST localhost:8080/mcp \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -H "mcp-session-id: $SID" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

## Docker

```bash
docker build -t serpotter-api .
docker run --rm -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-data:/data \
  serpotter-api

docker compose up -d --build
docker compose run --rm --entrypoint serpotter-api api seed-token --name local
```

Image runs as non-root **uid 10001**, HEALTHCHECK on `GET /ready`, volume `/data`.
Bind-mounts of `/data` must be writable by uid 10001 (named volumes are fine).

## Layout

```
crates/
  serpotter-api/       # binary + thin axum shells (admin / mcp / product)
  serpotter-product/   # search / extract / research orchestration
  serpotter-core/      # routing, RRF, types
  serpotter-db/        # sqlx + migrations (schema v10)
  serpotter-auth/      # tok- + problem+json
  serpotter-keypool/   # shared-cap key acquire/report
  serpotter-providers/ # Tavily / Firecrawl / Exa / xAI HTTP
  serpotter-outbound/  # ProxyPool + URL helpers
apps/admin/            # Vite + React SPA
docs/ops/              # deploy, env, API contract
```

## Docs

| Doc | Contents |
| --- | --- |
| [docs/ops/deploy.md](docs/ops/deploy.md) | binary, Docker, compose, seed, readiness |
| [docs/ops/env.md](docs/ops/env.md) | environment variables |
| [docs/ops/api.md](docs/ops/api.md) | HTTP/MCP contract, auth, errors, smoke |

Starter env: [`.env.example`](.env.example). Admin design tokens: [`design.md`](design.md).

## Quality / CI

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cd apps/admin && npm ci && npm run build
```

GitHub Actions (`.github/workflows/ci.yml`) runs the same gates on `main` and PRs.
