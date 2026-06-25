# burrow-rs

自製 ngrok — 用 Rust 實作的 HTTP 反向隧道服務。

## 架構

```
外部使用者
    │ HTTPS
    ▼
┌─────────────────────────────────┐
│  burrow-server (Render)         │
│                                 │
│  /burrow/ws  ← WebSocket 控制通道│
│  /{subdomain}/{path} ← HTTP入口  │
└────────────┬────────────────────┘
             │ WebSocket (wss://)
             │ 持久連線
             ▼
┌─────────────────────────────────┐
│  burrow (client, PC本機)         │
│                                 │
│  接收 RequestIncoming            │
│  → 轉發到 localhost:{port}       │
│  → 回傳 ResponseOutgoing         │
└────────────┬────────────────────┘
             │ HTTP
             ▼
┌─────────────────────────────────┐
│  你的本機服務 (e.g. :3000)        │
└─────────────────────────────────┘
```

## 部署 Server 到 Render

### 方法 A：Docker（推薦，建構較快）

1. Push 這個 repo 到 GitHub
2. Render Dashboard → **New Web Service**
3. 選 Docker runtime，指向這個 repo
4. 設定環境變數：

| 變數 | 說明 |
|------|------|
| `SERVER_SECRET` | 客戶端認證 token，保密 |
| `PUBLIC_HOST` | Render 給的 hostname，如 `burrow-server.onrender.com` |
| `PORT` | `8080`（Render 預設會注入） |
| `RUST_LOG` | `tunnel_server=info` |

5. 部署完成後記下 `https://your-app.onrender.com`

### 方法 B：Native Rust build

render.yaml 已配置，直接 `git push`，Render 會自動偵測。

---

## 本機使用 Client

### 安裝

```bash
cargo install --path client
```

或直接執行：

```bash
cargo run --bin burrow -- --help
```

### 啟動隧道

```bash
# 基本用法：將本機 3000 port 暴露出去
TUNNEL_SERVER=wss://your-app.onrender.com/burrow/ws \
TUNNEL_TOKEN=your-secret \
burrow --port 3000

# 指定 subdomain（公開網址會是 https://your-app.onrender.com/myapp/...）
burrow \
  --port 3000 \
  --server wss://your-app.onrender.com/burrow/ws \
  --token your-secret \
  --subdomain myapp
```

輸出範例：
```
✅ Tunnel active!
   Public URL : https://your-app.onrender.com/myapp
   Subdomain  : myapp
   Forwarding : https://your-app.onrender.com/myapp → localhost:3000
```

### 環境變數方式

```bash
export TUNNEL_SERVER=wss://your-app.onrender.com/burrow/ws
export TUNNEL_TOKEN=your-secret
export LOCAL_PORT=3000
export TUNNEL_SUBDOMAIN=myapp
burrow
```

---

## 注意事項

### Render Free Tier 限制
Render 免費方案的 Web Service 在無流量時會 **spin down（休眠）**。這會導致 WebSocket 連線中斷。  
解法：
- 升級到 Paid plan（$7/月起）
- 或使用 cron job 每 14 分鐘 ping 一次保持喚醒

### WebSocket Keep-alive
Client 每 30 秒會收到 Ping，自動回 Pong。Render 平台預設 WebSocket timeout 為 60 秒，此設計在範圍內。

### Body 大小限制
目前 server 端限制單一 request body 為 **4 MB**。可在 `proxy_handler` 中調整 `to_bytes` 的第二個參數。

### HTTPS / TLS
Render 自動提供 TLS termination，無需在應用內處理。本機 client 使用 `wss://` 連線即可。

---

## 本機開發測試

```bash
# 終端 1：啟動 server
SERVER_SECRET=test cargo run --bin burrow-server

# 終端 2：啟動你的本機服務（示例用 Python）
python3 -m http.server 3000

# 終端 3：啟動 client
cargo run --bin burrow -- --port 3000 --token test --subdomain demo

# 終端 4：測試
curl http://localhost:8080/demo/
```

---

## Quick Start Scripts

| Script | Description |
|--------|-------------|
| `scripts/demo.sh` | Prints terminal-by-terminal setup instructions |
| `scripts/test-integration.sh` | Full automated integration test (11 tests) |

### Integration Tests

```bash
# Run all integration tests (builds + tests)
bash scripts/test-integration.sh

# Skip build if already built
bash scripts/test-integration.sh skipbuild
```

The integration test validates:
- Tunnel registration with custom subdomain
- HTTP GET forwarding (status + body)
- HTTP POST forwarding with request body
- `X-Forwarded-For`, `X-Forwarded-Proto`, `X-Forwarded-Host` header injection
- Host header rewrite (`localhost:{port}`)
- 404 for unknown subdomains
- Invalid token rejection
- Duplicate subdomain rejection

### Unit Tests

```bash
# Run all unit tests
cargo test

# Run specific crate tests
cargo test -p burrow-common
cargo test -p burrow-client
```

Unit tests cover:
- `ControlMessage` JSON serialization round-trips (all variants)
- Hop-by-hop header filtering (`is_hop_by_hop`, `connection_tokens`)
- `Set-Cookie` `Domain` attribute rewriting
- Error response formatting
