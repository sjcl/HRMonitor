# HRMonitor

Pulsoid WebSocket から心拍数データを収集し、TimescaleDB に保存、Next.js フロントエンドでグラフ表示する心拍モニタリングシステム。

## プロジェクト構成

```
backend/
  common/             共有型 crate (NATS メッセージ型, TokenEncryption, PulsoidOAuthConfig, web feature で auth/access/error)
  api-backend/        Rust (axum + sqlx + Redis + NATS) HTTP API 専用 — ポート 3001
  ws-gateway/         Rust (axum ws) WebSocket 配信専用 — ポート 3002
  pulsoid-ingest/     Pulsoid WS ingest サービス (NATS)
  pulsoid-refresher/  OAuth token 定期リフレッシュサービス (DB スキャン + NATS)
  migration/          DB マイグレーション runner
frontend/           Next.js (App Router + TanStack Query + Recharts) — ポート 3000
nginx/              nginx (リバースプロキシ + 静的ファイル配信) — ポート 80
docs/               仕様書 (API, アーキテクチャ, スキーマ, エージェントプロンプト)
```

## 技術スタック

### Backend (api-backend)
- Rust (edition 2024), axum 0.8, tokio, sqlx (PostgreSQL), async-nats, redis
- DB: TimescaleDB (PostgreSQL)、マイグレーションは専用 migration crate が実行
- Redis: latest heart rate キャッシュ (`latest_bpm:{user_id}`)
- heart_rate_records は TimescaleDB hypertable (recorded_at でパーティション)
- NATS publish 専用: `pulsoid.connection.changed` (OAuth callback / manual token PUT/DELETE 時)
- OAuth 初回認可 (code 交換) と manual token PUT は api-backend が担当。token refresh は pulsoid-refresher が所有
- WebSocket 配信には関与しない (ws-gateway に分離)

### WS Gateway (ws-gateway)
- axum 0.8 (ws feature) WebSocket 配信専用プロセス。HTTP API の再起動で WS が切れないよう分離
- NATS `hr.received` を subscribe → `tokio::sync::broadcast` → WS クライアントに push
- 起動時に DB から `latest_bpm:*` を `SET NX EX` で warm-up (pulsoid-ingest 書き込みを上書きしない)
- `/api/ws/me`, `/api/ws/users/{id}`, `/api/ws/groups/{id}` を配信
- `AUTH_URL` 由来の Origin allowlist と `require_auth` (Cookie → sessions テーブル) は api-backend と同じ実装を common::auth から共有

### Pulsoid Ingest (pulsoid-ingest)
- Pulsoid WS ワーカー: ユーザーごとに1つ spawned、指数バックオフでリトライ
- 心拍データ: DB 書き込み → NATS `hr.received` publish
- OAuth token 期限は passive に検知: 期限に近い行への WS 接続を見送り、pulsoid-refresher が `revision` を bump したら自然に世代交代
- 定期 DB reconciliation (60秒) で connection.changed ロストを補完
- ユーザー:Pulsoidトークンは 1:1 (pulsoid_connections テーブル)

### Pulsoid Refresher (pulsoid-refresher)
- 60 秒ごとに `pulsoid_connections` をスキャンし、`token_expires_at` が 300 秒以内に迫った OAuth 行を事前リフレッシュ
- 単一インスタンス運用を推奨。Postgres advisory lock (`pg_try_advisory_xact_lock`) で cross-process dedup を担保するので redeploy 時の一瞬の二重起動は安全
- リフレッシュ成功時は `revision` を bump し `pulsoid.connection.changed` を publish。pulsoid-ingest が拾って worker を差し替える
- リフレッシュ失敗時は既存の sticky-error invariant に従い `connection_state = 'error'` に遷移 (401 / `invalid_grant` のみ terminal)

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

# WS Gateway
cd backend && cargo run -p ws-gateway
# DATABASE_URL, REDIS_URL, NATS_URL, AUTH_URL 環境変数
# AUTH_URL は /api/ws/* の Origin ヘッダ検証に使用。release build では未設定で panic、debug build では http://localhost:3000 にフォールバック

# Pulsoid Ingest
cd backend && cargo run -p pulsoid-ingest
# DATABASE_URL, NATS_URL, TOKEN_ENCRYPTION_KEY 環境変数

