# Deploy

Single binary (`serpotter-api`) + SQLite. Schema version **17** (`EXPECTED_SCHEMA_VERSION`).

| Probe | Path | Meaning |
| --- | --- | --- |
| Liveness | `GET /live` | process up |
| Readiness | `GET /ready` | DB migrated and schema ≥ 17 |

## Binary (host)

```bash
cp .env.example .env
# cargo does not load .env
set -a; source .env; set +a

# optional: mint a client token (secret printed once on stdout)
cargo run -p serpotter-api -- seed-token --name local

# optional: seed an upstream provider key
cargo run -p serpotter-api -- seed-key --service tavily --key "$TAVILY_API_KEY"
# services: tavily | firecrawl | exa | xai (default: tavily)

export ADMIN_SECRET=dev-admin   # enables admin API + SPA bootstrap
cargo run -p serpotter-api

curl -fsS localhost:8080/live
curl -fsS localhost:8080/ready
```

Default host DB when unset: `sqlite:data/serpotter.db?mode=rwc` (creates `data/` as needed).

**`data/` permissions (host deploys):** the SQLite file holds plaintext upstream
provider keys and tok- client tokens (personal-use at-rest threat model). The
binary creates `data/` with the default umask (typically world-readable `0755` /
`0644` on a multi-user host). Restrict it before first run:

```bash
mkdir -p data
chmod 700 data
# or run the whole process under umask 077 so the DB file inherits 0600:
#   umask 077; set -a; source .env; set +a; serpotter-api
```

Container deploys need no host chmod when using a named volume (image ownership
is uid `10001`); bind-mounts must be `chown 10001:10001` per the section above.

Graceful shutdown: SIGINT / SIGTERM stops the HTTP server, then aborts the 15m maintenance task.

## Docker image

Local image tag matches the GHCR package name (`serpotter`). The process binary inside remains `serpotter-api`.

```bash
docker build -t serpotter .

# named volume (recommended): ownership is already uid 10001 inside the image
docker run --rm -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-data:/data \
  serpotter

# seed against the same volume (override entrypoint)
docker run --rm -v serpotter-data:/data \
  --entrypoint serpotter-api serpotter seed-token --name local

docker run --rm -v serpotter-data:/data \
  -e TAVILY_API_KEY \
  --entrypoint serpotter-api serpotter \
  seed-key --service tavily --key "$TAVILY_API_KEY"
```

### GHCR pull

```bash
docker pull ghcr.io/jveko/serpotter:latest
# Public repo packages are typically pullable without login.
# If the package is private: gh auth token | docker login ghcr.io -u USER --password-stdin
```

Image defaults:

| Item | Value |
| --- | --- |
| GHCR | `ghcr.io/jveko/serpotter` (`:latest`, bare `:sha`, semver on tags) |
| Admin SPA | baked at `/admin-dist`; `ADMIN_SPA_DIR=/admin-dist` → site root `/` |
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
  serpotter
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
curl -fsS -o /dev/null -w "%{http_code}\n" localhost:8080/

# seed (one-shot, same volume)
docker compose run --rm --entrypoint serpotter-api api seed-token --name local
docker compose run --rm --entrypoint serpotter-api api \
  seed-key --service tavily --key "$TAVILY_API_KEY"
```

`docker-compose.yml` provides volume `serpotter-data`, `restart: unless-stopped`, healthcheck on `/ready`, and env for `ADMIN_SECRET` / `LOG_FORMAT` / `CREDIT_SYNC_CRON`. Comment-document `MCP_ALLOWED_HOSTS` (never pass empty string — that disables the allowlist), `REQUIRE_OUTBOUND_PROXY`, and optional SPA override mount.

### Compose (prod / GHCR) — full file

Use **only** `docker-compose.prod.yml` (standalone). It does **not** use base
`docker-compose.yml` and never builds from source.

| Item | Value |
| --- | --- |
| File | `docker-compose.prod.yml` |
| Image | `ghcr.io/jveko/serpotter` (package **serpotter**; binary **serpotter-api**) |
| Platform | `linux/amd64` (CI publishes amd64 only; arm64 hosts use emulation) |
| Pull | `pull_policy: always` |
| Build | **none** |
| Volume | `serpotter-prod-data` → `/data` (uid 10001) |
| SPA | baked `/admin-dist` → site root `/` (router fallback; same as the image-defaults table below) |
| Port | host `${PUBLISH_PORT:-8080}` → container `8080` |
| `ADMIN_SECRET` | **required** (compose fails if unset) |

```bash
export ADMIN_SECRET=change-me
# optional pin (default latest):
# export SERPOTTER_IMAGE_TAG=<git-sha|v1.2.3|latest>
# export GITHUB_REPOSITORY=jveko/serpotter
# export PUBLISH_PORT=8080
# export MCP_ALLOWED_HOSTS=your.host,your.host:8080
# export CREDIT_SYNC_CRON=1

docker compose -f docker-compose.prod.yml pull
docker compose -f docker-compose.prod.yml up -d

# config must show image + platform, no build:
docker compose -f docker-compose.prod.yml config | grep -E 'image:|platform:|build:' || true

