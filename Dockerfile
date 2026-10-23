FROM rust:1-bookworm AS builder

WORKDIR /app

COPY services/api/Cargo.toml services/api/Cargo.lock ./
COPY services/api/src ./src
COPY services/api/migrations ./migrations

RUN cargo build --release --locked --bin nhlanganiso-api

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/nhlanganiso-api /usr/local/bin/nhlanganiso-api

EXPOSE 10000

CMD ["nhlanganiso-api"]
