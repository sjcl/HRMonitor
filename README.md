# HRMonitor

Pulsoid WebSocket から心拍数データをリアルタイム収集し、TimescaleDB に保存、WebSocket でブラウザにプッシュ配信する心拍モニタリングシステム。Discord OAuth によるユーザー認証付き。

## プロジェクト構成

```
backend/
  common/             共有 crate (JWT, Origin 検証, OAuth, NATS メッセージ型, トークン暗号化)
  api-backend/        HTTP API + 認証 (Discord OAuth / JWT 発行 / リフレッシュ) — ポート 3001
  ws-gateway/         WebSocket 配信専用 — ポート 3002
  pulsoid-ingest/     Pulsoid WS ingest サービス
  pulsoid-refresher/  Pulsoid OAuth トークン定期リフレッシュ
  migration/          DB マイグレーション runner
frontend/           Vite + React SPA (ビルド成果物のみを配信。本番に Node.js は常駐しない)
nginx/              Nginx (SPA 配信 + リバースプロキシ) — ポート 80
docs/               仕様書 (API, アーキテクチャ, スキーマ)
```

## 技術スタック

### Backend

- Rust (edition 2024), axum 0.8, tokio, sqlx 0.8 (PostgreSQL)
- TimescaleDB (PostgreSQL 18) — `heart_rate_records` は hypertable
- Redis — 最新心拍数キャッシュ + **リフレッシュセッション**
- NATS (Core NATS) — サービス間通信
- 認証: Ed25519 (EdDSA) JWT + Redis リフレッシュセッション
- Pulsoid OAuth トークン暗号化: AES-256-GCM

### Frontend

- Vite 8, React 19, TypeScript 6
- React Router 8 (ライブラリモード)
- TanStack React Query 5 (データ取得)
- Recharts 3 (心拍グラフ)
- Tailwind CSS 4
- Vitest + Testing Library, ESLint

## アーキテクチャ

```
ブラウザ
  ↓ HTTP / WebSocket
[Nginx :80] ─── 唯一の公開エントリポイント
  ├── /assets/*       → SPA の静的アセット (365日 immutable キャッシュ)
  ├── /api/auth/*     → [api-backend :3001] (Discord OAuth / JWT / リフレッシュ)
  ├── /api/ws/*       → [ws-gateway :3002]  (WebSocket upgrade)
  ├── /api/*          → [api-backend :3001] (REST API)
  ├── /healthz        → [api-backend :3001]
  └── /*              → index.html (SPA フォールバック, no-cache)

[pulsoid-ingest] ←── WebSocket ──→ Pulsoid API
  ├── DB 保存 → TimescaleDB
  ├── キャッシュ更新 → Redis
  └── NATS publish → [ws-gateway] → 接続中のブラウザへプッシュ
```

## 認証

### 全体像

| | アクセストークン | リフレッシュトークン |
|---|---|---|
| 形式 | Ed25519 (EdDSA) JWT | CSPRNG 32 バイト |
| Cookie | `__Host-hrmonitor_session` | `__Host-hrmonitor_refresh` |
| 有効期限 | 30 分 | 30 日 (回転しても延長されない) |
| 検証方法 | **ローカル署名検証のみ** (DB / Redis 参照なし) | Redis 上の HMAC-SHA256 と照合 |

Cookie 属性は本番で `Secure; HttpOnly; SameSite=Lax; Path=/`、`Domain` なし。

**通常の認証処理は PostgreSQL にも Redis にも一切アクセスしません。** JWT の署名検証だけで完結します。一方、グループ所属や心拍公開範囲といった「最新状態が必要な認可」は、これまでどおりリクエストごとに DB を参照します。

### Discord OAuth ログイン

1. `GET /api/auth/login/discord` — PKCE (S256) と 1 回限りの `state` を生成し Redis に 5 分保存。ブラウザ用 nonce を `__Host-hrmonitor_oauth` Cookie に発行してから Discord へリダイレクト。
2. `GET /api/auth/callback/discord` — nonce Cookie と Redis 上のハッシュを照合してから `state` を `GETDEL` で消費し、認可コードを交換。`identify` スコープのみを要求し、**Discord のトークンは保存しません**。
3. `users` / `accounts` を upsert して JWT + リフレッシュ Cookie を発行。

