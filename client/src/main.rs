use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use std::time::Duration;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};
use burrow_common::ControlMessage;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "burrow", about = "Tunnel client – expose localhost to the internet")]
struct Args {
    /// Local port to expose
    #[arg(short, long, env = "LOCAL_PORT", default_value = "3000")]
    port: u16,

    /// Tunnel server WebSocket URL, e.g. wss://your-app.onrender.com/tunnel/ws
    #[arg(short, long, env = "TUNNEL_SERVER", default_value = "ws://localhost:8080/tunnel/ws")]
    server: String,

    /// Auth token (must match SERVER_SECRET on the server)
    #[arg(short, long, env = "TUNNEL_TOKEN", default_value = "changeme")]
    token: String,

    /// Preferred subdomain (server assigns one if not set)
    #[arg(long, env = "TUNNEL_SUBDOMAIN")]
    subdomain: Option<String>,

    /// Reconnect delay in seconds
    #[arg(long, default_value = "5")]
    reconnect_delay: u64,
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "burrow=info".into()),
        )
        .init();

    let args = Args::parse();

    loop {
        info!(
            "Connecting to tunnel server {} → localhost:{}",
            args.server, args.port
        );
        match run_session(&args).await {
            Ok(()) => {
                info!("Session ended cleanly. Reconnecting in {}s…", args.reconnect_delay);
            }
            Err(e) => {
                error!("Session error: {e:#}. Reconnecting in {}s…", args.reconnect_delay);
            }
        }
        sleep(Duration::from_secs(args.reconnect_delay)).await;
    }
}

// ── Single session ────────────────────────────────────────────────────────────