curl -fsS localhost:${PUBLISH_PORT:-8080}/live
curl -fsS localhost:${PUBLISH_PORT:-8080}/ready
curl -fsS -o /dev/null -w "%{http_code}\n" localhost:${PUBLISH_PORT:-8080}/         # expect 200

# seed (same volume; entrypoint = binary name)
docker compose -f docker-compose.prod.yml run --rm --entrypoint serpotter-api \
  api seed-token --name local
docker compose -f docker-compose.prod.yml run --rm --entrypoint serpotter-api \
  api seed-key --service tavily --key "$TAVILY_API_KEY"

docker compose -f docker-compose.prod.yml logs -f api
docker compose -f docker-compose.prod.yml down        # keep volume
# docker compose -f docker-compose.prod.yml down -v  # wipe SQLite
```

Local **build** path remains `docker compose up -d --build` via `docker-compose.yml` only.
Prefer pinning `SERPOTTER_IMAGE_TAG` to a bare git sha or semver in real prod; `:latest` is convenience.

### Admin SPA

Default: the multi-stage image already bakes SPA output at `/admin-dist` and sets `ADMIN_SPA_DIR=/admin-dist`, so the console is served at the **site root** without a host bind-mount. Image `admin-build` uses Node **22.18+** and `npm run build` (Vite+ under the hood: `tsc -b && vp build`; Vite `base` stays the default `/`).\n\nThe SPA is the router **fallback**: `/api`, `/mcp`, `/live` and `/ready` are matched first and never shadowed, unknown `/api` paths answer a JSON 404, and any other unmatched path returns `index.html` so refreshing `/keys` or `/logs` boots the app instead of 404ing.

Local SPA toolchain (same scripts as CI/Docker):

```bash
cd web
npm ci
npm run dev        # Vite+ dev server → http://localhost:5173/
npm run typecheck  # tsc -b
npm run check      # vp check
npm run build      # tsc -b && vp build → dist/ (assets under /assets/)
```

Optional host override (rebuild SPA locally and bind-mount):

```bash
cd web && npm ci && npm run build   # Vite+; base '/'
# In docker-compose.yml optionally set ADMIN_SPA_DIR and uncomment:
#   volumes: - ./web/dist:/admin-dist:ro
# Host dist must be readable by container uid 10001 (world-readable dist is fine).
docker compose up -d --build
# SPA at http://localhost:8080/
```

### Admin SPA security notes (localStorage)

The admin console persists credentials in `localStorage` (JS-readable, no
expiry, no `HttpOnly`), consistent with the personal-use plaintext-at-rest
threat model:

- **Secret-mode login** stores the raw `ADMIN_SECRET` under
  `serpotter_admin_secret` (cleared by session login/logout).
- **Session login** stores the `adm-` session token (7-day TTL).
- The **playground** stores the last used `tok-` client token plaintext
  (`PLAY_TOKEN_KEY`) and intentionally keeps it across logout so a working
  token survives an admin session; deleting that token in the Tokens panel
  clears it.

Any XSS in the served SPA, or another extension/site sharing the origin, can
read these values. This is a documented trade-off, not a bug. A future
hardening option is serving the admin session as an `HttpOnly` + `SameSite`
cookie instead of a JS-visible token; that is not implemented today.

## TLS / exposure (prod)

**Guidance — pick what fits your network.** `docker-compose.prod.yml` publishes
the entire surface — admin API (raw `ADMIN_SECRET` bearer + `adm-` sessions),
`tok-` product endpoints, MCP, and the admin SPA — over **plaintext HTTP** on
`${PUBLISH_PORT:-8080}`. If that port is reachable from the internet, the admin
secret bearer and all tokens travel in cleartext. The documented public-MCP
setup (`MCP_ALLOWED_HOSTS=<public hostname>`) is exactly this exposure case.
Options, in order of safety:

- **Terminate TLS in front of the port** — run Caddy, nginx, or cloudflared on
  `:443` and reverse-proxy to `127.0.0.1:${PUBLISH_PORT}` (admin/token/MCP auth
  headers then travel over TLS).
- **Bind to loopback only** — set `PUBLISH_PORT=127.0.0.1:8080` in the compose
  env (or run with `-p 127.0.0.1:8080:8080`) and reach the service over an SSH
  tunnel / Tailscale; nothing is exposed to the network directly.
- **Avoid** publishing `${PUBLISH_PORT}` (bare `8080` → `0.0.0.0`) on a public
  VPS without one of the above.

When exposing MCP to browser-origin clients, also set `MCP_ALLOWED_ORIGINS`
(the origin allowlist — see [env.md](./env.md)); the Host allowlist alone does
not restrict which page can open the connection.

## Gate before traffic

1. `GET /live` → 200
2. `GET /ready` → 200 (schema migrated to ≥ 17)
3. Product: `POST /api/search` with `Authorization: Bearer tok-…`
4. Admin: `Authorization: Bearer $ADMIN_SECRET` or session after bootstrap

Optional live vendor + MCP smoke (not CI):

```bash
SERPOTTER_TOKEN=tok-… ./scripts/live-smoke.sh
```

See [api.md](./api.md). Env reference: [env.md](./env.md).
