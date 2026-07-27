# CI/CD + Containers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish a single SPA-baked image `ghcr.io/jveko/serpotter` via Merpati-aligned CI (quality gate → main publish; tags/dispatch separate), with prod compose pull overlay.

**Architecture:** Upgrade the root multi-stage `Dockerfile` (admin npm build + cargo-chef + BuildKit mounts + runtime uid 10001). Extend `ci.yml` with locked rust jobs, PR `docker-smoke` (push false), and main `publish` (`needs: [rust, admin]`). Add `docker-publish.yml` for `v*` + `workflow_dispatch`. Add `docker-compose.prod.yml` image override; update ops docs.

**Tech Stack:** GitHub Actions, GHCR, Docker Buildx, cargo-chef, Node 22/npm, debian:bookworm-slim, existing serpotter-api binary.

**Spec:** `docs/superpowers/specs/2026-07-27-ci-cd-containers-design.md`

## Global Constraints

- Image name: `ghcr.io/${{ github.repository }}` → `ghcr.io/jveko/serpotter` (no `/api` suffix)
- Runtime user: **uid 10001** (do not change to Merpati 1001)
- SPA: Vite `base: '/admin/'`; image `ADMIN_SPA_DIR=/admin-dist`
- No auto-SSH deploy; no multi-arch; no separate frontend image
- Main `:latest` owned by `ci.yml` publish; tag workflow does not force `:latest` except dispatch-from-main
- GHA cache scope: `serpotter` (single writer)
- Action pins: reuse Merpati SHAs for checkout/buildx/login/metadata/build-push/rust-cache/rust-toolchain where listed below
- Never `git commit --no-verify`
- Product wire/schema unchanged

## File map

| File | Responsibility |
| --- | --- |
| `Dockerfile` | admin-build + chef/planner/builder + runtime (default stage) |
| `.dockerignore` | Keep build context small; allow `apps/admin` sources + crates |
| `.github/workflows/ci.yml` | rust, admin, docker-smoke (PR), publish (main) |
| `.github/workflows/docker-publish.yml` | tag `v*` + workflow_dispatch publish |
| `docker-compose.yml` | local `build: .`; SPA comments reflect image default |
| `docker-compose.prod.yml` | GHCR `image:` override |
| `docs/ops/deploy.md` | pull + prod compose + SPA-in-image |
| `docs/ops/env.md` | `ADMIN_SPA_DIR` image default |
| `AGENTS.md` | CI/Docker map notes |

## Action pin reference (from Merpati)

```text
actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30 # stable
Swatinem/rust-cache@42dc69e1aa15d09112580998cf2ef0119e2e91ae # v2
docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c # v4
docker/login-action@af1e73f918a031802d376d3c8bbc3fe56130a9b0 # v4
docker/metadata-action@dc802804100637a589fabce1cb79ff13a1411302 # v6
docker/build-push-action@53b7df96c91f9c12dcc8a07bcb9ccacbed38856a # v7
actions/setup-node@v4   # major pin OK (not used in Merpati docker path)
```

---

### Task 1: Multi-stage Dockerfile + dockerignore

**Files:**
- Modify: `Dockerfile` (replace entire file)
- Modify: `.dockerignore` (only if needed after review)
- Test: local `docker build` (smoke)

**Interfaces:**
- Consumes: workspace `Cargo.toml`/`Cargo.lock`/`crates/**`, `apps/admin/**` (lockfile + sources), existing binary name `serpotter-api`
- Produces: default image stage with `/usr/local/bin/serpotter-api`, `/admin-dist`, `ADMIN_SPA_DIR=/admin-dist`, uid 10001, HEALTHCHECK `/ready`

- [ ] **Step 1: Replace `Dockerfile` with the full multi-stage file**

Write `Dockerfile` exactly:

