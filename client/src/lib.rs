use base64::{engine::general_purpose::STANDARD as B64, Engine};
use burrow_common::ControlMessage;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use std::time::Duration;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection error: {0}")]
    Connection(Box<tokio_tungstenite::tungstenite::Error>),
    #[error("registration rejected: {0}")]
    Rejected(String),
    #[error("unexpected server message")]
    UnexpectedMessage,
    #[error("connection closed")]
    Closed,
    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),
}

impl From<tokio_tungstenite::tungstenite::Error> for ClientError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        ClientError::Connection(Box::new(e))
    }
}

pub struct ClientOpts {
    pub port: u16,
    pub server: String,
    pub token: String,
    pub subdomain: Option<String>,
    pub reconnect_delay: u64,
}

impl Default for ClientOpts {
    fn default() -> Self {
        Self {
            port: 3000,
            server: "ws://localhost:8080/tunnel/ws".into(),
            token: "changeme".into(),
            subdomain: None,
            reconnect_delay: 5,
        }
    }
}

pub async fn run(opts: &ClientOpts) -> Result<(), ClientError> {
    loop {
        info!(
            "Connecting to tunnel server {} → localhost:{}",
            opts.server, opts.port
        );
        match run_session(opts).await {
            Ok(()) => {
                info!(
                    "Session ended cleanly. Reconnecting in {}s\u{2026}",
                    opts.reconnect_delay
                );
            }
            Err(e) => {
                error!(
                    "Session error: {e:#}. Reconnecting in {}s\u{2026}",
                    opts.reconnect_delay
                );
            }
        }
        sleep(Duration::from_secs(opts.reconnect_delay)).await;
    }
}

async fn run_session(args: &ClientOpts) -> Result<(), ClientError> {
    let (ws_stream, _) = connect_async(&args.server).await?;

    let (mut ws_send, mut ws_recv) = ws_stream.split();

    let register = ControlMessage::Register {
        subdomain: args.subdomain.clone(),
        token: args.token.clone(),
    };
    ws_send.send(Message::Text(register.to_json())).await?;

    let (public_url, _subdomain) = match ws_recv.next().await {
        Some(Ok(Message::Text(txt))) => match ControlMessage::from_json(&txt) {
            Ok(ControlMessage::Registered {
                public_url,
                subdomain,
            }) => {
                info!("✅ Tunnel active!");
                info!("   Public URL : {public_url}");
                info!("   Subdomain  : {subdomain}");
                info!("   Forwarding : {public_url} → localhost:{}", args.port);
                (public_url, subdomain)
            }
            Ok(ControlMessage::Error { message }) => {
                return Err(ClientError::Rejected(message));
            }
            _ => return Err(ClientError::UnexpectedMessage),
        },
        _ => return Err(ClientError::Closed),
    };
    let public_host = {
        let url = url::Url::parse(&public_url).ok();
        url.and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| public_url.clone())
    };

    let http = HttpClient::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let local_port = args.port;

    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = resp_rx.recv().await {
            if ws_send.send(Message::Text(msg.to_json())).await.is_err() {
                break;
            }
        }
    });

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
                    Some(Ok(Message::Ping(_))) => {}
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

