FROM rust:1.94-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev libpq-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/Cargo.toml
COPY common/src common/src
COPY api-backend/Cargo.toml api-backend/Cargo.toml
COPY api-backend/src api-backend/src

# Stub out other workspace members so cargo resolves the workspace
RUN mkdir -p migration/src && echo 'fn main() {}' > migration/src/main.rs
COPY migration/Cargo.toml migration/Cargo.toml
RUN mkdir -p pulsoid-ingest/src && echo 'fn main() {}' > pulsoid-ingest/src/main.rs
COPY pulsoid-ingest/Cargo.toml pulsoid-ingest/Cargo.toml
RUN mkdir -p pulsoid-refresher/src && echo 'fn main() {}' > pulsoid-refresher/src/main.rs
COPY pulsoid-refresher/Cargo.toml pulsoid-refresher/Cargo.toml

RUN cargo build --release -p api-backend

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y libssl3t64 libpq5 ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/api-backend /usr/local/bin/api-backend

EXPOSE 3001

CMD ["api-backend"]
