# Serpotter

Rust search proxy (mysearch rebrand). **Auth + Tavily search (lean).**

## Run

```bash
cp .env.example .env
# cargo does not load .env; export vars into the process environment:
set -a; source .env; set +a

# mint client token (prints secret once on stdout)
cargo run -p serpotter-api -- seed-token --name local

# seed upstream Tavily key
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"

cargo run -p serpotter-api
curl -s localhost:8080/live
curl -s localhost:8080/ready
curl -s -X POST localhost:8080/api/search \
  -H "Authorization: Bearer tok-..." \
  -H "content-type: application/json" \
  -d '{"query":"rust axum","maxResults":5}'
```

Optional: `TAVILY_BASE_URL` (default `https://api.tavily.com`).

## Spec / plans

- `docs/superpowers/specs/2026-07-22-serpotter-foundation-design.md`
- `docs/superpowers/plans/2026-07-22-serpotter-auth-tokens.md`
- `docs/superpowers/plans/2026-07-22-serpotter-keypool-tavily.md`
