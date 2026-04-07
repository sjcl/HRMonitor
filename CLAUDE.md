# HRMonitor

Pulsoid WebSocket から心拍数データを収集し、TimescaleDB に保存、Next.js フロントエンドでグラフ表示する心拍モニタリングシステム。

## プロジェクト構成

```
apps/backend/    Rust (axum + sqlx + TimescaleDB) — ポート 3001
apps/frontend/   Next.js (App Router + TanStack Query + Recharts) — ポート 3000
apps/nginx/      nginx (リバースプロキシ + 静的ファイル配信) — ポート 80
docs/            仕様書 (API, アーキテクチャ, スキーマ, エージェントプロンプト)
```

## 技術スタック

### Backend
- Rust (edition 2024), axum 0.8 (ws feature), tokio, sqlx (PostgreSQL), tokio-tungstenite, redis
- DB: TimescaleDB (PostgreSQL), 起動時に `sqlx::migrate!()` でマイグレーション適用
- Redis: latest heart rate キャッシュ (`latest_bpm:{user_id}`)
- heart_rate_records は TimescaleDB hypertable (recorded_at でパーティション)
- Pulsoid WS ワーカー: ユーザーごとに1つ spawned、指数バックオフでリトライ
- ユーザー:Pulsoidトークンは 1:1 (users テーブルに直接格納)
- WebSocket配信: `tokio::sync::broadcast` で in-process pub/sub、`/api/ws/heart-rates` で購読

### Frontend
- Next.js 16, React 19, TypeScript
- Auth.js v5 (next-auth@5 beta) — Discord OAuth + カスタム PostgreSQL アダプター
- @tanstack/react-query 5 (ユーザーメタデータ取得)
- recharts 3 (心拍グラフ)
- Tailwind CSS 4
- `useHeartRateWs` フックで latest heart rate をリアルタイム受信
- nginx がすべてのプロキシ (HTTP API, WebSocket, Auth) と静的ファイル配信を担当

## 開発コマンド

```bash
# Backend
cd apps/backend && cargo run
# DATABASE_URL 環境変数、デフォルト postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor

# Frontend
cd apps/frontend && npm run dev
# BACKEND_URL 環境変数で backend を指定 (デフォルト http://backend:3001)

# Docker
docker compose up --build
```

## API エンドポイント

- `GET /api/users/{id}` (閲覧、`{id}` に `me` 可), `PATCH /api/users/me`
- `GET/PUT/DELETE /api/users/me/pulsoid-token`
- `GET /api/users/{id}/heart-rates?period=`, `GET /api/users/{id}/heart-rates/by-date?date=` (`{id}` に `me` 可)
- `GET /api/users/{id}/heart-rates/daily-stats?date=`, `GET /api/users/{id}/heart-rates/minute-stats?period=`
- `GET /api/users/{id}/latest-heart-rate` (Redis優先、DBフォールバック)
- `WS /api/ws/me`, `WS /api/ws/users/{id}`, `WS /api/ws/groups/{id}`

## アーキテクチャ要点

- 認証: Auth.js v5 (Discord OAuth) + データベースセッション戦略
  - Frontend (Next.js) が OAuth フロー処理、セッションを PostgreSQL に保存
  - Backend (Rust) は Cookie からセッショントークンを読み、sessions テーブルで認証
  - `/api/auth/*` は nginx が frontend にプロキシ、他の `/api/*` は backend にプロキシ
  - users (1:N) accounts (1:N) sessions のリレーション
- Backend, Frontend は Docker 内部ネットワーク限定 (expose のみ、ports なし)
- nginx が唯一のパブリックエントリポイント (静的ファイル配信 + リバースプロキシ)
- cloudflared トンネルで nginx を公開
- Latest heart rate は WebSocket でリアルタイム配信 (Redis キャッシュ → broadcast → WS push)
- 心拍更新フロー: Pulsoid WS → DB保存 → Redis更新 → broadcast → 購読クライアントにpush

## 実装状況

- [x] Backend: DB初期化、モデル、エラーハンドリング
- [x] Backend: 全 API エンドポイント (users, pulsoid-token, heart_rates, ws)
- [x] Backend: Pulsoid WebSocket ワーカー + WorkerManager
- [x] Backend: Redis キャッシュ + WebSocket 配信
- [x] Frontend: ユーザー一覧ページ (/users) — WS でリアルタイム BPM
- [x] Frontend: ユーザー詳細ページ (/users/[id]) — グラフ、トークン管理、WS
- [x] nginx: リバースプロキシ (HTTP API, WebSocket) + 静的ファイル配信
- [x] Docker: Dockerfile (backend/frontend/nginx), docker-compose.yml, cloudflared, redis
- [x] Auth: Discord OAuth (Auth.js v5) + カスタムアダプター + DB セッション
- [x] Auth: Backend 認証ミドルウェア (Cookie → sessions テーブル)
- [x] Auth: ログインページ、ナビバー、ルート保護 (middleware.ts)
- [x] Auth: nginx /api/auth/ ルーティング + X-Forwarded-Proto 修正
- [ ] README.md 更新
- [ ] E2E テスト (実際の Pulsoid トークンで動作確認)
