# HRMonitor

Pulsoid WebSocket から心拍数データをリアルタイム収集し、TimescaleDB に保存、WebSocket でブラウザにプッシュ配信する心拍モニタリングシステム。Discord OAuth によるユーザー認証付き。

## プロジェクト構成

```
backend/    Rust (axum + sqlx + TimescaleDB + Redis) — ポート 3001
frontend/   Next.js 16 (App Router + TanStack Query + Recharts + Auth.js v5) — ポート 3000
nginx/      Nginx (リバースプロキシ + 静的ファイル配信) — ポート 80
docs/            仕様書 (API, アーキテクチャ, スキーマ)
```

## 技術スタック

### Backend

- Rust (edition 2024), axum 0.8, tokio, sqlx 0.8 (PostgreSQL)
- TimescaleDB (PostgreSQL 17) — `heart_rate_records` は hypertable
- Redis — 最新心拍数キャッシュ (`latest_bpm:{user_id}`)
- Pulsoid WebSocket 接続: tokio-tungstenite
- リアルタイム配信: `tokio::sync::broadcast` → WebSocket プッシュ
- Pulsoid OAuth トークン暗号化: AES-256-GCM

### Frontend

- Next.js 16, React 19, TypeScript 6
- Auth.js v5 (next-auth@5 beta) — Discord OAuth、データベースセッション
- TanStack React Query 5 (データ取得)
- Recharts 3 (心拍グラフ)
- Tailwind CSS 4
- `useHeartRateWs` フック — WebSocket によるリアルタイム BPM 受信

### インフラ

- Docker + Docker Compose (6 サービス)
- Nginx — リバースプロキシ + 静的ファイル配信 (唯一の公開エントリポイント)
- cloudflared (Cloudflare Tunnel) — 本番公開用 (prod プロファイル)

## アーキテクチャ

```
ブラウザ
  ↓ HTTP / WebSocket
[Nginx :80] ─── 唯一の公開エントリポイント
  ├── /_next/static/  → 静的ファイル (365日キャッシュ)
  ├── /api/auth/*     → [Next.js Frontend :3000] (Auth.js)
  ├── /api/ws/*       → [Rust Backend :3001] (WebSocket upgrade)
  ├── /api/*          → [Rust Backend :3001] (REST API)
  ├── /healthz        → [Rust Backend :3001] (ヘルスチェック)
  └── /*              → [Next.js Frontend :3000] (SSR)

[Rust Backend :3001] ←── WebSocket ──→ Pulsoid API
  ├── DB 保存 → TimescaleDB
  ├── キャッシュ更新 → Redis
  └── broadcast → 接続中のブラウザへ WebSocket プッシュ
```

### リアルタイムデータフロー

```
Pulsoid WS → Backend 受信 → DB 保存 → Redis 更新 → broadcast → クライアント WebSocket プッシュ
```

## 認証

### Discord OAuth (ユーザー認証)

- Auth.js v5 (next-auth@5 beta) による Discord OAuth
- セッション戦略: データベースセッション (PostgreSQL に保存)
- Frontend が OAuth フローを処理、Backend はクッキーのセッショントークンを `sessions` テーブルで検証
- Nginx が `/api/auth/*` を Frontend にプロキシ

### Pulsoid OAuth (心拍データ連携)

- ユーザーが Pulsoid アカウントを OAuth フローで接続
- アクセストークン・リフレッシュトークンは AES-256-GCM で暗号化し `pulsoid_connections` テーブルに保存
- フロー:
  1. `POST /api/oauth/pulsoid/connect` で接続リクエスト作成
  2. `GET /api/oauth/pulsoid/connect/{request_id}` で Pulsoid 認可画面へリダイレクト
  3. `GET /api/oauth/pulsoid/callback` でコールバック処理、トークン取得・暗号化・保存

## セットアップ

### 1. 環境変数の設定

```bash
cp .env.example .env
```

`.env` を編集し、以下の値を設定:

