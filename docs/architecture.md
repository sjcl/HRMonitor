# アーキテクチャ

## 概要

Pulsoid から心拍数を取り込み、TimescaleDB に保存し、WebSocket でブラウザへプッシュ配信する。認証は Rust バックエンドが所有し、フロントエンドは静的な SPA として配信される。

```
                      ┌─────────────────┐
   ブラウザ ──────────▶│  cloudflared    │  (prod プロファイルのみ)
                      └────────┬────────┘
                               ▼
                      ┌─────────────────┐
                      │  nginx :80      │  唯一の公開エントリポイント
                      └────────┬────────┘
        ┌──────────────────────┼──────────────────────┐
        ▼                      ▼                      ▼
  /assets/*, /*         /api/auth/*, /api/*      /api/ws/*
  静的 SPA (dist)       api-backend :3001        ws-gateway :3002
                               │                      │
                               ├── TimescaleDB ───────┤
                               ├── Redis ─────────────┤
                               └── NATS ──────────────┘

  pulsoid-ingest ──── Pulsoid WebSocket
  pulsoid-refresher ─ Pulsoid OAuth token refresh
```

## サービス

| サービス | 役割 |
|---|---|
| `nginx` | SPA のビルドと配信、リバースプロキシ、セキュリティヘッダ |
| `api-backend` | REST API、Discord OAuth、JWT 発行、リフレッシュセッション管理 |
| `ws-gateway` | WebSocket 配信専用。HTTP API の再起動で接続が切れないよう分離 |
| `pulsoid-ingest` | ユーザーごとに Pulsoid WS ワーカーを張り、心拍を DB / Redis / NATS へ |
| `pulsoid-refresher` | Pulsoid OAuth トークンを期限前にリフレッシュ |
| `migration` | 起動時に一度だけ DB マイグレーションを実行 |
| `timescaledb` | 心拍時系列 (hypertable) とユーザー / グループ |
| `redis` | 最新心拍キャッシュ + リフレッシュセッション (AOF 有効) |
| `nats` | サービス間メッセージング (Core NATS、JetStream 不使用) |

`nginx` 以外はすべて Docker 内部ネットワークのみ (`expose` のみで `ports` なし)。本番に Node.js プロセスは存在しない — SPA は nginx イメージのビルド段階で生成される。

## 認証アーキテクチャ

### 設計の要点

認証 (このリクエストは誰か) と認可 (この人はこれを見てよいか) を分離している。

- **認証**は Ed25519 JWT のローカル署名検証のみ。データベースにも Redis にも触れない。
- **認可**は毎回データベースを参照する。グループ所属や心拍公開範囲は変わりうるので、最長 30 分有効なトークンの内容を信用してはならない。

この分離のため、JWT には `iss` / `aud` / `sub` / `sid` / `iat` / `exp` / `jti` しか入らない。メール、Discord トークン、権限、心拍データは**含めない**。

### トークン

| | アクセストークン | リフレッシュトークン |
|---|---|---|
| 形式 | Ed25519 (EdDSA) JWS | CSPRNG 32 バイト |
| Cookie | `__Host-hrmonitor_session` | `__Host-hrmonitor_refresh` |
| 値 | JWT | `{sid}.{secret}` |
| 有効期限 | 30 分 | 30 日 (ローテーションで延長されない) |
| サーバー側の保存 | なし | Redis に HMAC-SHA256 のみ |

Cookie は本番で `Secure; HttpOnly; SameSite=Lax; Path=/`、`Domain` なし。開発時は `__Host-` が平文 HTTP で使えないため別名 (`hrmonitor_session_dev` など) に切り替わるが、これは明示的な `AUTH_INSECURE_DEV_COOKIES=1` と、`PUBLIC_ORIGIN` がループバックであることの両方を満たす場合に限られる。それ以外では起動を拒否する。ビルドプロファイル (debug/release) では判定しない — Dockerfile は開発用 compose でも `--release` でビルドするためである。