```dockerfile
# syntax=docker/dockerfile:1
# Multi-stage: admin SPA + cargo-chef Rust build + non-root runtime.
#
#   docker build -t serpotter .
#   # image includes /admin-dist and ADMIN_SPA_DIR=/admin-dist

# ── Admin SPA (vite base: /admin/) ───────────────────────────────────────────
FROM node:22-bookworm AS admin-build
WORKDIR /admin
COPY apps/admin/package.json apps/admin/package-lock.json ./
RUN npm ci
COPY apps/admin/ ./
RUN npm run build \
    && mkdir -p /admin-dist \
    && cp -a dist/. /admin-dist/

# ── cargo-chef ───────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release -p serpotter-api \
    && mkdir -p /out \
    && cp /app/target/release/serpotter-api /out/serpotter-api

# ── Runtime ──────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin serpotter \
    && mkdir -p /data /admin-dist \
    && chown -R serpotter:serpotter /data

COPY --from=builder /out/serpotter-api /usr/local/bin/serpotter-api
COPY --from=admin-build --chown=serpotter:serpotter /admin-dist /admin-dist

USER serpotter
EXPOSE 8080
VOLUME /data
ENV DATABASE_URL=sqlite:/data/serpotter.db?mode=rwc
ENV PORT=8080
ENV RUST_LOG=info,serpotter_api=info
ENV ADMIN_SPA_DIR=/admin-dist

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/ready || exit 1

ENTRYPOINT ["serpotter-api"]
```

- [ ] **Step 2: Review `.dockerignore`**

Current file should remain:

```gitignore
/target
**/target
**/node_modules
apps/admin/node_modules
apps/admin/dist
/data
.env
*.db
*.db-*
.git
.github
docs
**/.DS_Store
```

**Must not** ignore `apps/admin` sources, `Cargo.toml`, `Cargo.lock`, or `crates/`.  
If anything blocks `cargo chef prepare`, fix only that line. Do **not** add a blanket `apps/` ignore.

- [ ] **Step 3: Local build smoke**

```bash
docker build -t serpotter:local .
```

Expected: build completes; stages `admin-build`, `planner`, `builder`, `runtime` run. First build is slow (chef install + cook).

Optional runtime smoke (named volume):

```bash
docker run --rm -d --name serpotter-smoke -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-smoke-data:/data \
  serpotter:local
sleep 3
curl -fsS localhost:8080/ready
curl -fsS -o /dev/null -w "%{http_code}\n" localhost:8080/admin/
docker stop serpotter-smoke
```

Expected: `/ready` JSON/OK body; `/admin/` returns **200** (not 404). If `/admin/` is 404, check `ADMIN_SPA_DIR` and that `dist` was copied (Vite outputs `dist/`).

- [ ] **Step 4: Commit**

```bash
git add Dockerfile .dockerignore
git commit -m "build(docker): multi-stage SPA bake and cargo-chef"
```

---

### Task 2: Extend `ci.yml` (quality + smoke + main publish)

**Files:**
- Modify: `.github/workflows/ci.yml` (replace entire file)
- Test: YAML structure review; no need to push yet

**Interfaces:**
- Consumes: Task 1 `Dockerfile` at repo root
- Produces: jobs `rust`, `admin`, `docker-smoke` (PR), `publish` (main → GHCR `:sha` + `:latest`)

- [ ] **Step 1: Replace `.github/workflows/ci.yml`**

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0
  REGISTRY: ghcr.io
  IMAGE: ghcr.io/${{ github.repository }}

