# Serpotter CI/CD + Containers Design

**Date:** 2026-07-27  
**Status:** Approved for implementation planning  
**Scope:** Build and publish a single GHCR image; no auto-SSH deploy

## Problem

Serpotter already has:

- Quality CI (`.github/workflows/ci.yml`): workspace `cargo test` + `clippy -D warnings`, admin SPA `npm ci` + build
- A multi-stage `Dockerfile` (Rust builder → non-root runtime uid **10001**, `/data`, `HEALTHCHECK` on `/ready`)
- Local `docker-compose.yml` with `build: .` only

Gaps:

- No image publish to GHCR
- No tag strategy / GHA layer cache for container builds
- Admin SPA is **not** in the image (bind-mount or separate Vite only)
- No prod compose overlay that pulls a pre-built image (Merpati has this)

## Goals

1. Publish **one** image: `ghcr.io/jveko/serpotter` (`ghcr.io/${{ github.repository }}`)
2. Bake admin SPA into the image (`vite` `base: '/admin/'` → serve via `ADMIN_SPA_DIR`)
3. Merpati-aligned pipeline: quality gate, then main publish with `needs:`; tags/dispatch in a separate workflow
4. Local compose still builds; prod overlay pulls GHCR
5. PR docker **build-only** smoke so Dockerfile/SPA stage cannot break silently

## Non-goals

- Auto-deploy / SSH / Dokploy / host restart
- Multi-arch builds (default `linux/amd64` runner only)
- Separate frontend nginx image (Merpati pattern — rejected for Serpotter)
- Merpati extras: `hk`, `mise`, `cargo-nextest`, multi-bin `BUILD_BIN` targets
- Changing product wire/schema, uid **10001**, or SQLite `/data` layout

## Reference: Merpati CI/CD (sibling project)

| Piece | Merpati | Serpotter adaptation |
| --- | --- | --- |
| Quality | `ci.yml` check + test | Keep rust + admin jobs; add `--locked`, `CARGO_INCREMENTAL=0`, concurrency |
| Main images | **Same `ci.yml`**, `needs: [check, test]`, push `sha` + `latest` | `publish` job `needs: [rust, admin]`, single image |
| Tags / manual | `docker-publish.yml` on `v*` + `workflow_dispatch` only; no re-test | Same |
| Dockerfile | cargo-chef + BuildKit cargo mounts; multi-bin targets | chef + mounts; **one** runtime stage; **admin-build** stage |
| Frontend | Separate `ghcr.io/.../frontend` | **Baked** into API image |
| Prod compose | `docker-compose.prod.yml` image overrides | Same idea, one service |
| Actions | SHA-pinned, `persist-credentials: false` | Same default |
| PR image build | None | **Yes** — build-only smoke (Serpotter-only) |

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Delivery scope | Build + push only (no auto-deploy) |
| Image name | `ghcr.io/jveko/serpotter` |
| SPA | Multi-stage npm build; copy dist; `ADMIN_SPA_DIR=/admin-dist` |
| Main publish gate | Inside `ci.yml` after `needs: [rust, admin]` |
| Tags / dispatch | `docker-publish.yml` (no re-lint/test) |
| PR Docker | Build image, `push: false` |
| Package visibility | Leave GitHub default (repo is public) |
| Runtime user | Keep **uid 10001** (do not adopt Merpati 1001) |
| Cargo in image | cargo-chef + BuildKit `--mount=type=cache` |
| Platform | `linux/amd64` only |

## Architecture

```text
PR / push main
  ci.yml
    rust     → test + clippy (--locked)
    admin    → npm ci + build
    docker-smoke  (PR only, needs rust+admin)
      → docker build, push: false, GHA cache read
    publish       (main push only, needs rust+admin)
      → ghcr.io/jveko/serpotter :sha + :latest

push tag v*  OR  workflow_dispatch
  docker-publish.yml
      → semver (+ sha); latest only on dispatch from main
      → shared GHA cache scope=serpotter
```