### 鍵配布

`JWT_PUBLIC_KEYS` は標準の JWK Set (`kty=OKP`, `crv=Ed25519`, `alg=EdDSA`, `use=sig`)。起動時に検証し、`kid` の重複・空文字・秘密要素 `d` の混入をすべて拒否する。

- 秘密鍵 (`JWT_PRIVATE_KEY`) は api-backend のみが持つ。
- ws-gateway は `common` の `jwt` feature だけを有効にするため、**署名側のコードがバイナリに存在しない**。
- api-backend は起動時に、秘密鍵から導出した公開鍵が `JWT_ACTIVE_KID` の JWK と一致することを確認する。

`kid` はメモリ上の `HashMap` のキーとしてのみ使い、ファイルパス・URL・DB クエリには使わない。

検証時には RFC 8725 に沿って、`alg` を `EdDSA` に固定 (`kid` を見る前に確認)、ヘッダの未知パラメータ (`crit` / `b64` / `jku` / `jwk` など) を一括拒否、トークンとセグメントの長さ上限、`iat` が未来すぎないこと、`iat <= exp`、最大有効期間の超過を確認する。`exp` の判定は RFC 7519 §4.1.4 に従い `now >= exp + leeway` (境界を含めて失効)。

### Discord OAuth フロー

```
GET /api/auth/login/discord
  ├─ PKCE verifier / challenge (S256) を生成
  ├─ state と nonce を CSPRNG で生成
  ├─ Redis: auth:oauth:discord:v1:{state} = {verifier, HMAC(nonce), return_to, tz}  TTL 5分
  ├─ Set-Cookie: __Host-hrmonitor_oauth = nonce  (Max-Age 300)
  └─ 302 → Discord

GET /api/auth/callback/discord?code&state
  ├─ nonce Cookie を読む (無ければ失敗)
  ├─ Redis: GETDEL で state を 1 回限り消費
  ├─ nonce の HMAC を定数時間で照合 ← ここが login CSRF 対策 (RFC 9700 §2.1)
  ├─ code + verifier を交換 → identify で Discord プロフィール取得
  ├─ users / accounts を upsert (advisory lock で同時初回ログインを直列化)
  ├─ Redis にリフレッシュセッションを作成
  └─ 302 → return_to  (アクセス / リフレッシュ Cookie を発行、oauth Cookie を削除)
```

Discord のアクセストークンは `identify` 呼び出し後に破棄し、保存しない。スコープは `identify` のみで、メールアドレスは要求しない。

`return_to` は `PUBLIC_ORIGIN` をベースに URL としてパースし、origin の完全一致を確認する。バックスラッシュや制御文字を含む入力は事前に拒否する (ブラウザ間のパース差異による bypass 対策)。

初回ログイン時のみ、クエリ `?tz=` で渡された IANA タイムゾーンを検証して `users.timezone` に設定する (不正・欠落なら `UTC`)。既存ユーザーのタイムゾーンはログインで上書きしない。

### リフレッシュのローテーション

`POST /api/auth/refresh` は Lua スクリプト 1 往復でアトミックに処理する。

```
current_hash と一致            → ローテーション           (200, 新 Cookie)
previous_hash と一致 & grace内 → 正当な並行アクセス       (200, 勝者と同じ Cookie)
previous_hash と一致 & grace外 → 再利用検知、セッション削除 (401)
どれとも一致しない             → セッションは残す          (401)
```

grace 窓 (10 秒) が必要な理由は 2 つある。複数タブの同時リフレッシュと、**ローテーションは成功したがレスポンスが届かなかった**場合である。後者に対応するため、新しい secret の AES-256-GCM 暗号文を grace と同じ寿命の Redis キーに置き、grace 経路で復号して勝者と同一のトークンを返す。AAD は `(version, sid, new_current_hash)` — いずれも Lua 実行前に確定するため、1 往復・原子的という性質を壊さずに済む (新しい `rotation` 値は Lua 内でしか決まらないので AAD には使えない)。

