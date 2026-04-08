FROM rust:1.94-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev libpq-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY api-backend/Cargo.toml api-backend/Cargo.toml
COPY api-backend/src api-backend/src
COPY api-backend/migrations api-backend/migrations

RUN cargo build --release -p api-backend

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y libssl3t64 libpq5 ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/api-backend /usr/local/bin/api-backend

EXPOSE 3001

CMD ["api-backend"]
