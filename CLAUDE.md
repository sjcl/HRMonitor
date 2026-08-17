# HRMonitor

Pulsoid WebSocket から心拍数データを収集し、TimescaleDB に保存、React SPA でグラフ表示する心拍モニタリングシステム。認証は Rust バックエンドに集約 (Discord OAuth + Ed25519 JWT + Redis リフレッシュセッション)。

## プロジェクト構成

```
backend/
  common/             共有 crate (NATS メッセージ型, TokenEncryption, PulsoidOAuthConfig / DiscordOAuthConfig,
                      jwt feature で Claims/JwtVerifier/JwtSigner, web feature で auth/access/error/origin)
  api-backend/        Rust (axum + sqlx + Redis + NATS) HTTP API + 認証 — ポート 3001
  ws-gateway/         Rust (axum ws) WebSocket 配信専用 — ポート 3002
  pulsoid-ingest/     Pulsoid WS ingest サービス (NATS)
  pulsoid-refresher/  OAuth token 定期リフレッシュサービス (DB スキャン + NATS)
  migration/          DB マイグレーション runner
frontend/           Vite + React SPA (React Router + TanStack Query + Recharts) — ビルド成果物のみ
nginx/              nginx (SPA ビルド + 配信 + リバースプロキシ) — ポート 80
docs/               仕様書 (API, アーキテクチャ, スキーマ, エージェントプロンプト)
```

## 技術スタック

### Backend (api-backend)
- Rust (edition 2024), axum 0.8, tokio, sqlx (PostgreSQL), async-nats, redis
- DB: TimescaleDB (PostgreSQL)、マイグレーションは専用 migration crate が実行
- Redis: latest heart rate キャッシュ (`latest_bpm:{user_id}`) + リフレッシュセッション (`auth:session:v1:{sid}`)
- heart_rate_records は TimescaleDB hypertable (recorded_at でパーティション)
- NATS publish 専用: `pulsoid.connection.changed` (OAuth callback / manual token PUT/DELETE 時)
- OAuth 初回認可 (code 交換) と manual token PUT は api-backend が担当。token refresh は pulsoid-refresher が所有
- WebSocket 配信には関与しない (ws-gateway に分離)
- **認証の所有者**: Discord OAuth (PKCE + nonce 束縛)、Ed25519 JWT 発行、Redis リフレッシュセッションの回転・失効
- `gen-keys` / `gen-jwt-key --kid` サブコマンドで鍵を生成できる

### WS Gateway (ws-gateway)
- axum 0.8 (ws feature) WebSocket 配信専用プロセス。HTTP API の再起動で WS が切れないよう分離
- NATS `hr.received` を subscribe → `tokio::sync::broadcast` → WS クライアントに push
- 起動時に DB から `latest_bpm:*` を `SET NX EX` で warm-up (pulsoid-ingest 書き込みを上書きしない)
- `/api/ws/me`, `/api/ws/users/{id}`, `/api/ws/groups/{id}` を配信
- `PUBLIC_ORIGIN` 由来の Origin allowlist (`common::origin`) と `require_auth` (Cookie → JWT ローカル検証) を api-backend と共有
- `common` の feature は `jwt` のみ有効。`jwt-issue` を付けないので、**秘密鍵を読むコードがこのバイナリには存在しない**
- アクセストークンの `exp` に達したら close code `4401` で切断する (SPA がリフレッシュして再接続)

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
- Vite 8, React 19, TypeScript 6 — **SPA。本番に Node.js プロセスは無い** (nginx イメージのビルド段階で `dist/` を生成)
- React Router 8 (ライブラリモード)。`ProtectedRoute` は UI 制御のみで、認可の強制は Rust 側
- @tanstack/react-query 5 (ユーザーメタデータ取得)
- recharts 3 (心拍グラフ)
- Tailwind CSS 4 (`@tailwindcss/vite`)
- Vitest + Testing Library, ESLint (typescript-eslint + react-hooks)
- `src/lib/http.ts` が唯一の API クライアント。401 → single-flight リフレッシュ → 1 回だけ再試行。
  リフレッシュが 401 のときだけ `/login` へ、5xx / ネットワーク失敗は一時障害として再試行する
- `useHeartRateWs` 系フックで latest heart rate をリアルタイム受信。接続前に `/api/auth/session` で鮮度を確認し、
  close code 4401 を受けたらリフレッシュしてから再接続
