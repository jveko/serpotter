# multi-stage build: compile serpotter-api, slim runtime
FROM rust:1-bookworm AS builder
WORKDIR /app

# Cache dependency builds
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p serpotter-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin serpotter \
    && mkdir -p /data \
    && chown -R serpotter:serpotter /data

COPY --from=builder /app/target/release/serpotter-api /usr/local/bin/serpotter-api

USER serpotter
EXPOSE 8080
VOLUME /data
ENV DATABASE_URL=sqlite:/data/serpotter.db?mode=rwc
ENV PORT=8080
ENV RUST_LOG=info,serpotter_api=info

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/ready || exit 1

ENTRYPOINT ["serpotter-api"]
