# HRMonitor

Pulsoid WebSocket から心拍数データをリアルタイム収集し、SQLite に保存、Next.js フロントエンドでグラフ表示する心拍モニタリングシステム。

## プロジェクト構成

```
apps/backend/    Rust (axum + sqlx + SQLite) — ポート 3001
apps/frontend/   Next.js (App Router + TanStack Query + Recharts) — ポート 3000
docs/            仕様書 (API, アーキテクチャ, スキーマ)
```

## 技術スタック

### Backend

- Rust (edition 2024), axum 0.8, tokio, sqlx 0.8 (SQLite)
- Pulsoid WebSocket 接続: tokio-tungstenite 0.29
- DB: SQLite (`/data/app.db`)、起動時にスキーマ自動作成

### Frontend

- Next.js 16, React 19, TypeScript 6
- TanStack React Query 5 (ポーリングによるデータ取得)
- Recharts 3 (心拍グラフ)
- Tailwind CSS 4

### インフラ

- Docker + Docker Compose
- cloudflared (Cloudflare Tunnel) による公開
- Cloudflare Access による認証 (アプリ側に認証機能なし)

## セットアップ

### Docker (推奨)

```bash
docker compose up --build
```

http://localhost:3000 でフロントエンドにアクセスできます。

### 本番環境 (Cloudflare Tunnel)

```bash
CLOUDFLARE_TUNNEL_TOKEN=<your-token> docker compose -f docker-compose.yml --profile prod up --build
```

> **Note:** `-f docker-compose.yml` を明示指定することで `docker-compose.override.yml` の読み込みをスキップし、Frontend ポートをホストに公開しません (Docker 内部ネットワークのみ)。

### ローカル開発

```bash
# Backend
cd apps/backend
DATABASE_URL=sqlite:///data/app.db cargo run

# Frontend
cd apps/frontend
BACKEND_URL=http://localhost:3001 npm run dev
```

## API エンドポイント

| メソッド | パス | 説明 |
|---------|------|------|
| `GET` | `/api/users` | ユーザー一覧 (最新BPM・トークン数付き) |
| `POST` | `/api/users` | ユーザー作成 |
| `PATCH` | `/api/users/{id}` | ユーザー名更新 |
| `GET` | `/api/users/{id}/pulsoid-tokens` | トークン一覧 |
| `POST` | `/api/users/{id}/pulsoid-tokens` | トークン追加 |
| `PATCH` | `/api/pulsoid-tokens/{id}` | トークン更新 (ラベル・有効状態) |
| `DELETE` | `/api/pulsoid-tokens/{id}` | トークン削除 |
| `GET` | `/api/users/{id}/heart-rates?from=&to=&limit=` | 心拍データ取得 |
| `GET` | `/api/users/{id}/daily-stats` | 日別統計 |
| `GET` | `/api/users/{id}/latest-heart-rate` | 最新心拍数 |

## アーキテクチャ

```
ブラウザ
  ↓ HTTP (ポーリング)
[Next.js Frontend :3000]
  ↓ /api/* リバースプロキシ
[Rust Backend :3001] ←── WebSocket ──→ Pulsoid API
  ↓
[SQLite /data/app.db]
```

- Backend は Docker 内部ネットワーク限定 (`expose` のみ、`ports` なし)
- Frontend の `next.config.ts` rewrites で `/api/*` を Backend にプロキシ
- Pulsoid WebSocket ワーカーはトークンごとに1タスク生成、指数バックオフでリトライ
- ブラウザ WebSocket は使わずポーリングのみ

## ドキュメント

- [API 仕様](docs/api.md)
- [アーキテクチャ](docs/architecture.md)
- [DB スキーマ](docs/schema.sql)