jobs:
  rust:
    name: Rust test & clippy
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
        with:
          persist-credentials: false

      - uses: dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30 # stable
        with:
          components: clippy

      - uses: Swatinem/rust-cache@42dc69e1aa15d09112580998cf2ef0119e2e91ae # v2
        with:
          key: rust

      - name: cargo test
        run: cargo test --workspace --locked

      - name: cargo clippy
        run: cargo clippy --workspace --locked -- -D warnings

  admin:
    name: Admin SPA build
    runs-on: ubuntu-latest
    timeout-minutes: 15
    defaults:
      run:
        working-directory: apps/admin
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
        with:
          persist-credentials: false

      - uses: actions/setup-node@v4
        with:
          node-version: "22"
          cache: npm
          cache-dependency-path: apps/admin/package-lock.json

      - run: npm ci
      - run: npm run build

  # PR only: prove Dockerfile + SPA stage build (no registry write)
  docker-smoke:
    name: Docker build smoke
    if: github.event_name == 'pull_request'
    needs: [rust, admin]
    runs-on: ubuntu-latest
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
        with:
          persist-credentials: false

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c # v4

      - name: Build (no push)
        uses: docker/build-push-action@53b7df96c91f9c12dcc8a07bcb9ccacbed38856a # v7
        with:
          context: .
          push: false
          tags: serpotter:smoke
          cache-from: type=gha,scope=serpotter
          # read-only cache on PR — main publish is the single cache-to writer

  # Main only: publish after quality gate (Merpati pattern)
  publish:
    name: Publish image
    if: github.event_name == 'push' && github.ref == 'refs/heads/main'
    needs: [rust, admin]
    runs-on: ubuntu-latest
    timeout-minutes: 45
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
        with:
          persist-credentials: false

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c # v4

      - name: Log in to GHCR
        uses: docker/login-action@af1e73f918a031802d376d3c8bbc3fe56130a9b0 # v4
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Docker metadata
        id: meta
        uses: docker/metadata-action@dc802804100637a589fabce1cb79ff13a1411302 # v6
        with:
          images: ${{ env.IMAGE }}
          tags: |
            type=sha,prefix=
            type=raw,value=latest

      - name: Build and push
        uses: docker/build-push-action@53b7df96c91f9c12dcc8a07bcb9ccacbed38856a # v7
        with:
          context: .
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha,scope=serpotter
          cache-to: type=gha,scope=serpotter,mode=max
```

- [ ] **Step 2: Sanity-check workflow locally**

```bash
# structure
grep -n "needs:\|docker-smoke\|publish:\|CARGO_INCREMENTAL\|--locked" .github/workflows/ci.yml
```

Expected: `docker-smoke` and `publish` both `needs: [rust, admin]`; test/clippy use `--locked`; `CARGO_INCREMENTAL: 0` in `env`.

If `actionlint` is installed: `actionlint .github/workflows/ci.yml` → no errors. If not installed, skip (do not add as a project dep).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: gate GHCR publish on rust and admin"
```

---

### Task 3: Add `docker-publish.yml` (tags + dispatch)

**Files:**
- Create: `.github/workflows/docker-publish.yml`
- Test: YAML structure review

**Interfaces:**
- Consumes: Task 1 `Dockerfile`; shared GHA cache `scope=serpotter`
- Produces: semver (+ sha) tags on GHCR; `:latest` only when `workflow_dispatch` on `main`

- [ ] **Step 1: Create `.github/workflows/docker-publish.yml`**

```yaml
name: docker-publish

# Version tags and manual rebuilds. Main-branch :latest publishes from ci.yml
# after rust+admin (needs:), so this file does not re-run tests.
on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: docker-${{ github.ref }}
  cancel-in-progress: true

env:
  REGISTRY: ghcr.io
  IMAGE: ghcr.io/${{ github.repository }}
  CARGO_INCREMENTAL: 0

jobs:
  publish:
    name: Publish image
    runs-on: ubuntu-latest
    timeout-minutes: 45
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
        with:
          persist-credentials: false

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c # v4

      - name: Log in to GHCR
        uses: docker/login-action@af1e73f918a031802d376d3c8bbc3fe56130a9b0 # v4
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Docker metadata
        id: meta
        uses: docker/metadata-action@dc802804100637a589fabce1cb79ff13a1411302 # v6
        with:
          images: ${{ env.IMAGE }}
          tags: |
            type=semver,pattern={{version}}
            type=semver,pattern={{major}}.{{minor}}
            type=sha,prefix=
            type=raw,value=latest,enable=${{ github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' }}

      - name: Build and push
        uses: docker/build-push-action@53b7df96c91f9c12dcc8a07bcb9ccacbed38856a # v7
        with:
          context: .
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha,scope=serpotter
          cache-to: type=gha,scope=serpotter,mode=max
```

