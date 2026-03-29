FROM rust:1.94-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev libpq-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY migrations ./migrations

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y libssl3 libpq5 ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hrmonitor_backend /usr/local/bin/hrmonitor_backend

EXPOSE 3001

CMD ["hrmonitor_backend"]
