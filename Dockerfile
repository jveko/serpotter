# multi-stage build: compile serpotter-api, slim runtime
FROM rust:1-bookworm AS builder
WORKDIR /app

# Cache dependency builds
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p serpotter-api

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/serpotter-api /usr/local/bin/serpotter-api

EXPOSE 8080
VOLUME /data
ENV DATABASE_URL=sqlite:/data/serpotter.db?mode=rwc
ENV PORT=8080
ENV RUST_LOG=info,serpotter_api=info

ENTRYPOINT ["serpotter-api"]
