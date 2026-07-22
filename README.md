# Serpotter

Rust search proxy (mysearch rebrand). **Foundation + API tokens.**

## Run

```bash
cp .env.example .env
# cargo does not load .env; export vars into the process environment:
set -a; source .env; set +a

# mint a token (prints secret once on stdout)
cargo run -p serpotter-api -- seed-token --name local

cargo run -p serpotter-api
curl -s localhost:8080/live
curl -s localhost:8080/ready
curl -s -X POST localhost:8080/api/search \
  -H "Authorization: Bearer tok-..."
```

## Spec

See `docs/superpowers/specs/2026-07-22-serpotter-foundation-design.md` and
`docs/superpowers/plans/2026-07-22-serpotter-auth-tokens.md`.