```text
Dockerfile stages
  admin-build  → node:22, vite build → /admin-dist
  chef/planner → cargo-chef prepare → recipe.json
  builder      → cook + cargo build -p serpotter-api → /out/serpotter-api
  runtime      → bookworm-slim, uid 10001, binary + SPA, HEALTHCHECK /ready
```

## Dockerfile design

### Stages

1. **`admin-build`** (`node:22-bookworm` or current Node 22 LTS image)
   - Copy `apps/admin/package.json` + lockfile → `npm ci`
   - Copy `apps/admin` sources → `npm run build`
   - Output: static assets under `/admin-dist` (or `/app/dist` copied later)
   - Vite already sets `base: '/admin/'`

2. **`chef` / `planner`** (`rust:1-bookworm` or pinned minor)
   - Install `cargo-chef --locked`
   - `cargo chef prepare --recipe-path recipe.json`

3. **`builder`**
   - `cargo chef cook --release --recipe-path recipe.json` with BuildKit mounts:
     - `/usr/local/cargo/registry`
     - `/usr/local/cargo/git`
     - `/app/target`
   - Copy workspace sources; `cargo build --release -p serpotter-api` (same mounts)
   - Copy binary to `/out/serpotter-api` (so cache mount does not trap the artifact)

4. **`runtime`** (`debian:bookworm-slim`)
   - `ca-certificates` + `curl` (HEALTHCHECK)
   - `useradd` **serpotter uid 10001**, `/data` owned by that user
   - `COPY` binary → `/usr/local/bin/serpotter-api`
   - `COPY` admin dist → `/admin-dist`
   - `ENV ADMIN_SPA_DIR=/admin-dist`
   - Keep `DATABASE_URL`, `PORT`, `RUST_LOG`, `VOLUME /data`, `EXPOSE 8080`
   - `HEALTHCHECK` → `curl -fsS http://127.0.0.1:8080/ready`
   - `ENTRYPOINT ["serpotter-api"]`
   - Default stage (no multi-target matrix)

### `.dockerignore`

Preserve ignores for `target`, `node_modules`, `apps/admin/dist`, `data`, `.env`, `.git`, etc.  
**Must allow** `apps/admin` sources and full Cargo workspace for chef/build.  
Do not ignore files required by `cargo chef prepare`.

### Local build

```bash
docker build -t serpotter .
```

Same default stage as CI; no `--target` matrix.

## Workflow design

### `.github/workflows/ci.yml`

**Triggers:** `push` to `main`, `pull_request` (to `main` or bare PR as today).

**Env:**

- `CARGO_TERM_COLOR: always`
- `CARGO_INCREMENTAL: 0`
- `REGISTRY: ghcr.io`
- `IMAGE: ghcr.io/${{ github.repository }}`

**Concurrency:** `group: ${{ github.workflow }}-${{ github.ref }}`, `cancel-in-progress: true`.

**Jobs:**

| Job | When | Role |
| --- | --- | --- |
| `rust` | always | checkout (`persist-credentials: false`), stable + clippy, Swatinem/rust-cache `key: rust`, `cargo test --workspace --locked`, `cargo clippy --workspace --locked -- -D warnings` |
| `admin` | always | Node 22, `npm ci`, `npm run build` in `apps/admin` |
| `docker-smoke` | `pull_request` only | `needs: [rust, admin]`; buildx; **no** GHCR login; `docker/build-push-action` with `push: false`; `cache-from: type=gha,scope=serpotter` (read); optional local tag for clarity |
| `publish` | `push && refs/heads/main` | `needs: [rust, admin]`; `permissions: packages: write`; login `GITHUB_TOKEN`; metadata tags `type=sha,prefix=` + `type=raw,value=latest`; build-push `push: true`; `cache-from` + `cache-to: type=gha,scope=serpotter,mode=max` |

**Action pinning:** SHA-pin critical actions (checkout, buildx, login, metadata, build-push, rust-cache) in the Merpati style where practical.

### `.github/workflows/docker-publish.yml`