- [ ] **Step 2: Verify concurrency group differs from `ci`**

```bash
grep -n "concurrency:" -A2 .github/workflows/ci.yml .github/workflows/docker-publish.yml
```

Expected: `ci` uses `github.workflow`-based group; `docker-publish` uses `docker-${{ github.ref }}`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/docker-publish.yml
git commit -m "ci: add tag and dispatch docker publish workflow"
```

---

### Task 4: Compose prod overlay + local compose SPA notes

**Files:**
- Create: `docker-compose.prod.yml`
- Modify: `docker-compose.yml` (SPA comments only — keep `build: .`)

**Interfaces:**
- Consumes: image `ghcr.io/jveko/serpotter:latest` (or `${GITHUB_REPOSITORY}`)
- Produces: `docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d` pull path

- [ ] **Step 1: Create `docker-compose.prod.yml`**

```yaml
# Production overrides — use pre-built GHCR image instead of building locally.
# Usage:
#   export GITHUB_REPOSITORY=jveko/serpotter   # optional; default below
#   docker compose -f docker-compose.yml -f docker-compose.prod.yml pull
#   docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
#
# Prefer pinning :sha or a semver tag in real prod; :latest is convenience.

services:
  api:
    image: ghcr.io/${GITHUB_REPOSITORY:-jveko/serpotter}:latest
```

- [ ] **Step 2: Update SPA comments in `docker-compose.yml`**

Replace the SPA-related comment block (lines that say build host dist / uncomment `ADMIN_SPA_DIR`) with:

```yaml
      # Admin SPA: image build bakes dist at /admin-dist and sets ADMIN_SPA_DIR.
      # Optional override (host dist must be readable by uid 10001):
      # ADMIN_SPA_DIR: /admin-dist
    volumes:
      - serpotter-data:/data
      # - ./apps/admin/dist:/admin-dist:ro
```

Keep all other env/volume/healthcheck settings unchanged. Do **not** remove `build: .` from the dev compose file.

- [ ] **Step 3: Commit**

```bash
git add docker-compose.yml docker-compose.prod.yml
git commit -m "build(compose): add GHCR prod overlay"
```

---

### Task 5: Ops docs + AGENTS map

**Files:**
- Modify: `docs/ops/deploy.md`
- Modify: `docs/ops/env.md`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: Tasks 1–4 behavior (image name, SPA bake, workflows, prod compose)
- Produces: operators can pull/run without reading the design spec

- [ ] **Step 1: Update `docs/ops/deploy.md` Docker sections**

In **Docker image**:

- Change local tag examples from `serpotter-api` to `serpotter` **or** document both (`docker build -t serpotter .` matching GHCR name). Prefer:

```bash
docker build -t serpotter .
docker run --rm -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-data:/data \
  serpotter
