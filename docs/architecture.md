# Architecture

## Overview

This system collects heart rate data from Pulsoid WebSocket and stores it in SQLite.
Frontend fetches data periodically via HTTP.

## Components

### Backend
- Rust (axum)
- Handles:
    - Pulsoid WebSocket ingestion
    - Data persistence
    - HTTP API

### Frontend
- Next.js
- Displays:
    - Users
    - Heart rate graph
    - Token management

### Database
- SQLite (single file)
- Stored in Docker volume

---

## Data Flow

Pulsoid WS → Backend Worker → SQLite → HTTP API → Frontend

---

## Connection Model

- One WebSocket per Pulsoid token
- Tokens belong to users
- Users may have multiple tokens

---

## Deployment

- Docker Compose
- cloudflared tunnel
- Cloudflare Access protects frontend

---

## Security Model

- No app-level authentication
- Access restricted via Cloudflare Access
- Backend is not publicly reachable

---

## Scaling Strategy (future)

- SQLite → PostgreSQL
- Add aggregation tables
- Add Redis for caching