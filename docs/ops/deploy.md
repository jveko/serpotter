# Deploy

Single binary (`serpotter-api`) + SQLite. Schema version **10** (`EXPECTED_SCHEMA_VERSION`). Process readiness is `GET /ready` (schema ≥ 10); liveness is `GET /live`.

## Binary (host)

```bash
cp .env.example .env
# cargo does not load .env
set -a; source .env; set +a

# optional: mint a client token (secret printed once on stdout)
cargo run -p serpotter-api -- seed-token --name local

# optional: seed an upstream provider key
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"
# services: tavily | firecrawl | exa | xai (default service for seed-key is tavily)

export ADMIN_SECRET=dev-admin   # enables admin API + SPA bootstrap
cargo run -p serpotter-api

curl -fsS localhost:8080/live
curl -fsS localhost:8080/ready
```

Default host DB path when unset: `sqlite:data/serpotter.db?mode=rwc` (creates `data/` as needed).

Graceful shutdown: SIGINT / SIGTERM stops the HTTP server, then aborts the 15m maintenance task.

## Docker image

```bash
docker build -t serpotter-api .

# named volume (recommended): ownership is already uid 10001 inside the image
docker run --rm -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-data:/data \
  serpotter-api

# seed against the same volume (override entrypoint)
docker run --rm -v serpotter-data:/data \
  --entrypoint serpotter-api serpotter-api seed-token --name local

docker run --rm -v serpotter-data:/data \
  -e TAVILY_API_KEY \
  --entrypoint serpotter-api serpotter-api \
  seed-key --service tavily --key "$TAVILY_API_KEY"
```

Image defaults:

| Item | Value |
| --- | --- |
| User | `serpotter` **uid 10001** (non-root) |
| Port | `8080` |
| Volume | `/data` |
| `DATABASE_URL` | `sqlite:/data/serpotter.db?mode=rwc` |
| HEALTHCHECK | `curl -fsS http://127.0.0.1:8080/ready` |

### Bind-mount ownership

If you bind-mount a host directory onto `/data`, it **must be writable by uid `10001`**:

```bash
mkdir -p ./data
sudo chown 10001:10001 ./data
docker run --rm -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v "$(pwd)/data:/data" \
  serpotter-api
```

Named Docker volumes created by compose/image do not need host `chown`.

## Compose

```bash
export ADMIN_SECRET=change-me   # override default dev-admin
# Public MCP: export MCP_ALLOWED_HOSTS=your.host,your.host:8080
# Optional JSON logs: export LOG_FORMAT=json  (compose default)
# Optional credit sync each 15m: export CREDIT_SYNC_CRON=1
docker compose up -d --build

curl -fsS localhost:8080/ready

# seed (one-shot, same volume)
docker compose run --rm --entrypoint serpotter-api api seed-token --name local
docker compose run --rm --entrypoint serpotter-api api \
  seed-key --service tavily --key "$TAVILY_API_KEY"
```

See `docker-compose.yml`: volume `serpotter-data`, `restart: unless-stopped`, healthcheck on `/ready`, `ADMIN_SECRET` / `LOG_FORMAT` / `CREDIT_SYNC_CRON` from env. Comment-document `MCP_ALLOWED_HOSTS` (never pass empty string — that disables allowlist), `REQUIRE_OUTBOUND_PROXY`, and optional SPA mount.

### Admin SPA bind-mount (no Docker npm stage)

```bash
cd apps/admin && npm ci && npm run build   # vite base: '/admin/'
# In docker-compose.yml uncomment:
#   ADMIN_SPA_DIR: /admin-dist
#   volumes: - ./apps/admin/dist:/admin-dist:ro
# Host dist must be readable by container uid 10001 (world-readable dist is fine).
docker compose up -d --build
# SPA at http://localhost:8080/admin/
```

## Gate before traffic

1. `GET /live` → 200  
2. `GET /ready` → 200 (schema migrated to ≥ 10)  
3. Product: `POST /api/search` with `Authorization: Bearer tok-…`  
4. Admin: `Authorization: Bearer $ADMIN_SECRET` or session after bootstrap  

Optional live vendor+MCP smoke (not CI): `SERPOTTER_TOKEN=tok-… ./scripts/live-smoke.sh` — see [cutover.md](./cutover.md).

Env reference: [env.md](./env.md). Client cutover: [cutover.md](./cutover.md).
