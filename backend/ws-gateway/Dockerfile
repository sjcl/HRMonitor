FROM rust:1.95-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/Cargo.toml
COPY common/src common/src
COPY ws-gateway/Cargo.toml ws-gateway/Cargo.toml
COPY ws-gateway/src ws-gateway/src

# Stub out other workspace members so cargo resolves the workspace
RUN mkdir -p api-backend/src && echo 'fn main() {}' > api-backend/src/main.rs
COPY api-backend/Cargo.toml api-backend/Cargo.toml
RUN mkdir -p migration/src && echo 'fn main() {}' > migration/src/main.rs
COPY migration/Cargo.toml migration/Cargo.toml
RUN mkdir -p pulsoid-ingest/src && echo 'fn main() {}' > pulsoid-ingest/src/main.rs
COPY pulsoid-ingest/Cargo.toml pulsoid-ingest/Cargo.toml
RUN mkdir -p pulsoid-refresher/src && echo 'fn main() {}' > pulsoid-refresher/src/main.rs
COPY pulsoid-refresher/Cargo.toml pulsoid-refresher/Cargo.toml

RUN cargo build --release -p ws-gateway

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ws-gateway /usr/local/bin/ws-gateway

EXPOSE 3002

CMD ["ws-gateway"]
