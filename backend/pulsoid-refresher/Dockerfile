FROM rust:1.95-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY common/Cargo.toml common/Cargo.toml
COPY common/src common/src
COPY pulsoid-refresher/Cargo.toml pulsoid-refresher/Cargo.toml
COPY pulsoid-refresher/src pulsoid-refresher/src

# Stub out other workspace members so cargo can resolve the workspace
# graph without pulling in their sources.
RUN mkdir -p api-backend/src && echo 'fn main() {}' > api-backend/src/main.rs
COPY api-backend/Cargo.toml api-backend/Cargo.toml
RUN mkdir -p migration/src && echo 'fn main() {}' > migration/src/main.rs
COPY migration/Cargo.toml migration/Cargo.toml
RUN mkdir -p pulsoid-ingest/src && echo 'fn main() {}' > pulsoid-ingest/src/main.rs
COPY pulsoid-ingest/Cargo.toml pulsoid-ingest/Cargo.toml
RUN mkdir -p ws-gateway/src && echo 'fn main() {}' > ws-gateway/src/main.rs
COPY ws-gateway/Cargo.toml ws-gateway/Cargo.toml

RUN cargo build --release -p pulsoid-refresher

FROM debian:trixie-slim

RUN apt-get update \
 && apt-get install -y ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 app \
 && useradd --system --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin app

COPY --from=builder /app/target/release/pulsoid-refresher /usr/local/bin/pulsoid-refresher

USER app:app

CMD ["pulsoid-refresher"]