「未知の secret ではセッションを消さない」のは意図的である。`sid` は JWT にもリフレッシュ Cookie にも入っており秘密ではないため、任意の不正値で失効できると他人を強制ログアウトさせられてしまう。代償として、2 世代以上前のトークンの遅い再生はファミリー失効まで至らず 401 のみになる。

リフレッシュ Cookie の `Max-Age` はセッションの残り時間 (`exp - now`) に合わせ、ローテーションで 30 日にリセットしない。

### ログアウト

`POST /api/auth/logout` は有効なアクセストークンを要求しない。期限切れのタブからでもログアウトできる必要があるためである。ただし `sid` を名乗るだけでは失効させない。失効の根拠になるのは次のいずれかで、どちらも「そのセッションを保持している証明」である:

1. **有効なアクセス JWT** — 署名検証済みなので、その `sid` クレームをそのまま権限として扱う。
2. **リフレッシュ Cookie** — secret の HMAC が Redis 上の `current_hash` / `previous_hash` と一致した場合のみ。判定と削除は Lua (`REVOKE_SCRIPT`) で原子的に行うので、同時実行中のローテーションに割り込まれない。

これは上の「未知の secret ではセッションを消さない」と同じ理由による。`sid` の提示だけで消せると、`sid` を知った者が誰でも対象ユーザーを強制ログアウトさせられる。どちらの根拠も無い場合は Redis に触れず、それでも 204 を返して Cookie を消す (セッションの存在を探る手掛かりを与えないため)。

なお失効側では `previous_hash` を grace 窓の期限に関係なく受け付ける。失効は fail-safe 方向の操作であり、ローテーション直後にログアウトしたタブを取り残さないためである (grace 経過後の旧世代提示はリフレッシュ側でも再利用検知として失効するので、結果は一致する)。

Redis の削除に失敗した場合は **503 を返し、認証 Cookie を残す**。成功したように見せるとリフレッシュトークンが最大 30 日生き残るためで、SPA は成功するまで再試行する。

### 障害時の挙動

Redis が落ちている間も、既存のアクセス JWT は最大 30 分そのまま有効である (ローカル検証のため)。リフレッシュとログインだけが 503 で失敗し、SPA はログイン画面へ飛ばさずに再試行する。

AOF (`appendfsync everysec`) を有効にしているため Redis の再起動でセッションは概ね残るが、直近 1 秒以内に作成・回転されたものは失われうる。

Redis 接続は `redis::aio::ConnectionManager` (`common::redis_conn`) で、接続断を検出すると背後で再接続する。**Redis を再起動してもサービスの再起動は不要**である。以前使っていた `MultiplexedConnection` には自動再接続が無く、Redis 再起動後はプロセスを再起動するまで Redis 処理が失敗し続けていた (`/healthz` は静的な `"ok"` なので、その間もコンテナは正常扱いだった)。

失敗したコマンドは再実行せず呼び出し元に返す。ローテーションの Lua スクリプトを盲目的に再適用しないためであり、再試行は本来それを持つべき層 — 503 を再試行する SPA、10 秒ごとの WS self-heal、次の Pulsoid フレーム — が担う。再接続の再試行回数を既定の 6 回から 1 回に絞っているのも同じ理由で、この再試行予算は待っているコマンドのレイテンシにそのまま乗るため、Redis 断中の `/api/auth/refresh` は粘らず数秒で 503 を返す。

`/healthz` が Redis を見ないのは意図的である。manager が自力で復帰する以上、復帰中の窓でコンテナを unhealthy にするのは何もしないより悪い。接続の確立自体も遅延評価なので、Redis が落ちている状態でも各サービスは起動する。

アクセス JWT は即時失効できない。ログアウトや再利用検知の後も、発行済みの JWT は最大 30 分間有効である。

