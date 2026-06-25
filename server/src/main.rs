//! burrow-rs tunnel server.
//!
//! Accepts WebSocket connections from tunnel clients at `/tunnel/ws`, and
//! proxies external HTTP requests through to registered clients. Supports
//! multiple concurrent tunnels, header rewriting, rate limiting, and
//! keep-alive heartbeats.
//!
//! # Environment variables
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `SERVER_SECRET` | `"changeme"` | Auth token clients must send to register |
//! | `PUBLIC_HOST` | `"localhost"` | Public-facing hostname for tunnel URLs |
//! | `PORT` | `8080` | HTTP listen port |
//! | `MAX_BODY_BYTES` | `4194304` | Max request body size in bytes |
//! | `RUST_LOG` | `"burrow_server=debug,tower_http=info"` | Tracing filter |

use tracing::warn;

#[tokio::main]
async fn main() -> Result<(), burrow_server::ServerError> {
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

    let max_body_bytes: usize = std::env::var("MAX_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4 * 1024 * 1024);

    burrow_server::run(burrow_server::ServerOpts {
        secret,
        public_host,
        port,
        max_body_bytes,
    })
    .await
}