- localStorage / sessionStorage は使わない (トークンは HttpOnly Cookie)
- nginx がすべてのプロキシ (HTTP API, WebSocket, Auth) と SPA 配信を担当

## 開発コマンド

```bash
# Backend (api-backend)
cd backend && cargo run -p api-backend
# DATABASE_URL, REDIS_URL, NATS_URL, PUBLIC_ORIGIN, JWT_*, REFRESH_*, DISCORD_* 環境変数
# 鍵生成: cargo run -p api-backend -- gen-keys
# 鍵ローテーション: cargo run -p api-backend -- gen-jwt-key --kid k2

# WS Gateway
cd backend && cargo run -p ws-gateway
# DATABASE_URL, REDIS_URL, NATS_URL, PUBLIC_ORIGIN, JWT_ISSUER, JWT_AUDIENCE, JWT_PUBLIC_KEYS 環境変数
# PUBLIC_ORIGIN は Origin ヘッダ検証と Cookie 属性の決定に使用。未設定なら起動時 panic (fail-closed)

# Pulsoid Ingest
cd backend && cargo run -p pulsoid-ingest
# DATABASE_URL, NATS_URL, TOKEN_ENCRYPTION_KEY 環境変数

# Pulsoid Refresher
cd backend && cargo run -p pulsoid-refresher
# DATABASE_URL, NATS_URL, TOKEN_ENCRYPTION_KEY, PULSOID_CLIENT_ID, PULSOID_CLIENT_SECRET 環境変数

# Migration
cd backend && cargo run -p migration
# DATABASE_URL 環境変数

# Frontend (Vite dev server。/api は :3001、/api/ws は :3002 へプロキシ)
cd frontend && npm run dev
# lint / test / build: npm run lint && npm run test && npm run build

# Docker (requires Docker Compose v2.20.0+)
docker compose up --build
```

## API エンドポイント

- `GET /api/auth/login/discord?return_to=&tz=`, `GET /api/auth/callback/discord`
- `POST /api/auth/refresh`, `POST /api/auth/logout`, `GET /api/auth/session`
- `GET /api/users/{id}` (閲覧、`{id}` に `me` 可), `PATCH /api/users/me`
- `GET/PUT/DELETE /api/users/me/pulsoid-token`
- `GET /api/users/{id}/heart-rates?period=`, `GET /api/users/{id}/heart-rates/by-date?date=` (`{id}` に `me` 可)
- `GET /api/users/{id}/heart-rates/daily-stats?date=`, `GET /api/users/{id}/heart-rates/minute-stats?period=`
- `WS /api/ws/me`, `WS /api/ws/users/{id}`, `WS /api/ws/groups/{id}`

## アーキテクチャ要点

- 認証: Rust に集約。Discord OAuth (Authorization Code + PKCE S256) → Ed25519 JWT + Redis リフレッシュセッション
  - アクセス JWT: 30 分、`__Host-hrmonitor_session`。**検証はローカル署名検証のみで DB/Redis を参照しない**
  - リフレッシュ: 30 日、`__Host-hrmonitor_refresh`、値は `{sid}.{secret}`。Redis には HMAC のみ保存
  - `/api/auth/*` も含め全 `/api/*` (WS 以外) は nginx が backend にプロキシ、`/api/ws/*` は ws-gateway
  - OAuth `state` はブラウザ nonce Cookie (`__Host-hrmonitor_oauth`) と束縛 (RFC 9700 §2.1、login CSRF 対策)
  - リフレッシュは毎回ローテーション。直前世代には 10 秒の grace 窓があり、勝者の secret を AEAD 封印して
    Redis に置くことで「回転成功・レスポンス消失」からも復旧できる
  - grace を過ぎた旧世代の提示のみ再利用検知として失効。未知の secret は失効させない (sid は秘密ではないため DoS になる)
  - ログアウトは有効な JWT を要求しない。Redis 削除に失敗したら 503 を返し Cookie を残す
  - **認可** (グループ所属・心拍公開範囲) は従来どおりリクエストごとに DB を参照する
  - users (1:N) accounts のリレーション。`accounts (provider, provider_account_id)` で既存ユーザーを引き当てる
  - `sessions` テーブルは未使用のまま残置 (削除は別 PR)