# Pulsoid Refresher
cd backend && cargo run -p pulsoid-refresher
# DATABASE_URL, NATS_URL, TOKEN_ENCRYPTION_KEY, PULSOID_CLIENT_ID, PULSOID_CLIENT_SECRET 環境変数

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
- `WS /api/ws/me`, `WS /api/ws/users/{id}`, `WS /api/ws/groups/{id}`

## アーキテクチャ要点

- 認証: Auth.js v5 (Discord OAuth) + データベースセッション戦略
  - Frontend (Next.js) が OAuth フロー処理、セッションを PostgreSQL に保存
  - Backend (Rust) は Cookie からセッショントークンを読み、sessions テーブルで認証
  - `/api/auth/*` は nginx が frontend にプロキシ、`/api/ws/*` は ws-gateway、他の `/api/*` は backend にプロキシ
  - users (1:N) accounts (1:N) sessions のリレーション
- Backend, ws-gateway, Frontend は Docker 内部ネットワーク限定 (expose のみ、ports なし)
- nginx が唯一のパブリックエントリポイント (静的ファイル配信 + リバースプロキシ)
- cloudflared トンネルで nginx を公開
- サービス間通信: NATS (Core NATS, JetStream 不使用)
  - `hr.received`: pulsoid-ingest → ws-gateway (心拍データ、WS broadcast 用)
  - `pulsoid.connection.changed`: api-backend / pulsoid-refresher → pulsoid-ingest (トークン変更通知)
- OAuth token refresh は pulsoid-refresher が DB スキャンで proactive に実行 (NATS 要求経路は廃止)
- Latest heart rate は WebSocket でリアルタイム配信 (NATS → Redis → broadcast → WS push、ws-gateway 担当)
- `/api/ws/*` の Origin ヘッダは `AUTH_URL` 由来の単一オリジンと完全一致でなければ 403 (ws-gateway 内で require_auth より前に実行)
- 心拍更新フロー: Pulsoid WS → DB保存 → pulsoid-ingest Redis 更新 → NATS publish → ws-gateway broadcast → 購読クライアントにpush
- DB マイグレーション: 専用 migration crate (docker-compose で service_completed_successfully 依存)
- 共通 web コード (error / auth / access) は `common` crate の `web` feature 配下。`AppError` / `AuthConfig` / `require_auth` / `AuthContext` / `UserIdParam` / `ensure_can_view_user` / `ensure_active_member` を api-backend と ws-gateway が共有利用

## 実装状況

- [x] Backend: DB初期化、モデル、エラーハンドリング
- [x] Backend: 全 API エンドポイント (users, pulsoid-token, heart_rates, ws)
- [x] WS Gateway: Redis キャッシュ warm-up + WebSocket 配信 (NATS 経由) を api-backend から分離
- [x] Pulsoid Ingest: Pulsoid WebSocket ワーカー + WorkerManager + reconciliation
- [x] Pulsoid Ingest: OAuth token 期限の passive 検知 (WS 接続見送り + revision 世代交代)
- [x] Pulsoid Refresher: 60 秒定期スキャン + advisory lock + proactive OAuth refresh
- [x] Frontend: ユーザー一覧ページ (/users) — WS でリアルタイム BPM
- [x] Frontend: ユーザー詳細ページ (/users/[id]) — グラフ、トークン管理、WS
- [x] nginx: リバースプロキシ (HTTP API, WebSocket) + 静的ファイル配信
- [x] Docker: Dockerfile (api-backend/ws-gateway/pulsoid-ingest/pulsoid-refresher/migration/frontend/nginx), docker-compose.yml, cloudflared, redis, nats
- [x] Auth: Discord OAuth (Auth.js v5) + カスタムアダプター + DB セッション
- [x] Auth: Backend 認証ミドルウェア (Cookie → sessions テーブル)
- [x] Auth: ログインページ、ナビバー、ルート保護 (middleware.ts)
- [x] Auth: nginx /api/auth/ ルーティング + X-Forwarded-Proto 修正
- [ ] README.md 更新
- [ ] E2E テスト (実際の Pulsoid トークンで動作確認)
