# HRMonitor

Pulsoid WebSocket から心拍数データを収集し、TimescaleDB に保存、Next.js フロントエンドでグラフ表示する心拍モニタリングシステム。

## プロジェクト構成

```
backend/
  common/           共有型 crate (NATS メッセージ型, TokenEncryption)
  api-backend/      Rust (axum + sqlx + Redis + NATS) — ポート 3001
  pulsoid-ingest/   Pulsoid WS ingest サービス (NATS)
  migration/        DB マイグレーション runner
frontend/           Next.js (App Router + TanStack Query + Recharts) — ポート 3000
nginx/              nginx (リバースプロキシ + 静的ファイル配信) — ポート 80
docs/               仕様書 (API, アーキテクチャ, スキーマ, エージェントプロンプト)
```

## 技術スタック

### Backend (api-backend)
- Rust (edition 2024), axum 0.8 (ws feature), tokio, sqlx (PostgreSQL), async-nats, redis
- DB: TimescaleDB (PostgreSQL)、マイグレーションは専用 migration crate が実行
- Redis: latest heart rate キャッシュ (`latest_bpm:{user_id}`)
- heart_rate_records は TimescaleDB hypertable (recorded_at でパーティション)
- NATS で pulsoid-ingest と通信 (hr.received, pulsoid.connection.changed, pulsoid.token.refresh_needed)
- OAuth token refresh は api-backend が実行 (pulsoid-ingest からの要求に応じて)
- WebSocket配信: NATS `hr.received` → Redis 更新 → `tokio::sync::broadcast` → WS push

### Pulsoid Ingest (pulsoid-ingest)
- Pulsoid WS ワーカー: ユーザーごとに1つ spawned、指数バックオフでリトライ
- 心拍データ: DB 書き込み → NATS `hr.received` publish
- OAuth token 失効検知 → NATS `pulsoid.token.refresh_needed` publish (refresh は api-backend が実行)
- 定期 DB reconciliation (60秒) で connection.changed ロストを補完
- ユーザー:Pulsoidトークンは 1:1 (pulsoid_connections テーブル)

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
# Backend (api-backend)
cd backend && cargo run -p api-backend
# DATABASE_URL, REDIS_URL, NATS_URL 環境変数

# Pulsoid Ingest
cd backend && cargo run -p pulsoid-ingest
# DATABASE_URL, NATS_URL, TOKEN_ENCRYPTION_KEY 環境変数

# Migration
cd backend && cargo run -p migration
# DATABASE_URL 環境変数

# Frontend
cd frontend && npm run dev

# Docker (requires Docker Compose v2.20.0+)
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
- サービス間通信: NATS (Core NATS, JetStream 不使用)
  - `hr.received`: pulsoid-ingest → api-backend (心拍データ)
  - `pulsoid.connection.changed`: api-backend → pulsoid-ingest (トークン変更通知)
  - `pulsoid.token.refresh_needed`: pulsoid-ingest → api-backend (OAuth token refresh 要求)
- Latest heart rate は WebSocket でリアルタイム配信 (NATS → Redis → broadcast → WS push)
- 心拍更新フロー: Pulsoid WS → DB保存 → NATS publish → api-backend → Redis更新 → broadcast → 購読クライアントにpush
- DB マイグレーション: 専用 migration crate (docker-compose で service_completed_successfully 依存)

## 実装状況

- [x] Backend: DB初期化、モデル、エラーハンドリング
- [x] Backend: 全 API エンドポイント (users, pulsoid-token, heart_rates, ws)
- [x] Backend: Redis キャッシュ + WebSocket 配信 (NATS 経由)
- [x] Pulsoid Ingest: Pulsoid WebSocket ワーカー + WorkerManager + reconciliation
- [x] Pulsoid Ingest: OAuth token 失効検知 → NATS refresh 要求
- [x] Backend: Token refresh subscriber (NATS)
- [x] Frontend: ユーザー一覧ページ (/users) — WS でリアルタイム BPM
- [x] Frontend: ユーザー詳細ページ (/users/[id]) — グラフ、トークン管理、WS
- [x] nginx: リバースプロキシ (HTTP API, WebSocket) + 静的ファイル配信
- [x] Docker: Dockerfile (api-backend/pulsoid-ingest/migration/frontend/nginx), docker-compose.yml, cloudflared, redis, nats
- [x] Auth: Discord OAuth (Auth.js v5) + カスタムアダプター + DB セッション
- [x] Auth: Backend 認証ミドルウェア (Cookie → sessions テーブル)
- [x] Auth: ログインページ、ナビバー、ルート保護 (middleware.ts)
- [x] Auth: nginx /api/auth/ ルーティング + X-Forwarded-Proto 修正
- [ ] README.md 更新
- [ ] E2E テスト (実際の Pulsoid トークンで動作確認)