```

- Add **Image defaults** rows:

| Item | Value |
| --- | --- |
| GHCR | `ghcr.io/jveko/serpotter` (`:latest`, bare `:sha`, semver on tags) |
| Admin SPA | baked at `/admin-dist`; `ADMIN_SPA_DIR=/admin-dist` → `/admin/` |
| User | `serpotter` **uid 10001** |
| … | (keep port/volume/DATABASE_URL/HEALTHCHECK) |

- Add **GHCR pull** subsection after local build:

```bash
docker pull ghcr.io/jveko/serpotter:latest
# Public repo packages are typically pullable without login.
# If the package is private: gh auth token | docker login ghcr.io -u USER --password-stdin
```

- Replace **Admin SPA bind-mount (no Docker npm stage)** with **Admin SPA**:
  - Default: image already serves `/admin/`
  - Optional host bind-mount override still documented briefly
  - Remove “no Docker npm stage” as the primary story

- Add **Compose (prod / GHCR)** after local compose:

```bash
export ADMIN_SECRET=change-me
docker compose -f docker-compose.yml -f docker-compose.prod.yml pull
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
curl -fsS localhost:8080/ready
curl -fsS -o /dev/null -w "%{http_code}\n" localhost:8080/admin/
```

- [ ] **Step 2: Update `docs/ops/env.md` `ADMIN_SPA_DIR` row**

Replace the notes cell so it reads approximately:

> if set to a directory of built SPA assets, serves under `/admin/*` via `ServeDir`. **Build with Vite `base: '/admin/'`**. **Container image default:** `/admin-dist` (SPA baked in multi-stage build). Host/dev: unset, or point at `apps/admin/dist` after `npm run build`. Override bind-mount still supported.

- [ ] **Step 3: Update `AGENTS.md` NOTES / COMMANDS for CI + Docker**

Add concise bullets (do not rewrite the whole file):

- CI: `.github/workflows/ci.yml` — rust (`test`+`clippy --locked`) + admin; PR `docker-smoke`; main `publish` → `ghcr.io/jveko/serpotter` (`needs: [rust, admin]`)
- Tags/dispatch: `.github/workflows/docker-publish.yml` (no re-test)
- Docker: multi-stage SPA + cargo-chef; runtime uid 10001; `ADMIN_SPA_DIR=/admin-dist`
- Prod: `docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d`

Update any COMMANDS that say `docker build -t serpotter-api` to `serpotter` for consistency with the image name (or note alias).

- [ ] **Step 4: Commit**

```bash
git add docs/ops/deploy.md docs/ops/env.md AGENTS.md
git commit -m "docs(ops): GHCR image, SPA bake, and compose prod"
```

---

### Task 6: End-to-end local verification gate

**Files:** none new (verification only)

- [ ] **Step 1: Re-run local image build after all file changes**

```bash
docker build -t serpotter:local .
```

Expected: success.

- [ ] **Step 2: Run container and hit probes**

```bash
docker rm -f serpotter-smoke 2>/dev/null || true
docker run --rm -d --name serpotter-smoke -p 8080:8080 \
  -e ADMIN_SECRET=dev-admin \
  -v serpotter-smoke-data:/data \
  serpotter:local
for i in 1 2 3 4 5 6 7 8 9 10; do
  curl -fsS localhost:8080/ready && break
  sleep 1
done
curl -fsS localhost:8080/live
curl -fsS -o /dev/null -w "admin:%{http_code}\n" localhost:8080/admin/
docker stop serpotter-smoke
```

Expected: `/live` and `/ready` succeed; `admin:` is **200** (or 200-range). Failure = fix Dockerfile/SPA copy before claiming done.

- [ ] **Step 3: Confirm git log is logical commits**

```bash
git log --oneline -6
```

Expected: separate commits roughly matching Tasks 1–5 (docker, ci, docker-publish, compose, docs). No `--no-verify`.

- [ ] **Step 4: Manual GHCR note (no code)**

After first push to `main` on GitHub: confirm package `serpotter` appears under `ghcr.io/jveko/serpotter`. If visibility/package settings need a one-time UI click, document in the PR/summary only — no code change required.

---

## Spec coverage checklist

| Spec requirement | Task |
| --- | --- |
| SPA-baked multi-stage Dockerfile + cargo-chef + mounts | Task 1 |
| uid 10001 / HEALTHCHECK / `/data` preserved | Task 1 |
| `ci.yml` rust+admin `--locked`, concurrency, env | Task 2 |
| PR docker-smoke push false | Task 2 |
| Main publish needs rust+admin, sha+latest | Task 2 |
| `docker-publish.yml` v* + dispatch, semver, no re-test | Task 3 |
| Distinct concurrency group | Task 3 |
| `docker-compose.prod.yml` | Task 4 |
| Dev compose keeps build; SPA comments | Task 4 |
| deploy.md / env.md / AGENTS.md | Task 5 |
| Local build + `/ready` + `/admin/` smoke | Task 1 + Task 6 |
| No auto-deploy / no multi-arch / single image name | Global + all tasks |

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-ci-cd-containers.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)
2. **Parallel Independent Domains** — only where tasks do not share write targets (limited here: Task 1 must finish before 2–3 meaningfully smoke; 4–5 can parallelize after 1)

**Which approach?**
