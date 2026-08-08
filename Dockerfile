# syntax=docker/dockerfile:1
# Multi-stage: admin SPA + cargo-chef Rust build + non-root runtime.
#
#   docker build -t serpotter .
#   # image includes /admin-dist and ADMIN_SPA_DIR=/admin-dist

# ── Admin SPA (served at site root; vite base stays "/") ─────────────────────
# Pin policy: node minor-pinned for the SPA build (matches CI Node 22.18;
# patch-minor track; bump deliberately, not by floating `node:bookworm`).
FROM node:22.18-bookworm AS admin-build
WORKDIR /admin
COPY apps/admin/package.json apps/admin/package-lock.json ./
RUN npm ci
COPY apps/admin/ ./
RUN npm run build \
    && mkdir -p /admin-dist \
    && cp -a dist/. /admin-dist/

# ── cargo-chef ───────────────────────────────────────────────────────────────
# Pin policy: rust pinned to 1.97.0, matching rust-toolchain.toml and the
# workspace Cargo.toml rust-version. Bump only in lockstep with the toolchain pin.
FROM rust:1.97.0-bookworm AS chef
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
# Pin policy: debian:bookworm-slim is the runtime base track (stable bookworm,
# no floating distro tag; only upgrade to the next stable track deliberately).
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
