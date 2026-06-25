#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────────
# burrow-rs demo – quick local tunnel in 4 terminals
# ──────────────────────────────────────────────────────────────────────────────
set -euo pipefail

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║                   burrow-rs Demo                           ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

BUILD=${1:-build}  # pass "nobuild" to skip cargo build

if [[ "$BUILD" != "nobuild" ]]; then
    echo "▸ Building (release)..."
    cargo build --release -p burrow
fi

# Config
PORT=${PORT:-8080}
SECRET=${SECRET:-demo-token}
SUBDOMAIN=${SUBDOMAIN:-demo}
LOCAL_PORT=${LOCAL_PORT:-3000}

echo "▸ Server port : $PORT"
echo "▸ Secret      : $SECRET"
echo "▸ Subdomain   : $SUBDOMAIN"
echo "▸ Local port  : $LOCAL_PORT"
echo ""

cat <<EOF
Run these commands in separate terminals:

╔═══════════════════════════════════════════════════════════════════╗
║  Terminal 1 – Start a local service                              ║
║    python3 -m http.server $LOCAL_PORT                            ║
║                                                                   ║
║  Terminal 2 – Start burrow server                                  ║
║    SERVER_SECRET=$SECRET PUBLIC_HOST=localhost PORT=$PORT        ║
║    RUST_LOG=burrow_server=info                                   ║
║    ./target/release/burrow server                                 ║
║                                                                   ║
║  Terminal 3 – Start burrow client                                  ║
║    TUNNEL_SERVER=ws://localhost:$PORT/tunnel/ws                  ║
║    TUNNEL_TOKEN=$SECRET                                          ║
║    LOCAL_PORT=$LOCAL_PORT                                        ║
║    TUNNEL_SUBDOMAIN=$SUBDOMAIN                                   ║
║    RUST_LOG=burrow=info                                          ║
║    ./target/release/burrow client                                ║
║                                                                   ║
║  Terminal 4 – Test the tunnel                                     ║
║    curl http://localhost:$PORT/$SUBDOMAIN/                        ║
║    curl -X POST -d 'hi' http://localhost:$PORT/$SUBDOMAIN/foo    ║
╚═══════════════════════════════════════════════════════════════════╝
EOF
