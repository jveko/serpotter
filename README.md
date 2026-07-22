# Serpotter

Rust search proxy (mysearch rebrand). **Foundation** only: health endpoints + sqlx SQLite.

## Run

```bash
cp .env.example .env
cargo run -p serpotter-api
curl -s localhost:8080/live
curl -s localhost:8080/ready
```

## Spec

See `docs/superpowers/specs/2026-07-22-serpotter-foundation-design.md`.
