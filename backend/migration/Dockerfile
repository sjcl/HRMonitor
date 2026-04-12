FROM rust:1.94-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev libpq-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/Cargo.toml
COPY common/src common/src
COPY migration/Cargo.toml migration/Cargo.toml
COPY migration/src migration/src
COPY api-backend/Cargo.toml api-backend/Cargo.toml
COPY api-backend/src api-backend/src
COPY migration/migrations migration/migrations

# Stub out other workspace members
RUN mkdir -p pulsoid-ingest/src && echo 'fn main() {}' > pulsoid-ingest/src/main.rs
COPY pulsoid-ingest/Cargo.toml pulsoid-ingest/Cargo.toml
RUN mkdir -p pulsoid-refresher/src && echo 'fn main() {}' > pulsoid-refresher/src/main.rs
COPY pulsoid-refresher/Cargo.toml pulsoid-refresher/Cargo.toml

RUN cargo build --release -p migration

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y libssl3t64 libpq5 ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/migration /usr/local/bin/migration

CMD ["migration"]