| 変数名 | 説明 | 生成方法 |
|--------|------|----------|
| `AUTH_SECRET` | Auth.js セッション署名キー | `openssl rand -base64 32` |
| `AUTH_URL` | OAuth コールバック用の公開 URL | 例: `http://localhost:3000` |
| `DISCORD_CLIENT_ID` | Discord OAuth クライアント ID | [Discord Developer Portal](https://discord.com/developers/applications) で取得 |
| `DISCORD_CLIENT_SECRET` | Discord OAuth クライアントシークレット | 同上 |
| `PULSOID_CLIENT_ID` | Pulsoid OAuth クライアント ID | [Pulsoid Developer](https://pulsoid.net/ui/keys) で取得 |
| `PULSOID_CLIENT_SECRET` | Pulsoid OAuth クライアントシークレット | 同上 |
| `PULSOID_REDIRECT_URI` | Pulsoid OAuth コールバック URI | 例: `https://yourdomain.com/api/oauth/pulsoid/callback` |
| `TOKEN_ENCRYPTION_KEY` | Pulsoid トークン暗号化キー (AES-256) | `openssl rand -base64 32` |
| `CLOUDFLARE_TUNNEL_TOKEN` | Cloudflare Tunnel トークン (本番のみ) | [Cloudflare Zero Trust](https://one.dash.cloudflare.com/) で取得 |

> **Note:** `DATABASE_URL`, `REDIS_URL`, `RUST_LOG`, `AUTH_TRUST_HOST`, `HOSTNAME` は docker-compose.yml 内で自動設定されるため、`.env` への記載は不要です。

### 2. Docker で起動 (推奨)

```bash
docker compose up --build
```

http://localhost:3000 でアクセスできます。

### 3. 本番環境 (Cloudflare Tunnel)

```bash
docker compose -f docker-compose.yml --profile prod up --build
```

> `-f docker-compose.yml` を明示指定することで `docker-compose.override.yml` の読み込みをスキップし、Nginx ポートをホストに公開しません (Docker 内部ネットワーク + Cloudflare Tunnel のみ)。

### 4. ローカル開発

```bash
# Backend (api-backend)
cd backend
DATABASE_URL=postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor \
REDIS_URL=redis://localhost:6379 \
NATS_URL=nats://localhost:4222 \
AUTH_URL=http://localhost:3000 \
cargo run -p api-backend

# Frontend
cd frontend
npm run dev
```

> `AUTH_URL` は `/api/ws/*` の Origin ヘッダ検証に使用されます。release ビルドでは未設定だと起動時に panic します (fail-closed)。debug ビルドでは `http://localhost:3000` にフォールバックしますが、フロントエンドを別オリジンで動かす場合は必ず一致させてください。

## Docker サービス一覧

| サービス | イメージ | 説明 |
|----------|---------|------|
| `timescaledb` | `timescale/timescaledb:2.26.1-pg17` | TimescaleDB (PostgreSQL 17) |
| `redis` | `redis:8.6.2-alpine` | Redis (最新心拍数キャッシュ) |
| `backend` | ビルド: `./backend` | Rust API サーバー |
| `frontend` | ビルド: `./frontend` | Next.js (standalone) |
| `nginx` | ビルド: `./nginx` | リバースプロキシ + 静的ファイル配信 |
| `cloudflared` | `cloudflare/cloudflared:latest` | Cloudflare Tunnel (prod プロファイル) |

## API エンドポイント

### REST API

| メソッド | パス | 説明 |
|---------|------|------|
| `GET` | `/api/users` | ユーザー一覧 |
| `PATCH` | `/api/users/{id}` | ユーザー更新 |
| `GET` | `/api/users/{id}/pulsoid-token` | Pulsoid トークン取得 |
| `PUT` | `/api/users/{id}/pulsoid-token` | Pulsoid トークン更新 |
| `DELETE` | `/api/users/{id}/pulsoid-token` | Pulsoid トークン削除 |
| `GET` | `/api/users/{id}/heart-rates?period=` | 心拍データ取得 (期間指定) |
| `GET` | `/api/users/{id}/heart-rates/by-date?date=` | 日付指定の心拍データ |
| `GET` | `/api/users/{id}/heart-rates/daily-stats?from=&to=` | 日別統計 |

### Pulsoid OAuth

| メソッド | パス | 説明 |
|---------|------|------|
| `POST` | `/api/oauth/pulsoid/connect` | Pulsoid 接続リクエスト作成 |
| `GET` | `/api/oauth/pulsoid/connect/{request_id}` | Pulsoid 認可画面へリダイレクト |
| `GET` | `/api/oauth/pulsoid/callback` | OAuth コールバック処理 |

### WebSocket

| パス | 説明 |
|------|------|
| `/api/ws/heart-rates` | リアルタイム心拍データ (subscribe/unsubscribe でユーザー選択) |

## ドキュメント

- [API 仕様](docs/api.md)
- [アーキテクチャ](docs/architecture.md)
- [DB スキーマ](docs/schema.sql)

## ライセンス

[MIT](LICENSE.md)
