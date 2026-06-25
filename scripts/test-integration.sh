#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────────
# Integration test for burrow-rs
# Spins up server + local service + client, runs HTTP tests against the tunnel.
# Usage: bash scripts/test-integration.sh
# ──────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SERVER_PID=
LOCAL_PID=
CLIENT_PID=
CLIENT2_PID=
CLIENT3_PID=
ECHO_PID=
ECHO2_PID=

cleanup() {
    local exit_code=$?
    echo "=== Cleaning up ==="
    for pid in "$CLIENT3_PID" "$ECHO2_PID" "$CLIENT2_PID" "$ECHO_PID" "$CLIENT_PID" "$SERVER_PID" "$LOCAL_PID"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    echo "=== Cleanup done ==="
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

PASS=0
FAIL=0
assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        echo "  PASS: $label"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $label (expected: '$expected', got: '$actual')"
        FAIL=$((FAIL + 1))
    fi
}

assert_contains() {
    local label="$1" needle="$2" haystack="$3"
    if echo "$haystack" | grep -Fq "$needle"; then
        echo "  PASS: $label"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $label (expected to contain: '$needle')"
        FAIL=$((FAIL + 1))
    fi
}

# ── Build (skip if already built) ──────────────────────────────────────────────
BUILT=${1:-}
if [[ "$BUILT" != "skipbuild" ]]; then
    echo "=== Building ==="
    cargo build --release -p burrow-server -p burrow-client 2>&1
fi

# ── Pick free ports ───────────────────────────────────────────────────────────
pick_port() {
    python3 -c "import socket; s=socket.socket(); s.bind(('',0)); print(s.getsockname()[1]); s.close()"
}
LOCAL_PORT=$(pick_port)
SERVER_PORT=$(pick_port)
HEADER_PORT=$(pick_port)
ECHO2_PORT=$(pick_port)

# ── Start local service (Python HTTP server) ──────────────────────────────────
echo "=== Starting local service on port $LOCAL_PORT ==="
mkdir -p /tmp/burrow-test-www
echo '<!DOCTYPE html><title>hello</title><p>Hello World</p>' > /tmp/burrow-test-www/index.html
python3 -m http.server "$LOCAL_PORT" --directory /tmp/burrow-test-www &
LOCAL_PID=$!
sleep 1
if ! kill -0 "$LOCAL_PID" 2>/dev/null; then
    echo "ERROR: local service failed to start"
    exit 1
fi
echo "Local service PID: $LOCAL_PID"

# ── Start server ──────────────────────────────────────────────────────────────
echo "=== Starting burrow-server on port $SERVER_PORT ==="
SERVER_SECRET=test-secret \
PUBLIC_HOST=localhost:$SERVER_PORT \
PORT=$SERVER_PORT \
RUST_LOG=burrow_server=info \
./target/release/burrow-server &
SERVER_PID=$!
sleep 2
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: server failed to start"
    exit 1
fi
echo "Server PID: $SERVER_PID"

# ── Health check ──────────────────────────────────────────────────────────────
echo "=== Health check ==="
HEALTH=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$SERVER_PORT/health")
assert_eq "health endpoint" "200" "$HEALTH"

# ── Start client ──────────────────────────────────────────────────────────────
echo "=== Starting burrow client ==="
TUNNEL_SERVER="ws://127.0.0.1:$SERVER_PORT/tunnel/ws" \
TUNNEL_TOKEN=test-secret \
LOCAL_PORT=$LOCAL_PORT \
TUNNEL_SUBDOMAIN=testapp \
RUST_LOG=burrow=info \
./target/release/burrow-client --reconnect-delay 10 &
CLIENT_PID=$!
sleep 2
if ! kill -0 "$CLIENT_PID" 2>/dev/null; then
    echo "ERROR: client failed to start"
    exit 1
fi
echo "Client PID: $CLIENT_PID"

# ── Wait for tunnel to register ──────────────────────────────────────────────
sleep 2

# ── Test 1: Basic GET ─────────────────────────────────────────────────────────
echo "=== Test: GET / ==="
RESP=$(curl -s -i "http://127.0.0.1:$SERVER_PORT/testapp/" 2>&1)
STATUS=$(echo "$RESP" | head -1 | awk '{print $2}')
BODY=$(echo "$RESP" | tail -1)
assert_eq "GET status" "200" "$STATUS"
assert_contains "GET body" "Hello World" "$BODY"

# ── Test 2: X-Forwarded-* headers injected ────────────────────────────────────
echo "=== Test: X-Forwarded-* headers ==="
# Start a simple echo server to inspect headers
echo "Starting header-echo server on port $HEADER_PORT"
python3 -c "
import http.server, socketserver, json, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        h = dict(self.headers)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(json.dumps(h).encode())
    def log_message(self, *a):
        pass
