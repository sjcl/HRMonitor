# HRMonitor

Pulsoid WebSocket から心拍数データを収集し、SQLite に保存、Next.js フロントエンドでグラフ表示する心拍モニタリングシステム。

## プロジェクト構成

```
apps/backend/    Rust (axum + sqlx + SQLite) — ポート 3001
apps/frontend/   Next.js (App Router + TanStack Query + Recharts) — ポート 3000
docs/            仕様書 (API, アーキテクチャ, スキーマ, エージェントプロンプト)
```

## 技術スタック

### Backend
- Rust (edition 2024), axum 0.8, tokio, sqlx (SQLite), tokio-tungstenite
- DB: SQLite (`/data/app.db`), 起動時に `sqlx::migrate!()` でマイグレーション適用
- Pulsoid WS ワーカー: トークンごとに1つ spawned、指数バックオフでリトライ

### Frontend
- Next.js 15, React 19, TypeScript
- @tanstack/react-query 5 (`refetchInterval: 5000` でポーリング)
- recharts 2 (心拍グラフ)
- Tailwind CSS 4
- `next.config.ts` の rewrites で `/api/*` を backend にプロキシ → CORS 不要

## 開発コマンド

```bash
# Backend
cd apps/backend && cargo run
# DATABASE_URL 環境変数、デフォルト sqlite:///data/app.db

# Frontend
cd apps/frontend && npm run dev
# BACKEND_URL 環境変数で backend を指定 (デフォルト http://backend:3001)

# Docker
docker compose up --build
```

## API エンドポイント (7つ)

- `GET/POST /api/users`, `PATCH /api/users/{id}`
- `GET/POST /api/users/{id}/pulsoid-tokens`
- `PATCH/DELETE /api/pulsoid-tokens/{id}`
- `GET /api/users/{id}/heart-rates?from=&to=&limit=`
- `GET /api/users/{id}/latest-heart-rate`

## アーキテクチャ要点

- 認証なし (Cloudflare Access がフロントエンドを保護)
- Backend は Docker 内部ネットワーク限定 (expose のみ、ports なし)
- Frontend が唯一のパブリックエントリポイント
- cloudflared トンネルでフロントエンドを公開
- ブラウザ WebSocket は使わない (ポーリングのみ)

## 実装状況

- [x] Backend: DB初期化、モデル、エラーハンドリング
- [x] Backend: 全7 API エンドポイント (users, tokens, heart_rates)
- [x] Backend: Pulsoid WebSocket ワーカー + WorkerManager
- [x] Frontend: ユーザー一覧ページ (/users)
- [x] Frontend: ユーザー詳細ページ (/users/[id]) — グラフ、トークン管理
- [x] Docker: Dockerfile (backend/frontend), docker-compose.yml, cloudflared
- [ ] README.md 更新
- [ ] E2E テスト (実際の Pulsoid トークンで動作確認)
