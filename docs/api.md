# API 仕様

すべてのエンドポイントは同一オリジン (`PUBLIC_ORIGIN`) から nginx 経由で提供される。認証は Cookie に載ったアクセス JWT で行い、`Authorization` ヘッダは使わない。

- `{id}` には `me` を指定できる (認証ユーザー自身に解決される)。
- 状態を変更するメソッド (GET/HEAD/OPTIONS 以外) は `Origin` ヘッダが `PUBLIC_ORIGIN` と完全一致しなければ **403**。ヘッダ欠落も 403。
- エラーは `{"error": "..."}` の JSON で返る。

## 認証

### `GET /api/auth/login/discord`

Discord の認可画面へリダイレクトする。

| クエリ | 説明 |
|---|---|
| `return_to` | 認証後の遷移先。同一オリジンの絶対パスのみ。不正な値は `/me` に落とす |
| `tz` | ブラウザの IANA タイムゾーン。**初回ログイン時のみ** `users.timezone` の初期値に使う |

`__Host-hrmonitor_oauth` Cookie (Max-Age 300) を発行する。**302**。

### `GET /api/auth/callback/discord`

Discord からのコールバック。`code` と `state` を受け取る。`state` は 1 回限り消費され、`__Host-hrmonitor_oauth` Cookie の nonce と照合される。

成功時はアクセス / リフレッシュ Cookie を発行して `return_to` へ **302**。失敗時は `/login?error=<code>` へ **302** (`denied` / `invalid_state` / `missing_state_cookie` / `exchange_failed` / `unavailable`)。成功・失敗いずれでも oauth Cookie を削除する。

### `POST /api/auth/refresh`

リフレッシュトークンをローテーションし、アクセストークンを再発行する。有効なアクセストークンは**不要**。

| ステータス | 意味 |
|---|---|
| **204** | 成功。新しい Cookie を発行 |
| **401** | セッションが無効・期限切れ・再利用検知。Cookie を削除 |
| **503** | セッションストアに到達できない。**Cookie は削除しない** — 再試行すること |
| **403** | Origin 不一致 |

### `POST /api/auth/logout`

セッションを失効させる。有効なアクセストークンは**不要** (期限切れのタブからもログアウトできる必要があるため)。

| ステータス | 意味 |
|---|---|
| **204** | 失効済み、または元から無効。Cookie を削除 |
| **503** | Redis 削除に失敗。**認証 Cookie は残る** — ログアウトは完了していない |
| **403** | Origin 不一致 |

### `GET /api/auth/session`

アクセストークンの有効性を確認する。**ローカル署名検証のみで DB / Redis を参照しない。** `Cache-Control: no-store`。

WebSocket 接続前の事前確認に使う (失敗したアップグレードはブラウザから詳細を取得できないため)。

- **200** `{"authenticated": true, "expires_at": <unix秒>}`
- **401** 未認証

## ユーザー

| メソッド | パス | 説明 |
|---|---|---|
| `GET` | `/api/users/me` | 自分のプロフィール (`id`, `display_name`, `avatar_url`, `timezone`, `heart_rate_visibility`) |
| `PATCH` | `/api/users/me` | `display_name` / `timezone` / `heart_rate_visibility` を更新 |
| `GET` | `/api/users/{id}/heart-rate-profile` | 表示用プロフィール。閲覧権限が無ければ **403** |

`heart_rate_visibility` は `group_default` または `private`。

## 心拍データ

| メソッド | パス | 説明 |
|---|---|---|
| `GET` | `/api/users/{id}/heart-rates?period=` | 生データ (期間指定) |
| `GET` | `/api/users/{id}/heart-rates/by-date?date=` | 生データ (日付指定) |
| `GET` | `/api/users/{id}/heart-rates/daily-stats?date=` | 日別統計 |
| `GET` | `/api/users/{id}/heart-rates/minute-stats?period=` | 分単位集計 |
| `GET` | `/api/users/{id}/heart-rates/minute-stats/by-date?date=` | 分単位集計 (日付指定) |
| `GET` | `/api/groups/{id}/heart-rates` | グループメンバーの生データ |
| `GET` | `/api/groups/{id}/heart-rates/minute-stats` | グループメンバーの分単位集計 |

閲覧可否はリクエストごとに DB を参照して判定する (JWT の内容では判定しない)。

## グループと招待

| メソッド | パス | 説明 |
|---|---|---|
| `GET` / `POST` | `/api/groups` | 一覧 / 作成 |
| `GET` / `PATCH` / `DELETE` | `/api/groups/{id}` | 取得 / 更新 / 削除 |
| `PATCH` / `DELETE` | `/api/groups/{id}/members/me` | 共有設定の変更 / 退出 |
| `GET` / `POST` | `/api/groups/{id}/invites` | 招待の一覧 / 作成 |
| `DELETE` | `/api/groups/{id}/invites/{invite_id}` | 招待の失効 |
| `GET` | `/api/invites/{token}` | 招待情報の取得 |
| `POST` | `/api/invites/{token}/accept` | 招待の受諾 |

招待トークンは SHA-256 でハッシュ化して保存される。

## Pulsoid 連携

| メソッド | パス | 説明 |
|---|---|---|
| `GET` / `PUT` / `DELETE` | `/api/users/me/pulsoid-token` | 接続状態の取得 / 手動トークン設定 / 解除 |
| `POST` | `/api/oauth/pulsoid/connect` | 接続リクエストを作成し `request_id` を返す |
| `GET` | `/api/oauth/pulsoid/connect/{request_id}` | Pulsoid の認可画面へリダイレクト |
| `GET` | `/api/oauth/pulsoid/callback` | OAuth コールバック |

`GET /api/users/me/pulsoid-token` は未接続なら **404**。

外部サイトへ遷移するため、SPA は接続開始前にアクセストークンの残り時間を確認し、必要ならリフレッシュしてから遷移する。

## WebSocket

| パス | 説明 |
|---|---|
| `/api/ws/me` | 自分の心拍 |
| `/api/ws/users/{id}` | 特定ユーザーの心拍 |
| `/api/ws/groups/{id}` | グループ全員の心拍 |

アップグレードには `Origin` ヘッダの完全一致が**必須** (メソッドに関わらず検証される)。

サーバーからのメッセージ:

```json
{"type": "snapshot", "data": {"<user_id>": {"user_id": "...", "bpm": 72, "recorded_at": 0, "received_at": 0} | null}}
{"type": "update",   "data": {"user_id": "...", "bpm": 72, "recorded_at": 0, "received_at": 0}}
```

close code:

| コード | 意味 |
|---|---|
| `1001` | サーバー停止中 |
| `4401` | アクセストークンの期限切れ。リフレッシュしてから再接続すること |

他人やグループを見る接続では、30 秒ごとに閲覧権限を DB で再確認し、失われていれば切断する。

## ヘルスチェック

| パス | 説明 |
|---|---|
| `/healthz` | api-backend の死活 (nginx がプロキシ) |
| `/nginx-health` | nginx 自身の死活 |