fn connection_tokens(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .filter(|(k, _)| k.to_lowercase() == "connection")
        .flat_map(|(_, v)| {
            v.split(',')
                .map(|t| t.trim().to_lowercase())
                .collect::<Vec<_>>()
        })
        .collect()
}

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

    let conn_tokens = connection_tokens(&headers);

    let mut req = http.request(method_parsed, &local_url).body(body);
    for (k, v) in &headers {
        let lower = k.to_lowercase();
        if is_hop_by_hop(&lower, &conn_tokens) {
            continue;
        }
        if lower == "host" {
            req = req.header("host", format!("localhost:{port}"));
            continue;
        }
        req = req.header(k, v);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();

            let local_origin = format!("http://localhost:{port}");
            let local_origin_127 = format!("http://127.0.0.1:{port}");

            let resp_headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    let name = k.to_string();
                    let val = v.to_str().ok()?.to_string();

                    if name.to_lowercase() == "location" {
                        let rewritten = val
                            .replace(&local_origin, public_url)
                            .replace(&local_origin_127, public_url);
                        return Some((name, rewritten));
                    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_known_headers() {
        let empty = &[];
        assert!(is_hop_by_hop("connection", empty));
        assert!(is_hop_by_hop("keep-alive", empty));
        assert!(is_hop_by_hop("proxy-authenticate", empty));
        assert!(is_hop_by_hop("proxy-authorization", empty));
        assert!(is_hop_by_hop("te", empty));
        assert!(is_hop_by_hop("trailers", empty));
        assert!(is_hop_by_hop("transfer-encoding", empty));
        assert!(is_hop_by_hop("upgrade", empty));
    }

    #[test]
    fn hop_by_hop_case_insensitive() {
        let empty = &[];
        assert!(is_hop_by_hop("connection", empty));
        assert!(is_hop_by_hop("transfer-encoding", empty));
        assert!(is_hop_by_hop("keep-alive", empty));
    }

    #[test]
    fn hop_by_hop_not_hop_by_hop() {
        let empty = &[];
        assert!(!is_hop_by_hop("content-type", empty));
        assert!(!is_hop_by_hop("host", empty));
        assert!(!is_hop_by_hop("x-forwarded-for", empty));
    }

    #[test]
    fn hop_by_hop_connection_tokens() {
        let tokens = &["x-custom".to_string(), "keep-alive".to_string()];
        assert!(is_hop_by_hop("x-custom", tokens));
        assert!(is_hop_by_hop("keep-alive", tokens));
        assert!(!is_hop_by_hop("x-other", tokens));
    }

    #[test]
    fn connection_tokens_parses() {
        let headers = vec![("connection".into(), "keep-alive, x-foo".into())];
        let tokens = connection_tokens(&headers);
        assert!(tokens.contains(&"keep-alive".to_string()));
        assert!(tokens.contains(&"x-foo".to_string()));
    }

    #[test]
    fn connection_tokens_empty_when_no_connection_header() {
        let headers = vec![("content-type".into(), "text/plain".into())];
        let tokens = connection_tokens(&headers);
        assert!(tokens.is_empty());
    }

    #[test]
    fn connection_tokens_case_insensitive_key() {
        let headers = vec![("Connection".into(), "X-Foo".into())];
        let tokens = connection_tokens(&headers);
        assert!(tokens.contains(&"x-foo".to_string()));
    }

    #[test]
    fn connection_tokens_multiple_connection_headers() {
        let headers = vec![
            ("connection".into(), "a".into()),
            ("connection".into(), "b, c".into()),
        ];
        let tokens = connection_tokens(&headers);
        assert!(tokens.contains(&"a".to_string()));
        assert!(tokens.contains(&"b".to_string()));
        assert!(tokens.contains(&"c".to_string()));
    }

    #[test]
    fn rewrite_domain_localhost_to_public() {
        let result = rewrite_set_cookie_domain(
            "session=abc; Domain=localhost; Path=/",
            "myapp.onrender.com",
        );
        assert_eq!(result, "session=abc; Domain=myapp.onrender.com; Path=/");
    }

    #[test]
    fn rewrite_domain_127_0_0_1_to_public() {
        let result = rewrite_set_cookie_domain("token=xyz; Domain=127.0.0.1", "example.com");
        assert_eq!(result, "token=xyz; Domain=example.com");
    }

    #[test]
    fn rewrite_domain_case_insensitive() {
        let result = rewrite_set_cookie_domain("x=y; DOMAIN=LOCALHOST", "public.io");
        assert_eq!(result, "x=y; Domain=public.io");
    }

    #[test]
    fn rewrite_domain_other_domain_untouched() {
        let result = rewrite_set_cookie_domain("x=y; Domain=.example.com", "public.io");
        assert_eq!(result, "x=y; Domain=.example.com");
    }

    #[test]
    fn rewrite_domain_multiple_cookies() {
        let result = rewrite_set_cookie_domain(
            "a=1; Domain=localhost; Path=/, b=2; Domain=localhost",
            "p.io",
        );
        assert_eq!(result, "a=1; Domain=p.io; Path=/, b=2; Domain=p.io");
    }

    #[test]
    fn rewrite_domain_no_domain_unchanged() {
        let result = rewrite_set_cookie_domain("session=abc; Path=/", "p.io");
        assert_eq!(result, "session=abc; Path=/");
    }

    #[test]
    fn rewrite_domain_empty_cookie() {
        let result = rewrite_set_cookie_domain("", "p.io");
        assert_eq!(result, "");
    }

    #[test]
    fn error_response_format() {
        let msg = error_response("req-1", "upstream error: connection refused");
        match msg {
            ControlMessage::ResponseOutgoing {
                request_id,
                status,
                headers,
                body_b64,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(status, 502);
                assert!(headers
                    .iter()
                    .any(|(k, v)| k == "content-type" && v == "text/plain"));
                let body = B64.decode(&body_b64).unwrap();
                assert_eq!(
                    String::from_utf8_lossy(&body),
                    "upstream error: connection refused"
                );
            }
            _ => panic!("wrong variant"),
        }
    }
}