**Triggers:** `push` tags `v*`, `workflow_dispatch`.

**Concurrency:** `group: docker-${{ github.ref }}` (must **not** share `ci` group — avoid cancelling quality jobs).

**Job `publish`:**

- No re-test / re-clippy (document in-file like Merpati)
- Login + metadata:
  - `type=semver,pattern={{version}}`
  - `type=semver,pattern={{major}}.{{minor}}`
  - `type=sha,prefix=`
  - `type=raw,value=latest,enable=${{ github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' }}`
- build-push + GHA cache `scope=serpotter`

**Latest ownership:** day-to-day `:latest` comes from **main** `ci.yml` publish. Tag pushes get semver (+ sha), not automatic latest (unless dispatch from main).

## Compose design

### `docker-compose.yml` (dev)

- Keep `build: .` for local iteration
- Named volume `serpotter-data`, healthcheck `/ready`, existing env knobs
- Image from local build includes SPA; bind-mount of `apps/admin/dist` remains optional advanced override, not the primary SPA path

### `docker-compose.prod.yml` (new)

```yaml
# Usage:
#   docker compose -f docker-compose.yml -f docker-compose.prod.yml pull
#   docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
services:
  api:
    image: ghcr.io/${GITHUB_REPOSITORY:-jveko/serpotter}:latest
    # build: omitted / overridden by image key
```

Pinning a digest or `:sha` tag is recommended for production; `:latest` is the convenient default documented for personal VPS.

## Docs updates

| Doc | Change |
| --- | --- |
| `docs/ops/deploy.md` | GHCR pull, prod compose overlay, SPA-in-image as default, seed via entrypoint unchanged |
| `docs/ops/env.md` | Image sets `ADMIN_SPA_DIR=/admin-dist`; remove “Docker has no SPA stage” as default story |
| `AGENTS.md` | Note dual workflows, image name, SPA bake, prod compose |

## Success criteria

1. **PR:** `rust` + `admin` green; `docker-smoke` builds the image without pushing.
2. **Push `main`:** after `rust` + `admin`, image appears on GHCR as `:latest` and bare git `:sha`.
3. **Tag `vX.Y.Z`:** semver tags on GHCR without re-running the full test job in `docker-publish.yml`.
4. **Prod path:** `docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d` → `GET /ready` 200 and `/admin/` serves the SPA.
5. **Local:** `docker build -t serpotter .` and compose `build` still work offline from GHCR.

## Implementation outline (for writing-plans)

1. Upgrade root `Dockerfile` (syntax directive, admin-build, chef/builder, runtime + SPA).
2. Adjust `.dockerignore` if needed for admin sources / chef.
3. Extend `ci.yml` (env, locked, concurrency, docker-smoke, publish).
4. Add `docker-publish.yml`.
5. Add `docker-compose.prod.yml`; light touch on `docker-compose.yml` comments/env.
6. Update `docs/ops/deploy.md`, `env.md`, `AGENTS.md`.
7. Verify: local `docker build`; workflow YAML sanity; document manual first-push expectations.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Full Rust rebuild every publish | cargo-chef + BuildKit mounts + GHA `scope=serpotter` |
| Broken image on main | `publish` `needs: [rust, admin]`; PR `docker-smoke` |
| Cache races | Single image, single cache writer on last/only build-push |
| SPA base path wrong | Keep existing `base: '/admin/'`; smoke `/admin/` after deploy |
| Private package pull friction | Repo is public; leave visibility default; document `docker login` if package is private |
| Action major breakage | SHA-pin publish-critical actions |

## Open items for implementer (non-blocking)

- Exact Node base tag (`node:22-bookworm` vs `node:22-alpine`) — prefer bookworm glibc consistency with docs; alpine is fine if npm build is pure static.
- Whether `docker-smoke` should `needs: [admin]` only vs both — design says both so red quality never spends a full image build; acceptable to `needs: [rust, admin]`.
- First GHCR package permissions after first push (GitHub UI once) — ops note only.
