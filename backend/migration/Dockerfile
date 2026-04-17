FROM rust:1.94-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/Cargo.toml
COPY common/src common/src
COPY migration/Cargo.toml migration/Cargo.toml
COPY migration/src migration/src
RUN mkdir -p api-backend/src && echo 'fn main() {}' > api-backend/src/main.rs
COPY api-backend/Cargo.toml api-backend/Cargo.toml
COPY migration/migrations migration/migrations

# Stub out other workspace members
RUN mkdir -p pulsoid-ingest/src && echo 'fn main() {}' > pulsoid-ingest/src/main.rs
COPY pulsoid-ingest/Cargo.toml pulsoid-ingest/Cargo.toml
RUN mkdir -p pulsoid-refresher/src && echo 'fn main() {}' > pulsoid-refresher/src/main.rs
COPY pulsoid-refresher/Cargo.toml pulsoid-refresher/Cargo.toml
RUN mkdir -p ws-gateway/src && echo 'fn main() {}' > ws-gateway/src/main.rs
COPY ws-gateway/Cargo.toml ws-gateway/Cargo.toml

RUN cargo build --release -p migration

FROM debian:trixie-slim

COPY --from=builder /app/target/release/migration /usr/local/bin/migration

CMD ["migration"]