httpd = socketserver.TCPServer(('', ${HEADER_PORT}), H)
httpd.serve_forever()
" &
ECHO_PID=$!
sleep 1

# Connect a second client to a different subdomain pointing at the echo server
TUNNEL_SERVER="ws://127.0.0.1:$SERVER_PORT/tunnel/ws" \
TUNNEL_TOKEN=test-secret \
LOCAL_PORT=$HEADER_PORT \
TUNNEL_SUBDOMAIN=headers \
RUST_LOG=burrow=info \
./target/release/burrow-client --reconnect-delay 10 &
CLIENT2_PID=$!
sleep 2

HEADER_RESP=$(curl -s "http://127.0.0.1:$SERVER_PORT/headers/" 2>&1)
HEADER_RESP_LOW=$(echo "$HEADER_RESP" | tr '[:upper:]' '[:lower:]')
assert_contains "X-Forwarded-For present" "x-forwarded-for" "$HEADER_RESP_LOW"
assert_contains "X-Forwarded-Proto is https" "https" "$HEADER_RESP"
assert_contains "Host header rewritten" "127.0.0.1" "$HEADER_RESP"
assert_contains "X-Burrow-Request-Id present" "x-burrow-request-id" "$HEADER_RESP_LOW"
kill "$CLIENT2_PID" 2>/dev/null || true
kill "$ECHO_PID" 2>/dev/null || true

# ── Test 3: POST with body ────────────────────────────────────────────────────
echo "=== Test: POST with body ==="
# Start an echo server that supports POST
python3 -c "
import http.server, socketserver
class E(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('content-length', 0))
        body = self.rfile.read(length)
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
httpd = socketserver.TCPServer(('', ${ECHO2_PORT}), E)
print('echo server on', ${ECHO2_PORT})
httpd.serve_forever()
" &
ECHO2_PID=$!
sleep 1

TUNNEL_SERVER="ws://127.0.0.1:$SERVER_PORT/tunnel/ws" \
TUNNEL_TOKEN=test-secret \
LOCAL_PORT=$ECHO2_PORT \
TUNNEL_SUBDOMAIN=posttest \
RUST_LOG=burrow=info \
./target/release/burrow-client --reconnect-delay 10 &
CLIENT3_PID=$!
sleep 2

POST_STATUS=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:$SERVER_PORT/posttest/" -d "Hello from POST")
assert_eq "POST status" "200" "$POST_STATUS"
POST_BODY=$(curl -s -X POST "http://127.0.0.1:$SERVER_PORT/posttest/" -d "Hello from POST")
assert_contains "POST response body" "Hello from POST" "$POST_BODY"

kill "$CLIENT3_PID" 2>/dev/null || true
kill "$ECHO2_PID" 2>/dev/null || true

# ── Test 4: 404 for unknown subdomain ─────────────────────────────────────────
echo "=== Test: Unknown subdomain ==="
NOT_FOUND=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$SERVER_PORT/nonexistent/" 2>&1)
assert_eq "unknown subdomain returns 404" "404" "$NOT_FOUND"

# ── Test 5: Invalid token → error ────────────────────────────────────────────
echo "=== Test: Invalid token ==="
# Run a quick client with bad token and capture output
BAD_TOKEN_OUT=$(TUNNEL_SERVER="ws://127.0.0.1:$SERVER_PORT/tunnel/ws" \
    TUNNEL_TOKEN=wrong-token \
    LOCAL_PORT=$LOCAL_PORT \
    RUST_LOG=burrow=info \
    timeout 3 ./target/release/burrow-client --reconnect-delay 1 2>&1 || true)
assert_contains "bad token rejected" "invalid token" "$BAD_TOKEN_OUT"

# ── Test 6: Subdomain already in use ──────────────────────────────────────────
echo "=== Test: Duplicate subdomain ==="
DUP_OUT=$(TUNNEL_SERVER="ws://127.0.0.1:$SERVER_PORT/tunnel/ws" \
    TUNNEL_TOKEN=test-secret \
    LOCAL_PORT=$LOCAL_PORT \
    TUNNEL_SUBDOMAIN=testapp \
    RUST_LOG=burrow=info \
    timeout 3 ./target/release/burrow-client --reconnect-delay 1 2>&1 || true)
assert_contains "duplicate subdomain rejected" "already in use" "$DUP_OUT"

# ── Summary ────────────────────────────────────────────────────────────────────
echo ""
echo "========================"
echo "  Results: $PASS passed, $FAIL failed"
echo "========================"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
