# burrow-rs TODO

## 🔴 High Priority

- [ ] **Per-IP rate limiting**
  Replace the current global `governor` quota with a per-IP `DashMap<IpAddr, Arc<DefaultDirectRateLimiter>>`.
  Requires reading `ConnectInfo` from axum and extracting the real remote addr (accounting for `X-Forwarded-For` behind Render's proxy).
  _Files:_ `server/src/main.rs`

- [x] **WebSocket `Upgrade` header passthrough documentation**
  Added nginx config snippet to README with correct `proxy_set_header Upgrade` / `proxy_set_header Connection "upgrade"` directives.
  Audit: `upgrade` is in the hop-by-hop drop list (correct for HTTP tunnel). WS-over-WS (separate feature) will need to lift it.
  _Files:_ `README.md`

---

## 🔵 Header Handling

- [x] **Inject `X-Forwarded-*` headers on server**
  External caller's real IP is lost — local service always sees `127.0.0.1`.
  Server must inject before forwarding into the tunnel:
  - `X-Forwarded-For: <caller IP>` (append if already present)
  - `X-Forwarded-Host: <original Host>`
  - `X-Forwarded-Proto: https`
  - `X-Real-IP: <caller IP>`

  Requires `ConnectInfo<SocketAddr>` extractor in `proxy_handler` + chain handling behind Render's proxy.
  _Files:_ `server/src/main.rs`

- [x] **`Host` header rewrite (server → client)**
  Server injects `X-Forwarded-Host` to preserve original value.
  Client rewrites `Host` to `localhost:{port}` before forwarding — prevents CSRF failures and broken URL generation in frameworks that use `Host`.
  _Files:_ `server/src/main.rs`, `client/src/main.rs`

- [x] **`Location` header rewrite on response**
  Local service 301/302 redirects with `Location: http://localhost:3000/foo` are returned verbatim — unusable by external callers.
  Client scans response headers before building `ResponseOutgoing`; rewrites `http://localhost:{port}` → public URL.
  _Files:_ `client/src/main.rs`

- [x] **Complete hop-by-hop header filter**
  Current client filter only drops 4 headers. Full HTTP/1.1 hop-by-hop set to drop:
  `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailers`, `transfer-encoding`, `upgrade`.
  Also parse the `Connection` header value itself — any token it lists must also be dropped.
  Apply on both request (client→local) and response (local→client) directions.
  _Files:_ `client/src/main.rs`

- [x] **`Set-Cookie` `Domain` rewrite**
  Local service sets `Set-Cookie: Domain=localhost` — browsers discard when response arrives from public hostname.
  Client rewrites `Domain=localhost` → `Domain=<public_host>` in all `Set-Cookie` response headers.
  Requires client to retain `public_host` from the `Registered` message.
  _Files:_ `client/src/main.rs`

---

---

## 🔥 High Value Features

- [ ] **`CONNECT` method / generic TCP tunneling**
  Currently only HTTP/HTTPS is supported. gRPC, databases, and other TCP protocols are completely blocked.
  Detect `CONNECT` method in `proxy_handler` → open a dedicated binary-frame channel over the existing WebSocket for raw TCP relay.
  Upgrades burrow-rs from "HTTP tunnel" to "general-purpose TCP tunnel".
  _Files:_ `common/src/lib.rs`, `server/src/main.rs`, `client/src/main.rs`

- [ ] **Multi-tenant token management**
  All clients share one `SERVER_SECRET` — anyone with the token can claim any subdomain.
  Add a SQLite/PostgreSQL-backed token table: each token has an allowed subdomain list and expiry time.
  Consider `sqlx` + SQLite for zero-dependency self-hosted deployments.
  _Files:_ `server/src/main.rs`, `server/Cargo.toml`

- [ ] **Admin dashboard UI (`/admin`)**
  No visibility into active tunnels or traffic. Add a lightweight HTML dashboard showing:
  - Active tunnel list (subdomain, connected duration, bytes in/out)
  - Per-tunnel recent request log (method, path, status, latency)

  Serve as a static embedded page (`include_str!`) behind a separate `ADMIN_SECRET` env var.
  _Files:_ `server/src/main.rs`

---

## 🟡 Protocol Improvements

- [ ] **WebSocket-over-tunnel (WS-over-WS)**
  If the local service is itself a WebSocket server, the tunnel breaks — server receives `Upgrade: websocket` and forwards it as a plain HTTP request, causing a protocol mismatch.
  Detect `Upgrade: websocket` in request headers → switch to bidirectional binary relay mode instead of the request/response round-trip.
  _Files:_ `common/src/lib.rs`, `server/src/main.rs`, `client/src/main.rs`

- [x] **`X-Burrow-Request-Id` passthrough header**
  Injected into forwarded requests so local service logs can be correlated with burrow server logs.
  _Files:_ `server/src/main.rs`

---

## 🟢 DX / Ops

- [x] **Single binary with subcommands (`burrow server` / `burrow client`)**
  Both packages expose library entry points (`burrow_server::run`, `burrow_client::run`).
  New `cli` crate provides the unified `burrow` binary with subcommands.
  Old `burrow-server` and `burrow-client` binaries still built for backward compat.
  _Files:_ `cli/src/main.rs`, `server/src/lib.rs`, `client/src/lib.rs`, `Cargo.toml`

- [ ] **`~/.burrow.toml` config file**
  Persistent config so users don't need env vars or CLI flags every time.
  Use `config` crate supporting file + env var + CLI flag layering (CLI wins).
  _Files:_ `client/src/main.rs`, `client/Cargo.toml`

- [ ] **Prometheus metrics endpoint (`/metrics`)**
  Expose tunnel count, request count, latency histogram (p50/p95/p99), error rate.
  Use `metrics` + `metrics-exporter-prometheus` crates. Compatible with Render metrics, Grafana Cloud, or self-hosted Prometheus.
  _Files:_ `server/src/main.rs`, `server/Cargo.toml`

## 🟡 Medium Priority

- [ ] **Streaming / chunked response support**
  Current design buffers the full response body in memory before forwarding (base64 round-trip).
  For large files or SSE streams this is a blocker.
  Requires changing `ControlMessage` to support chunked framing (e.g. `RequestChunk` / `ResponseChunk` variants) and updating both server and client pump loops.
  _Files:_ `common/src/lib.rs`, `server/src/main.rs`, `client/src/main.rs`

- [x] **Configurable body size limit**
  Body limit exposed as `MAX_BODY_BYTES` env var on server (default 4 MB).
  Documented in README and render.yaml.
  _Files:_ `server/src/main.rs`, `README.md`, `render.yaml`

- [ ] **Structured access logging**
  Current tracing only logs subdomain + request_id.
  Add a tracing span in `proxy_handler` recording: remote IP, method, path, status code, response latency, body bytes sent.
  Matches nginx `access_log` format for easier log aggregation.
  _Files:_ `server/src/main.rs`

---

## 🟢 Low Priority / Future

- [ ] **Body compression (`gzip` / `br`)**
  base64 encoding inflates body by ~33%. Add optional compression in the tunnel protocol.
  Add `encoding: Option<String>` field to `RequestIncoming` / `ResponseOutgoing`.
  Client compresses body before sending; server decompresses before responding to external caller.
  _Files:_ `common/src/lib.rs`, `server/src/main.rs`, `client/src/main.rs`

- [ ] **TLS termination support (self-hosted)**
  Render handles TLS termination automatically, but self-hosted deployments need it.
  Add optional `CERT_PATH` / `KEY_PATH` env vars and `rustls`-based listener.
  _Files:_ `server/src/main.rs`, `server/Cargo.toml`

- [ ] **Render free-tier keep-alive**
  Render Free Web Services spin down after 15 min of inactivity, dropping the WebSocket.
  Client already auto-reconnects, but cold-start latency is ~30s.
  Options: upgrade to Render Starter ($7/mo), or add a self-ping cron hitting `/health` every 10 min.
  _Files:_ `README.md`

- [ ] **Subdomain stale-lock cleanup**
  If a client crashes without sending Close frame, the subdomain stays locked until the recv loop times out.
  Add a TTL-based cleanup: if `pending` map grows beyond threshold with no activity for N seconds, force-deregister.
  _Files:_ `server/src/main.rs`

- [ ] **Multi-port / multi-service support**
  Currently one client process = one subdomain = one local port.
  Allow a single client to register multiple `(subdomain, port)` pairs in one session.
  _Files:_ `common/src/lib.rs`, `client/src/main.rs`, `server/src/main.rs`

---

## ✅ Resolved

- [x] Header rewrites: `X-Forwarded-*` injection, `Host` rewrite, `Location` rewrite, full hop-by-hop filter, `Set-Cookie Domain` rewrite


- [x] `/health` endpoint missing — Render health check returned 404 _(fixed: added `health_handler`)_
- [x] No server-side heartbeat — idle WebSocket cut by load balancer _(fixed: 30s Ping task)_
- [x] Token comparison timing side-channel _(fixed: `subtle::ConstantTimeEq`)_
- [x] `send_task_done()` dummy future — client never detected send task exit _(fixed: `mut` JoinHandle in `select!`)_
- [x] Global-only rate limiting _(fixed: `governor` quota; per-IP tracked above as follow-up)_
- [x] Project rename `tunnel-rs` → `burrow-rs`
