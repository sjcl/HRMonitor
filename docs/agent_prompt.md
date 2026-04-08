# Coding Agent Instructions

## Goal
Build a minimal heart-rate monitoring system using:
- Rust backend (axum + sqlx + SQLite)
- Next.js frontend (App Router)
- Docker deployment
- Cloudflare Access (frontend only)

## Architecture Constraints

### High-level
- Monorepo structure (frontend, backend, infra)
- Backend is NOT publicly exposed
- Frontend is the only public entrypoint
- Backend is accessed via internal Docker network

### Data Flow
Pulsoid WebSocket → Backend → SQLite → HTTP API → Frontend (polling)

### DO NOT implement:
- Authentication system for frontend users
- Browser WebSocket for live updates
- Redis, message queue, or caching layer
- Complex authorization or roles

---

## Backend Requirements

### Stack
- Rust
- axum
- tokio
- sqlx (SQLite)
- serde
- tracing

### Database
SQLite file at `/data/app.db`

### Tables

#### users
- id (TEXT, PK)
- name (TEXT)
- created_at (INTEGER)
- updated_at (INTEGER)

#### pulsoid_tokens
- id (TEXT, PK)
- user_id (TEXT, FK)
- label (TEXT)
- access_token (TEXT)
- is_active (BOOLEAN)
- last_connected_at (INTEGER, nullable)
- last_error (TEXT, nullable)
- created_at (INTEGER)
- updated_at (INTEGER)

#### heart_rate_records
- id (INTEGER, PK AUTOINCREMENT)
- user_id (TEXT)
- pulsoid_token_id (TEXT)
- recorded_at (INTEGER)
- bpm (INTEGER)
- received_at (INTEGER)

---

### Backend Behavior

#### On startup
- Load all `pulsoid_tokens WHERE is_active = true`
- Spawn one async worker per token

#### Worker loop
- Connect to Pulsoid WebSocket
- Parse heart rate messages
- Insert into DB
- On failure:
    - record `last_error`
    - retry with backoff

#### Validation
- bpm must be between 20 and 250
- timestamp fallback to now if invalid

---

### HTTP API

#### Users
- GET /api/users
- POST /api/users
- PATCH /api/users/:id

#### Tokens
- GET /api/users/:id/pulsoid-tokens
- POST /api/users/:id/pulsoid-tokens
- PATCH /api/pulsoid-tokens/:id
- DELETE /api/pulsoid-tokens/:id

#### Heart rate
- GET /api/users/:id/heart-rates
- GET /api/users/:id/latest-heart-rate

---

## Frontend Requirements

### Stack
- Next.js (App Router)
- No heavy state management required
- Use fetch or TanStack Query

### Pages

#### /users
- list users
- show latest bpm
- show token count

#### /users/[id]
- heart rate graph
- latest bpm
- list tokens
- add/remove token

---

### Data Fetching
- Poll every 5–10 seconds
- No WebSocket usage

---

## Docker Requirements

- Multi-stage builds for both frontend and backend
- Backend exposes internal port only
- Frontend exposed via Cloudflare Tunnel

---

## Code Style

- Keep implementation minimal
- Avoid unnecessary abstraction
- Prefer explicit code over generic frameworks
- Avoid premature optimization

---

## Deliverables

- Working backend (can receive Pulsoid data)
- Working frontend (can display graph)
- Docker Compose setup
- Minimal documentation

---

## Output Expectations

When implementing:
- Provide full file paths
- Provide complete files (not partial snippets)
- Keep code compile-ready
- No placeholders unless explicitly stated