- Backend, ws-gateway は Docker 内部ネットワーク限定 (expose のみ、ports なし)
- nginx が唯一のパブリックエントリポイント (SPA ビルド + 配信 + リバースプロキシ)
  - `add_header` は継承されないため、セキュリティヘッダは `nginx/security-headers.conf` を各 location で include する
- cloudflared トンネルで nginx を公開
- サービス間通信: NATS (Core NATS, JetStream 不使用)
  - `hr.received`: pulsoid-ingest → ws-gateway (心拍データ、WS broadcast 用)
  - `pulsoid.connection.changed`: api-backend / pulsoid-refresher → pulsoid-ingest (トークン変更通知)
- OAuth token refresh は pulsoid-refresher が DB スキャンで proactive に実行 (NATS 要求経路は廃止)
- Latest heart rate は WebSocket でリアルタイム配信 (NATS → Redis → broadcast → WS push、ws-gateway 担当)
- Origin 検証は `common::origin`。api-backend は unsafe メソッドのみ (`require_origin_unsafe`)、ws-gateway は全リクエスト (`require_origin_always`)
  - `PUBLIC_ORIGIN` と完全一致しなければ 403。Origin 欠落も 403 (fail-closed)。CORS レイヤーは無い
  - GET の OAuth コールバックは safe メソッドなので自然に対象外 (CSRF 対策は state + nonce + PKCE が担う)
  - axum 0.8 では**最後に付けた `.layer()` が最外**。Origin 検証を後から付けて認証より先に走らせている
- 心拍更新フロー: Pulsoid WS → DB保存 → pulsoid-ingest Redis 更新 → NATS publish → ws-gateway broadcast → 購読クライアントにpush
- DB マイグレーション: 専用 migration crate (docker-compose で service_completed_successfully 依存)
- 共通 web コード (error / auth / access / origin) は `common` crate の `web` feature 配下。`AppError` / `AuthConfig` / `require_auth` / `AuthContext` / `UserIdParam` / `ensure_can_view_user` / `ensure_active_member` / `OriginContext` を api-backend と ws-gateway が共有利用
- `common` の feature 分割: `jwt` (検証のみ) と `jwt-issue` (署名)。ws-gateway は前者だけを有効にするため、
  秘密鍵を扱うコードがバイナリに含まれない (規約ではなくコンパイル時の保証)

## 実装状況

- [x] Backend: DB初期化、モデル、エラーハンドリング
- [x] Backend: 全 API エンドポイント (users, pulsoid-token, heart_rates, ws)
- [x] WS Gateway: Redis キャッシュ warm-up + WebSocket 配信 (NATS 経由) を api-backend から分離
- [x] Pulsoid Ingest: Pulsoid WebSocket ワーカー + WorkerManager + reconciliation
- [x] Pulsoid Ingest: OAuth token 期限の passive 検知 (WS 接続見送り + revision 世代交代)
- [x] Pulsoid Refresher: 60 秒定期スキャン + advisory lock + proactive OAuth refresh
- [x] Frontend: 自分のページ (/me) — WS でリアルタイム BPM
- [x] Frontend: ユーザー詳細ページ (/users/[id]) — グラフ、トークン管理、WS
- [x] nginx: リバースプロキシ (HTTP API, WebSocket) + 静的ファイル配信
- [x] Docker: Dockerfile (api-backend/ws-gateway/pulsoid-ingest/pulsoid-refresher/migration/nginx), docker-compose.yml, cloudflared, redis (AOF), nats
- [x] Auth: Discord OAuth (PKCE + nonce 束縛) を Rust に集約
- [x] Auth: Ed25519 JWT 発行・ローカル検証 (JWK Set 配布、起動時に鍵の整合性を検証)
- [x] Auth: Redis リフレッシュセッション (回転 / grace / 再利用検知 / レスポンス消失からの復旧)
- [x] Auth: ログインページ、ナビバー、ルート保護 (SPA 側は UI 制御のみ)
- [x] Auth: nginx /api/auth/ を backend へルーティング + セキュリティヘッダ
- [x] Frontend: Next.js → Vite SPA 移行 (本番から Node.js を排除)
- [ ] README.md 更新
- [ ] E2E テスト (実際の Pulsoid トークンで動作確認)
