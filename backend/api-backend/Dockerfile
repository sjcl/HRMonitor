FROM rust:1.95-slim AS builder

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
RUN mkdir -p ws-gateway/src && echo 'fn main() {}' > ws-gateway/src/main.rs
COPY ws-gateway/Cargo.toml ws-gateway/Cargo.toml

RUN cargo build --release -p api-backend

FROM debian:trixie-slim

RUN apt-get update \
 && apt-get install -y ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 app \
 && useradd --system --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin app

COPY --from=builder /app/target/release/api-backend /usr/local/bin/api-backend

USER app:app

EXPOSE 3001

CMD ["api-backend"]
