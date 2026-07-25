# Serpotter

Rust search proxy (mysearch rebrand). Multi-provider search, extract/research, MCP, admin API + lean SPA.

## Run

```bash
cp .env.example .env
# cargo does not load .env; export vars into the process environment:
set -a; source .env; set +a

# mint client token (prints secret once on stdout)
cargo run -p serpotter-api -- seed-token --name local

# seed upstream provider key
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"

export ADMIN_SECRET=dev-admin   # enables /api/tokens|/api/keys|/api/stats admin
cargo run -p serpotter-api

curl -s localhost:8080/live
# ready: {"status":"ready","schemaVersion":9,"expected":9} (camelCase; not status "ok")
curl -s localhost:8080/ready
curl -s -X POST localhost:8080/api/search \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"query":"rust axum","maxResults":5}'

# admin: refresh Tavily/Firecrawl credits on api_keys (ADMIN_SECRET or adm- session)
curl -s -X POST localhost:8080/api/keys/sync-credits \
  -H "Authorization: Bearer $ADMIN_SECRET" \
  -H "content-type: application/json" \
  -d '{"service":"tavily"}'

# extract / research
curl -s -X POST localhost:8080/api/extract \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"url":"https://example.com"}'
curl -s -X POST localhost:8080/api/research \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"query":"axum middleware","extractTopN":2}'

# MCP (rmcp Streamable HTTP): Accept JSON+SSE; initialize mints Mcp-Session-Id
# tools/list alone without Accept/session may fail — prefer initialize → tools/list
curl -s -D /tmp/mcp-headers -X POST localhost:8080/mcp \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"readme","version":"0.1.0"}}}'
SID=$(grep -i '^mcp-session-id:' /tmp/mcp-headers | awk '{print $2}' | tr -d '\r')
curl -s -X POST localhost:8080/mcp \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -H "accept: application/json, text/event-stream" \
  -H "mcp-session-id: $SID" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

Admin SPA (optional):

```bash
cd apps/admin && npm i && npm run dev
# open http://localhost:5173 — login with ADMIN_SECRET (stores adm- session in localStorage)
# Keys list + Sync credits; Settings (socialEnabled); outbound nodes CRUD; search playground (tok- token)
```

Optional env: `TAVILY_BASE_URL`, `FIRECRAWL_BASE_URL`, `EXA_BASE_URL`, `XAI_BASE_URL`, `ADMIN_SECRET`. Full list: [docs/ops/env.md](docs/ops/env.md).

## Docker / deploy

```bash
# image (non-root uid 10001, HEALTHCHECK GET /ready, VOLUME /data)
docker build -t serpotter-api .
docker run --rm -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-data:/data \
  serpotter-api

# compose
docker compose up -d --build

# seed against volume
docker compose run --rm --entrypoint serpotter-api api seed-token --name local
```

Host bind-mounts of `/data` must be writable by **uid 10001** (or use a named volume). Ops details:

- [docs/ops/deploy.md](docs/ops/deploy.md) — binary, image, compose, seed, `/ready`
- [docs/ops/env.md](docs/ops/env.md) — env knobs
- [docs/ops/cutover.md](docs/ops/cutover.md) — mysearch → serpotter wire freeze

## CI

GitHub Actions (`.github/workflows/ci.yml`) on `push` to `main` and all PRs:

- **rust:** `cargo test --workspace` then `cargo clippy --workspace -- -D warnings` (stable + clippy, `Swatinem/rust-cache`)
- **admin:** Node 22, `npm ci` + `npm run build` in `apps/admin` (npm cache on lockfile)

## Spec / plans

- `docs/superpowers/specs/2026-07-22-serpotter-foundation-design.md` — foundation
- `docs/superpowers/specs/2026-07-22-serpotter-roadmap-design.md` — architecture + residual waves (SoT for product roadmap)
- `docs/superpowers/specs/2026-07-22-serpotter-restructure-design.md` — crate restructure (product/admin/mcp split)
- `docs/superpowers/specs/2026-07-23-keypool-outbound-twin-pools-design.md` — KeyPool + ProxyPool twin pools (Landed)
- `docs/superpowers/specs/2026-07-23-admin-spa-refactor-design.md` — admin SPA module split (Landed)
- `docs/superpowers/plans/2026-07-23-keypool-outbound-twin-pools.md` — twin-pools implementation plan (Landed; historical checkboxes)
- `docs/superpowers/plans/2026-07-23-serpotter-residual-full.md` — residual full A–F package (Landed)
- `docs/superpowers/plans/2026-07-22-serpotter-full-parity.md` — **SUPERSEDED** work queue (historical)
