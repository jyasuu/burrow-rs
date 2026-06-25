use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::warn;

#[derive(Parser, Debug)]
#[command(name = "burrow", about = "HTTP reverse tunnel – expose localhost to the internet")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the tunnel server
    Server {
        /// Auth token clients must send to register
        #[arg(long, env = "SERVER_SECRET")]
        secret: Option<String>,
        /// Public-facing hostname for tunnel URLs
        #[arg(long, env = "PUBLIC_HOST")]
        public_host: Option<String>,
        /// HTTP listen port
        #[arg(short, long, env = "PORT", default_value = "8080")]
        port: u16,
        /// Max request body size in bytes
        #[arg(long, env = "MAX_BODY_BYTES")]
        max_body_bytes: Option<usize>,
    },
    /// Run the tunnel client
    Client {
        /// Local port to expose
        #[arg(short, long, env = "LOCAL_PORT", default_value = "3000")]
        port: u16,
        /// Tunnel server WebSocket URL
        #[arg(short, long, env = "TUNNEL_SERVER", default_value = "ws://localhost:8080/tunnel/ws")]
        server: String,
        /// Auth token
        #[arg(short, long, env = "TUNNEL_TOKEN", default_value = "changeme")]
        token: String,
        /// Preferred subdomain
        #[arg(long, env = "TUNNEL_SUBDOMAIN")]
        subdomain: Option<String>,
        /// Reconnect delay in seconds
        #[arg(long, default_value = "5")]
        reconnect_delay: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "burrow=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Server { secret, public_host, port, max_body_bytes } => {
            let secret = secret.unwrap_or_else(|| {
                warn!("SERVER_SECRET not set – using insecure default");
                "changeme".to_string()
            });
            let public_host = public_host.unwrap_or_else(|| {
                warn!("PUBLIC_HOST not set – using localhost");
                "localhost".to_string()
            });
            burrow_server::run(burrow_server::ServerOpts {
                secret,
                public_host,
                port,
                max_body_bytes: max_body_bytes.unwrap_or(4 * 1024 * 1024),
            })
            .await
            .map_err(anyhow::Error::from)
        }
        Command::Client { port, server, token, subdomain, reconnect_delay } => {
            let opts = burrow_client::ClientOpts {
                port,
                server,
                token,
                subdomain,
                reconnect_delay,
            };
            burrow_client::run(&opts).await.map_err(anyhow::Error::from)
        }
    }
}