async fn run_session(args: &Args) -> Result<()> {
    let (ws_stream, _) = connect_async(&args.server)
        .await
        .with_context(|| format!("failed to connect to {}", args.server))?;

    let (mut ws_send, mut ws_recv) = ws_stream.split();

    // ── Register ──────────────────────────────────────────────────────────────
    let register = ControlMessage::Register {
        subdomain: args.subdomain.clone(),
        token: args.token.clone(),
    };
    ws_send
        .send(Message::Text(register.to_json().into()))
        .await
        .context("send Register")?;

    // ── Wait for Registered ack ───────────────────────────────────────────────
    let (public_url, _subdomain) = match ws_recv.next().await {
        Some(Ok(Message::Text(txt))) => match ControlMessage::from_json(&txt) {
            Ok(ControlMessage::Registered { public_url, subdomain }) => {
                info!("✅ Tunnel active!");
                info!("   Public URL : {public_url}");
                info!("   Subdomain  : {subdomain}");
                info!("   Forwarding : {public_url} → localhost:{}", args.port);
                (public_url, subdomain)
            }
            Ok(ControlMessage::Error { message }) => {
                anyhow::bail!("Server rejected registration: {message}");
            }
            _ => anyhow::bail!("unexpected server message"),
        },
        _ => anyhow::bail!("connection closed before Registered"),
    };
    // Extract public host for Set-Cookie Domain rewrite and Location rewrite
    let public_host = {
        // e.g. "https://foo.onrender.com/myapp" → "foo.onrender.com"
        let url = url::Url::parse(&public_url).ok();
        url.and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| public_url.clone())
    };

    let http = HttpClient::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let local_port = args.port;

    // ── Message loop ──────────────────────────────────────────────────────────
    // We need to send responses back while also receiving requests.
    // Use a channel to serialise writes to ws_send.
    let (resp_tx, mut resp_rx) =
        tokio::sync::mpsc::channel::<ControlMessage>(64);

    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = resp_rx.recv().await {
            if ws_send
                .send(Message::Text(msg.to_json().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // FIX 4: select! on the real send_task JoinHandle so we exit when it dies
    loop {
        tokio::select! {
            msg = ws_recv.next() => {
                match msg {
                    Some(Ok(Message::Text(txt))) => {
                        match ControlMessage::from_json(&txt) {
                            Ok(ControlMessage::RequestIncoming {
                                request_id,
                                method,
                                path,
                                headers,
                                body_b64,
                            }) => {
                                let http = http.clone();
                                let resp_tx = resp_tx.clone();
                                let public_url = public_url.clone();
                                let public_host = public_host.clone();
                                tokio::spawn(async move {
                                    let response = forward_request(
                                        &http,
                                        local_port,
                                        &request_id,
                                        &method,
                                        &path,
                                        headers,
                                        &body_b64,
                                        &public_url,
                                        &public_host,
                                    )
                                    .await;
                                    let _ = resp_tx.send(response).await;
                                });
                            }
                            Ok(ControlMessage::Ping) => {
                                let _ = resp_tx.send(ControlMessage::Pong).await;
                            }
                            Ok(ControlMessage::Error { message }) => {
                                error!("Server error: {message}");
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Ping(_))) => {
                        // tungstenite handles protocol-level ping/pong automatically
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        warn!("WebSocket closed");
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
            result = &mut send_task => {
                // send_task exited (WS write failed) – reconnect
                match result {
                    Ok(_) => warn!("Send task exited cleanly"),
                    Err(e) => error!("Send task panicked: {e}"),
                }
                break;
            }
        }
    }

    Ok(())
}

// ── Full hop-by-hop header set (RFC 7230 §6.1) ───────────────────────────────

fn is_hop_by_hop(name: &str, connection_tokens: &[String]) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    ) || connection_tokens.iter().any(|t| t == name)
}

/// Parse the Connection header value into individual token names.
fn connection_tokens(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .filter(|(k, _)| k.to_lowercase() == "connection")
        .flat_map(|(_, v)| v.split(',').map(|t| t.trim().to_lowercase()).collect::<Vec<_>>())
        .collect()
}

// ── Forward a single HTTP request to localhost ────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn forward_request(
    http: &HttpClient,
    port: u16,
    request_id: &str,
    method: &str,
    path: &str,
    headers: Vec<(String, String)>,
    body_b64: &str,
    public_url: &str,
    public_host: &str,
) -> ControlMessage {
    let local_url = format!("http://127.0.0.1:{port}{path}");
    let body = B64.decode(body_b64).unwrap_or_default();

    let method_parsed = match reqwest::Method::from_bytes(method.as_bytes()) {
        Ok(m) => m,
        Err(e) => return error_response(request_id, &format!("invalid method: {e}")),
    };

    // Collect hop-by-hop tokens listed in Connection header
    let conn_tokens = connection_tokens(&headers);

    let mut req = http.request(method_parsed, &local_url).body(body);
    for (k, v) in &headers {
        let lower = k.to_lowercase();
        // Drop hop-by-hop headers (complete RFC 7230 set)
        if is_hop_by_hop(&lower, &conn_tokens) {
            continue;
        }
        // Rewrite Host → localhost:{port} (server already set X-Forwarded-Host)
        if lower == "host" {
            req = req.header("host", format!("localhost:{port}"));
            continue;
        }
        req = req.header(k, v);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();

            // ── Response header rewriting ─────────────────────────────────────
            let local_origin = format!("http://localhost:{port}");
            let local_origin_127 = format!("http://127.0.0.1:{port}");

            let resp_headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    let name = k.to_string();
                    let val = v.to_str().ok()?.to_string();

                    // Rewrite Location: localhost → public URL
                    if name.to_lowercase() == "location" {
                        let rewritten = val
                            .replace(&local_origin, public_url)
                            .replace(&local_origin_127, public_url);
                        return Some((name, rewritten));
                    }

                    // Rewrite Set-Cookie Domain=localhost → public_host
                    if name.to_lowercase() == "set-cookie" {
                        let rewritten = rewrite_set_cookie_domain(&val, public_host);
                        return Some((name, rewritten));
                    }

                    Some((name, val))
                })
                .collect();

            let body_bytes = resp.bytes().await.unwrap_or_default();
            let body_b64 = B64.encode(&body_bytes);
            ControlMessage::ResponseOutgoing {
                request_id: request_id.to_string(),
                status,
                headers: resp_headers,
                body_b64,
            }
        }
        Err(e) => {
            warn!("Forward error for {request_id}: {e}");
            error_response(request_id, &format!("upstream error: {e}"))
        }
    }
}

/// Rewrite the `Domain` attribute in a Set-Cookie header value.
/// e.g. "session=abc; Domain=localhost; Path=/" → "session=abc; Domain=foo.onrender.com; Path=/"
fn rewrite_set_cookie_domain(cookie: &str, public_host: &str) -> String {
    cookie
        .split(';')
        .map(|part| {
            let trimmed = part.trim();
            if trimmed.to_lowercase().starts_with("domain=") {
                let current = &trimmed["domain=".len()..];
                if current.eq_ignore_ascii_case("localhost")
                    || current.eq_ignore_ascii_case("127.0.0.1")
                {
                    return format!(" Domain={public_host}");
                }
            }
            part.to_string()
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn error_response(request_id: &str, msg: &str) -> ControlMessage {
    let body = msg.as_bytes().to_vec();
    ControlMessage::ResponseOutgoing {
        request_id: request_id.to_string(),
        status: 502,
        headers: vec![("content-type".into(), "text/plain".into())],
        body_b64: B64.encode(body),
    }
}
