//! burrow-rs tunnel client.
//!
//! Connects to a burrow-server via WebSocket, registers a tunnel, and forwards
//! incoming HTTP requests to a local service. Automatically reconnects on
//! connection loss.
//!
//! # Usage
//!
//! ```bash
//! TUNNEL_SERVER=ws://localhost:8080/tunnel/ws \
//! TUNNEL_TOKEN=my-secret \
//! LOCAL_PORT=3000 \
//! TUNNEL_SUBDOMAIN=myapp \
//! burrow
//! ```

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "burrow-client", about = "Tunnel client – expose localhost to the internet")]
struct Args {
    #[arg(short, long, env = "LOCAL_PORT", default_value = "3000")]
    port: u16,
    #[arg(short, long, env = "TUNNEL_SERVER", default_value = "ws://localhost:8080/tunnel/ws")]
    server: String,
    #[arg(short, long, env = "TUNNEL_TOKEN", default_value = "changeme")]
    token: String,
    #[arg(long, env = "TUNNEL_SUBDOMAIN")]
    subdomain: Option<String>,
    #[arg(long, default_value = "5")]
    reconnect_delay: u64,
}

#[tokio::main]
async fn main() -> Result<(), burrow_client::ClientError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "burrow=info".into()),
        )
        .init();

    let args = Args::parse();
    let opts = burrow_client::ClientOpts {
        port: args.port,
        server: args.server,
        token: args.token,
        subdomain: args.subdomain,
        reconnect_delay: args.reconnect_delay,
    };
    burrow_client::run(&opts).await
}
