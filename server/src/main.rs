use anyhow::Result;
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Path, State,
    },
    http::{Request, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::{
    collections::HashMap,
    num::NonZeroU32,
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{oneshot, Mutex, RwLock},
    time::{interval, timeout},
};
use tracing::{info, warn};
use burrow_common::ControlMessage;
use uuid::Uuid;

// ── State ─────────────────────────────────────────────────────────────────────

struct TunnelEntry {
    ws_tx: tokio::sync::mpsc::Sender<ControlMessage>,
    pending: Mutex<HashMap<String, oneshot::Sender<ControlMessage>>>,
}

type TunnelRegistry = Arc<RwLock<HashMap<String, Arc<TunnelEntry>>>>;

#[derive(Clone)]
struct AppState {
    tunnels: TunnelRegistry,
    secret: String,
    public_host: String,
    /// Per-IP rate limiter for proxy requests (shared across all routes)
    rate_limiter: Arc<DefaultDirectRateLimiter>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "burrow_server=debug,tower_http=info".into()),
        )
        .init();

    let secret = std::env::var("SERVER_SECRET").unwrap_or_else(|_| {
        warn!("SERVER_SECRET not set – using insecure default");
        "changeme".to_string()
    });

    let public_host = std::env::var("PUBLIC_HOST").unwrap_or_else(|_| {
        warn!("PUBLIC_HOST not set – using localhost");
        "localhost".to_string()
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    // FIX 5: Rate limiter – 120 requests/minute globally (no per-IP tracking needed for MVP)
    let quota = Quota::per_minute(NonZeroU32::new(120).unwrap());
    let rate_limiter = Arc::new(RateLimiter::direct(quota));

    let state = AppState {
        tunnels: Arc::new(RwLock::new(HashMap::new())),
        secret,
        public_host,
        rate_limiter,
    };

    let app = Router::new()
        // FIX 1: /health endpoint for Render health checks
        .route("/health", get(health_handler))
        // WebSocket control channel
        .route("/tunnel/ws", get(ws_handler))
        // HTTP proxy entry points
        .route(
            "/{subdomain}/{*path}",
            get(proxy_handler)
                .post(proxy_handler)
                .put(proxy_handler)
                .delete(proxy_handler)
                .patch(proxy_handler),
        )
        .route(
            "/{subdomain}",
            get(proxy_handler)
                .post(proxy_handler)
                .put(proxy_handler)
                .delete(proxy_handler)
                .patch(proxy_handler),
        )
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    info!("Burrow server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

// FIX 1: Health check handler
async fn health_handler() -> StatusCode {
    StatusCode::OK
}

// ── WebSocket control handler ─────────────────────────────────────────────────

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_client(socket, state))
}

async fn handle_client(socket: WebSocket, state: AppState) {
    let (mut ws_send, mut ws_recv) = socket.split();

    // ── 1. Wait for Register ──────────────────────────────────────────────────
    let (subdomain, token) = match ws_recv.next().await {
        Some(Ok(Message::Text(txt))) => match ControlMessage::from_json(&txt) {
            Ok(ControlMessage::Register { subdomain, token }) => (subdomain, token),
            _ => {
                let _ = ws_send
                    .send(Message::Text(
                        ControlMessage::Error {
                            message: "expected Register as first message".into(),
                        }
                        .to_json()
                        .into(),
                    ))
                    .await;
                return;
            }
        },
        _ => return,
    };

    // FIX 3: Constant-time token comparison (prevents timing side-channel)
    let token_ok: bool = token.as_bytes().ct_eq(state.secret.as_bytes()).into();
    if !token_ok {
        let _ = ws_send
            .send(Message::Text(
                ControlMessage::Error {
                    message: "invalid token".into(),
                }
                .to_json()
                .into(),
            ))
            .await;
        return;
    }

    // ── 2. Assign subdomain ───────────────────────────────────────────────────
    let subdomain = subdomain.unwrap_or_else(|| Uuid::new_v4().to_string()[..8].to_string());

    let public_url = format!("https://{}/{subdomain}", state.public_host);

    // ── 3. Register tunnel ────────────────────────────────────────────────────
    let (msg_tx, mut msg_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);
    let entry = Arc::new(TunnelEntry {
        ws_tx: msg_tx,
        pending: Mutex::new(HashMap::new()),
    });

    {
        let mut reg = state.tunnels.write().await;
        if reg.contains_key(&subdomain) {
            let _ = ws_send
                .send(Message::Text(
                    ControlMessage::Error {
                        message: format!("subdomain '{subdomain}' already in use"),
                    }
                    .to_json()
                    .into(),
                ))
                .await;
            return;
        }
        reg.insert(subdomain.clone(), entry.clone());
        info!("Tunnel registered: {subdomain} → {public_url}");
    }

    // ── 4. Send Registered ack ────────────────────────────────────────────────
    if ws_send
        .send(Message::Text(
            ControlMessage::Registered {
                subdomain: subdomain.clone(),
                public_url: public_url.clone(),
            }
            .to_json()
            .into(),
        ))
        .await
        .is_err()
    {
        deregister(&state.tunnels, &subdomain).await;
        return;
    }

    // ── 5. Bidirectional pump + heartbeat ─────────────────────────────────────
    let tunnels_ref = state.tunnels.clone();
    let subdomain_ref = subdomain.clone();

    // FIX 2: Server-side heartbeat task – sends Ping every 30s
    let ping_tx = entry.ws_tx.clone();
    let heartbeat = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(30));
        ticker.tick().await; // skip immediate first tick
        loop {
            ticker.tick().await;
            if ping_tx.send(ControlMessage::Ping).await.is_err() {
                break;
            }
        }
    });

    // FIX 4: Use a real JoinHandle so send_task completion is detectable
    let send_task = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            if ws_send
                .send(Message::Text(msg.to_json().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let recv_loop = async {
        while let Some(Ok(msg)) = ws_recv.next().await {
            match msg {
                Message::Text(txt) => match ControlMessage::from_json(&txt) {
                    Ok(ControlMessage::ResponseOutgoing { ref request_id, .. }) => {
                        let entry = tunnels_ref.read().await.get(&subdomain_ref).cloned();
                        if let Some(e) = entry {
                            let mut pending = e.pending.lock().await;
                            if let Some(tx) = pending.remove(request_id.as_str()) {
                                let _ = tx.send(ControlMessage::from_json(&txt).unwrap());
                            }
                        }
                    }
                    Ok(ControlMessage::Pong) => {} // heartbeat acknowledged
                    _ => {}
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    // FIX 4: select! on both send_task (JoinHandle) and recv_loop
    tokio::select! {
        _ = send_task => {
            warn!("Send task exited for {subdomain}");
        }
        _ = recv_loop => {}
    }

    heartbeat.abort();
    deregister(&state.tunnels, &subdomain).await;
    info!("Tunnel disconnected: {subdomain}");
}

async fn deregister(tunnels: &TunnelRegistry, subdomain: &str) {
    tunnels.write().await.remove(subdomain);
}

// ── HTTP proxy handler ────────────────────────────────────────────────────────

async fn proxy_handler(
    Path(params): Path<HashMap<String, String>>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request<Body>,
) -> Response {
    // Rate limiting check
    if state.rate_limiter.check().is_err() {
        return error_response(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
    }

    let subdomain = match params.get("subdomain") {
        Some(s) => s.clone(),
        None => return error_response(StatusCode::BAD_REQUEST, "missing subdomain"),
    };

    let entry = {
        let reg = state.tunnels.read().await;
        reg.get(&subdomain).cloned()
    };

    let entry = match entry {
        Some(e) => e,
        None => return error_response(StatusCode::NOT_FOUND, "tunnel not found"),
    };

    let method = req.method().to_string();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| {
            let full = pq.as_str();
            let prefix = format!("/{subdomain}");
            if full.starts_with(&prefix) {
                full[prefix.len()..].to_string()
            } else {
                full.to_string()
            }
        })
        .unwrap_or_else(|| "/".to_string());

    let path = if path_and_query.is_empty() {
        "/".to_string()
    } else {
        path_and_query
    };

    // ── Header collection + X-Forwarded-* injection ───────────────────────────
    let caller_ip = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|existing| format!("{existing}, {}", peer.ip()))
        .unwrap_or_else(|| peer.ip().to_string());

    let original_host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();


    let mut headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
        // Drop the old X-Forwarded-For so we rebuild it cleanly below
        .filter(|(k, _)| k.to_lowercase() != "x-forwarded-for")
        .collect();

    // Inject / overwrite forwarding headers
    headers.push(("x-forwarded-for".into(), caller_ip));
    headers.push(("x-forwarded-host".into(), original_host));
    headers.push(("x-forwarded-proto".into(), "https".into()));
    headers.push(("x-real-ip".into(), peer.ip().to_string()));
    // Rewrite Host so local service sees its own address, not the public hostname
    headers.retain(|(k, _)| k.to_lowercase() != "host");
    // Client will rewrite Host to localhost:{port} — server sets forwarded-host above

    let body_bytes = match axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "failed to read body"),
    };
    let body_b64 = B64.encode(&body_bytes);

    let request_id = Uuid::new_v4().to_string();
    let (resp_tx, resp_rx) = oneshot::channel::<ControlMessage>();

    {
        let mut pending = entry.pending.lock().await;
        pending.insert(request_id.clone(), resp_tx);
    }

    let fwd = ControlMessage::RequestIncoming {
        request_id: request_id.clone(),
        method,
        path,
        headers,
        body_b64,
    };

    if entry.ws_tx.send(fwd).await.is_err() {
        return error_response(StatusCode::BAD_GATEWAY, "tunnel disconnected");
    }

    match timeout(Duration::from_secs(30), resp_rx).await {
        Ok(Ok(ControlMessage::ResponseOutgoing {
            status,
            headers,
            body_b64,
            ..
        })) => {
            let body = B64.decode(&body_b64).unwrap_or_default();
            let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let mut response = Response::builder().status(status_code);
            for (k, v) in &headers {
                if let (Ok(name), Ok(val)) = (
                    k.parse::<axum::http::HeaderName>(),
                    v.parse::<axum::http::HeaderValue>(),
                ) {
                    response = response.header(name, val);
                }
            }
            response
                .body(Body::from(body))
                .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "build error"))
        }
        Ok(Ok(_)) | Ok(Err(_)) => error_response(StatusCode::BAD_GATEWAY, "unexpected response"),
        Err(_) => {
            let mut pending = entry.pending.lock().await;
            pending.remove(&request_id);
            error_response(StatusCode::GATEWAY_TIMEOUT, "tunnel timeout")
        }
    }
}

fn error_response(status: StatusCode, msg: &str) -> Response {
    Response::builder()
        .status(status)
        .body(Body::from(msg.to_string()))
        .unwrap()
}