nonce Cookie による束縛は RFC 9700 §2.1 が求めるもので、これがないと攻撃者が開始したログインのコールバック URL を被害者に踏ませる login CSRF を防げません。

### リフレッシュとローテーション

`POST /api/auth/refresh` はリフレッシュトークンを毎回ローテーションします。直前世代のトークンには 10 秒の猶予窓があり、次の 2 つの正当なケースを吸収します。

- 複数タブが同時にリフレッシュした場合
- ローテーションは成功したがレスポンスがブラウザに届かなかった場合

後者のために、新しいシークレットの AES-256-GCM 暗号文を猶予窓と同じ寿命の Redis キーに保存し、猶予経路で復号して**勝者と同一のトークン**を返します。これがないと、レスポンスを失ったクライアントは 30 分後のリフレッシュで再利用検知に当たり、再ログインを強いられます。

猶予窓を過ぎた旧世代トークンの提示は**再利用**とみなしてセッションを削除します。一方、どの世代にも一致しないシークレットはセッションを削除せず 401 のみを返します — `sid` は秘密ではないため、任意の不正値で失効できると他人を強制ログアウトさせる DoS になるためです。

### ログアウト

`POST /api/auth/logout` は有効なアクセストークンを要求しません (期限切れのタブからでもログアウトできる必要があるため)。Redis の削除に失敗した場合は **503 を返し、認証 Cookie を消しません**。成功したように見せるとリフレッシュトークンが最大 30 日生き残るためです。SPA は成功するまで再試行します。

### Pulsoid OAuth (心拍データ連携)

- アクセストークン・リフレッシュトークンは AES-256-GCM で暗号化し `pulsoid_connections` テーブルに保存
- 外部サイトへ遷移する前に、SPA がアクセストークンの残り時間を確認し、必要ならリフレッシュしてから遷移します

### 障害時の挙動

| 状況 | 挙動 |
|---|---|
| Redis 停止中 | 既存のアクセス JWT は最大 30 分そのまま有効。リフレッシュとログインは 503 で失敗する。SPA はログイン画面へ飛ばさず再試行する |
| Redis 再起動 (AOF 有効) | `appendfsync everysec` のため最悪 1 秒分の書き込みが失われる。大半のセッションは存続するが、**直近 1 秒以内に作成・回転されたセッションは失われる可能性があり、該当ユーザーは再ログインになる** |
| Redis データ全損 | 全ユーザーが再ログイン。アクセス JWT は最大 30 分残存する |

アクセス JWT は即時失効できません。ログアウトや再利用検知を行っても、既存の JWT は最大 30 分間有効なままです。

## セットアップ

### 1. 鍵の生成

```bash
cd backend
cargo run -p api-backend -- gen-keys
```

出力された `JWT_ACTIVE_KID` / `JWT_PRIVATE_KEY` / `JWT_PUBLIC_KEYS` / `REFRESH_TOKEN_HMAC_KEY` / `REFRESH_SEAL_KEY` を `.env` に貼り付けます。

> **`REFRESH_TOKEN_HMAC_KEY` は稼働後に変更しないでください。** 全リフレッシュセッションがこの鍵で導出されているため、変更すると全ユーザーが強制ログアウトになります。

### 2. 環境変数の設定

```bash
cp .env.example .env
```

