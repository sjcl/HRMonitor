FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /data

COPY --from=builder /app/target/release/hrmonitor_backend /usr/local/bin/hrmonitor_backend

EXPOSE 3001

CMD ["hrmonitor_backend"]
