FROM rust:1.94-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /data

COPY --from=builder /app/target/release/hrmonitor-backend /usr/local/bin/hrmonitor-backend

EXPOSE 3001

CMD ["hrmonitor-backend"]