| 変数名 | 説明 | 生成方法 |
|--------|------|----------|
| `PUBLIC_ORIGIN` | 公開オリジン。Origin 検証・Cookie 属性・`return_to` 検証に使用 | 例: `http://localhost:3000` |
| `AUTH_INSECURE_DEV_COOKIES` | 開発時のみ `1`。ループバックオリジン以外では起動拒否 | 本番では設定しない |
| `JWT_ISSUER` / `JWT_AUDIENCE` | JWT の `iss` / `aud` | 任意の固定値 |
| `JWT_ACTIVE_KID` / `JWT_PRIVATE_KEY` | 署名鍵 (api-backend のみ) | `gen-keys` |
| `JWT_PUBLIC_KEYS` | JWK Set。api-backend と ws-gateway の両方に配布 | `gen-keys` |
| `REFRESH_TOKEN_HMAC_KEY` | リフレッシュトークンのハッシュ鍵 (**変更禁止**) | `gen-keys` |
| `REFRESH_SEAL_KEY` | ローテーション復旧用の暗号鍵 | `gen-keys` |
| `DISCORD_CLIENT_ID` / `DISCORD_CLIENT_SECRET` | Discord OAuth 資格情報 | [Discord Developer Portal](https://discord.com/developers/applications) |
| `DISCORD_REDIRECT_URI` | Discord のリダイレクト URI | 例: `http://localhost:3000/api/auth/callback/discord` |
| `PULSOID_CLIENT_ID` / `PULSOID_CLIENT_SECRET` | Pulsoid OAuth 資格情報 | [Pulsoid Developer](https://pulsoid.net/ui/keys) |
| `PULSOID_REDIRECT_URI` | Pulsoid のコールバック URI | 例: `https://yourdomain.com/api/oauth/pulsoid/callback` |
| `TOKEN_ENCRYPTION_KEY` | Pulsoid トークン暗号化キー (AES-256) | `openssl rand -base64 32` |
| `CLOUDFLARE_TUNNEL_TOKEN` | Cloudflare Tunnel トークン (本番のみ) | [Cloudflare Zero Trust](https://one.dash.cloudflare.com/) |

Discord Developer Portal の **OAuth2 → Redirects** に `DISCORD_REDIRECT_URI` と完全に同じ値を登録してください。

> **Note:** `DATABASE_URL`, `REDIS_URL`, `NATS_URL`, `RUST_LOG` は docker-compose.yml 内で自動設定されるため `.env` への記載は不要です。

### 3. Docker で起動 (推奨)

```bash
docker compose up --build
```

http://localhost:3000 でアクセスできます。

### 4. 本番環境 (Cloudflare Tunnel)

```bash
docker compose -f docker-compose.yml --profile prod up --build
```

> `-f docker-compose.yml` を明示指定することで `docker-compose.override.yml` の読み込みをスキップし、Nginx ポートをホストに公開しません。開発用の `AUTH_INSECURE_DEV_COOKIES` も適用されないため、`__Host-` 付きの Secure Cookie が使われます。

### 5. ローカル開発

```bash
# api-backend
cd backend
DATABASE_URL=postgres://hrmonitor:hrmonitor@localhost:5432/hrmonitor \
REDIS_URL=redis://localhost:6379 \
NATS_URL=nats://localhost:4222 \
PUBLIC_ORIGIN=http://localhost:3000 \
AUTH_INSECURE_DEV_COOKIES=1 \
cargo run -p api-backend

# frontend (Vite dev server; /api は上記サービスへプロキシされます)
cd frontend
npm install
npm run dev
```

### テスト

```bash
cd backend
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
REDIS_URL=redis://127.0.0.1:6379 cargo test   # REDIS_URL 未設定だとローテーションテストはスキップされます

cd frontend
npm run lint && npm run test && npm run build
```

## JWT 署名鍵のローテーション

```bash
cd backend
cargo run -p api-backend -- gen-jwt-key --kid k2
```

1. 新しい JWK を既存の `JWT_PUBLIC_KEYS` の `keys` 配列に**追加**し (旧鍵は残す)、**api-backend と ws-gateway の両方**をデプロイ。
2. `JWT_PRIVATE_KEY` と `JWT_ACTIVE_KID` を**同時に**新しい値へ切り替えて api-backend をデプロイ。`JWT_ACTIVE_KID` だけを変更すると、起動時の「秘密鍵から導出した公開鍵 == 公開している JWK」検証に失敗して起動しません。
3. 旧公開鍵の削除は、旧鍵で最後に JWT を発行してから **31 分以上** (アクセストークン 30 分 + clock skew 30 秒) 経過した後。実運用では 1 時間後を推奨します。

## Docker サービス一覧

| サービス | イメージ | 説明 |
|----------|---------|------|
| `timescaledb` | `timescale/timescaledb:2.29.2-pg18` | TimescaleDB (PostgreSQL 18) |
| `redis` | `redis:8.8.0-alpine` | 最新心拍キャッシュ + リフレッシュセッション (AOF 有効) |
| `nats` | `nats:2.14.4-alpine` | サービス間メッセージング |
| `migration` | ビルド: `./backend` | DB マイグレーション (起動時に一度だけ実行) |
| `backend` | ビルド: `./backend` | HTTP API + 認証 |
| `ws-gateway` | ビルド: `./backend` | WebSocket 配信 |
| `pulsoid-ingest` | ビルド: `./backend` | Pulsoid WS 取り込み |
| `pulsoid-refresher` | ビルド: `./backend` | Pulsoid トークン更新 |
| `nginx` | ビルド: `.` (`nginx/Dockerfile`) | SPA をビルドして配信 + リバースプロキシ |
| `cloudflared` | `cloudflare/cloudflared:latest` | Cloudflare Tunnel (prod プロファイル) |

SPA は nginx イメージのビルド段階で生成されるため、**本番環境に Node.js プロセスは存在しません**。

## API エンドポイント

### 認証

| メソッド | パス | 説明 |
|---------|------|------|
| `GET` | `/api/auth/login/discord` | Discord 認可画面へリダイレクト |
| `GET` | `/api/auth/callback/discord` | OAuth コールバック |
| `POST` | `/api/auth/refresh` | トークンのローテーションと再発行 |
| `POST` | `/api/auth/logout` | セッション失効 |
| `GET` | `/api/auth/session` | アクセストークンの有効性確認 (ローカル検証のみ) |

### REST API

| メソッド | パス | 説明 |
|---------|------|------|
| `GET` / `PATCH` | `/api/users/me` | 自分のプロフィール |
| `GET` | `/api/users/{id}/heart-rate-profile` | 表示用プロフィール |
| `GET` / `PUT` / `DELETE` | `/api/users/me/pulsoid-token` | Pulsoid トークン管理 |
| `GET` | `/api/users/{id}/heart-rates?period=` | 心拍データ (期間指定) |
| `GET` | `/api/users/{id}/heart-rates/by-date?date=` | 心拍データ (日付指定) |
| `GET` | `/api/users/{id}/heart-rates/daily-stats?date=` | 日別統計 |
| `GET` | `/api/users/{id}/heart-rates/minute-stats?period=` | 分単位集計 |
| `GET` / `POST` | `/api/groups` | グループ一覧・作成 |
| `GET` / `PATCH` / `DELETE` | `/api/groups/{id}` | グループ操作 |
| `PATCH` / `DELETE` | `/api/groups/{id}/members/me` | 共有設定・退出 |
| `GET` / `POST` | `/api/groups/{id}/invites` | 招待の一覧・作成 |
| `GET` / `POST` | `/api/invites/{token}` | 招待情報・受諾 |

### Pulsoid OAuth

| メソッド | パス | 説明 |
|---------|------|------|
| `POST` | `/api/oauth/pulsoid/connect` | 接続リクエスト作成 |
| `GET` | `/api/oauth/pulsoid/connect/{request_id}` | Pulsoid 認可画面へリダイレクト |
| `GET` | `/api/oauth/pulsoid/callback` | OAuth コールバック |

### WebSocket

| パス | 説明 |
|------|------|
| `/api/ws/me` | 自分の心拍 |
| `/api/ws/users/{id}` | 特定ユーザーの心拍 |
| `/api/ws/groups/{id}` | グループの心拍 |

アクセストークンの期限に達すると、gateway は close code **4401** で切断します。SPA はリフレッシュしてから自動的に再接続します。

## ドキュメント

- [API 仕様](docs/api.md)
- [アーキテクチャ](docs/architecture.md)
- [DB スキーマ](docs/schema.sql)

## ライセンス

[MIT](LICENSE.md)