## CSRF / Origin

すべてのブラウザ通信は nginx を通じて単一オリジンになるため、CORS レイヤーは存在しない。

- api-backend: `require_origin_unsafe` — safe メソッド (GET/HEAD/OPTIONS) 以外は `Origin` が `PUBLIC_ORIGIN` と完全一致しなければ 403。欠落も 403。
- ws-gateway: `require_origin_always` — WebSocket のアップグレードは GET だが状態を持つチャネルを開くため、メソッドに関わらず検証する。

OAuth コールバックはトップレベルの GET ナビゲーションで `Origin` を持たないが、safe メソッドなので特別扱いなしに通る。その CSRF 対策は state の 1 回限り消費と nonce 束縛と PKCE が担う。

axum 0.8 では**最後に付けた `.layer()` が最も外側**になる。Origin 検証は認証より後に `.layer()` するため、認証や DB アクセスの前に走る。

## リアルタイムデータフロー

```
Pulsoid WS → pulsoid-ingest → TimescaleDB 保存
                            → Redis 更新 (latest_bpm:v2:{user_id}, TTL 6h)
                            → NATS publish (hr.received)
                                   ↓
                              ws-gateway → tokio broadcast → 各 WS クライアント
```

pulsoid-ingest の Pulsoid WS ワーカーは接続中、30 秒ごとに WebSocket Ping を送る。Text / Binary / Ping / Pong を 90 秒間ひとつも受信しなければ transport がサイレントに停止したとみなし、接続を捨てて既存の再接続経路 (状態を `pending` に戻してバックオフ後に再接続) へ合流する。これは terminal な `error` ではないので再認可は不要。Pong が返らないこと単独では切断しない — 心拍 Text が流れていれば接続は維持され、逆にセンサーが offline でも Pong が届く限り維持される。

ws-gateway は起動時に直近 6 時間の心拍から Redis を `SET NX EX` で warm-up する (`NX` なので pulsoid-ingest の書き込みを上書きしない)。

WebSocket はアップグレード時に一度だけ認証されるため、トークンの `exp` に達したら close code **4401** で切断する。SPA はリフレッシュしてから再接続する。一方、他人のデータやグループを見る接続は 30 秒ごとに DB へ再認可を問い合わせる。

ブラウザは失敗した WebSocket アップグレードのステータスや本文を読めないため、SPA は接続前に `GET /api/auth/session` でトークンの鮮度を確認する。

## NATS サブジェクト

| サブジェクト | 送信元 → 受信先 | 内容 |
|---|---|---|
| `hr.received` | pulsoid-ingest → ws-gateway | 心拍データ (WS ブロードキャスト用) |
| `pulsoid.connection.changed` | api-backend / pulsoid-refresher → pulsoid-ingest | トークン変更通知 |

## Redis キー

| キー | 内容 | TTL |
|---|---|---|
| `latest_bpm:v2:{user_id}` | 最新心拍 (JSON) | 6 時間 |
| `auth:session:v1:{sid}` | リフレッシュセッション (JSON) | 30 日 |
| `auth:rotation:v1:{sid}` | 直近発行 secret の AEAD 暗号文 | 10 秒 |
| `auth:oauth:discord:v1:{state}` | OAuth チケット (JSON) | 5 分 |

## キャッシュとセキュリティヘッダ (nginx)

- `/assets/*` — Vite のハッシュ付き成果物なので `immutable`、365 日。
- `/` (index.html) — `no-cache`。ハッシュ付きアセットを指すため常に再検証が必要。
- `/api/auth/*` — `no-store`。
- セキュリティヘッダ (CSP, `X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`) は `nginx/security-headers.conf` に置き、**`add_header` を持つすべての location で `include` する**。nginx は同一階層に `add_header` が 1 つでもあると上位階層のものを継承しないため、`server` レベルに書くだけでは HTML と JS から消えてしまう。
