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
curl -s localhost:8080/ready
curl -s -X POST localhost:8080/api/search \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"query":"rust axum","maxResults":5}'

# admin: refresh Tavily/Firecrawl credits on api_keys (ADMIN_SECRET)
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

# MCP (same Bearer / x-api-key)
curl -s -X POST localhost:8080/mcp \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

Admin SPA (optional):

```bash
cd apps/admin && npm i && npm run dev
# open http://localhost:5173 — login with ADMIN_SECRET
# Settings (socialEnabled), outbound nodes CRUD, search playground (tok- token)
```

Optional env: `TAVILY_BASE_URL`, `FIRECRAWL_BASE_URL`, `EXA_BASE_URL`, `XAI_BASE_URL`, `ADMIN_SECRET`.

## Spec / plans

- `docs/superpowers/specs/2026-07-22-serpotter-foundation-design.md` — foundation
- `docs/superpowers/specs/2026-07-22-serpotter-roadmap-design.md` — current architecture + residual/deferred waves (SoT)
- `docs/superpowers/plans/2026-07-22-serpotter-full-parity.md` — **SUPERSEDED** work queue (historical)
