#!/usr/bin/env bash
# GUI end-to-end test for Pi Chat.
#
# Starts a mock OpenAI-compatible SSE server, the Vite dev server, and
# tauri-driver (WebKitWebDriver) pointed at the debug build, then drives the real
# webview with e2e.mjs. The app runs with an isolated XDG_DATA_HOME so your real
# chats/config are not touched. The API-key handling in e2e.mjs never overwrites
# an existing keychain entry.
#
# Prerequisites:
#   - node
#   - tauri-driver:  cargo install tauri-driver --version 2.0.6 --locked
#   - a built debug binary (built automatically below if missing)
#   - a desktop session (the app window is launched on the current display)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
E2E_DIR="$ROOT/e2e"
APP="$ROOT/src-tauri/target/debug/pi-chat"
MOCK_PORT="${MOCK_PORT:-8317}"
WD_PORT="${WD_PORT:-4444}"
VITE_PORT=1420

command -v node >/dev/null || { echo "node not found on PATH" >&2; exit 1; }
command -v tauri-driver >/dev/null || {
  echo "tauri-driver not found. Install it with:" >&2
  echo "  cargo install tauri-driver --version 2.0.6 --locked" >&2
  exit 1
}

if [ ! -x "$APP" ]; then
  echo "→ building pi-chat (debug)…"
  (cd "$ROOT/src-tauri" && cargo build)
fi

# Isolated app data dir; the app reads config from here, so seed the mock
# endpoint + model before it starts.
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pi-chat-e2e.XXXXXX")"
mkdir -p "$DATA_DIR/com.rp.chat"
cat > "$DATA_DIR/com.rp.chat/config.json" <<EOF
{
  "baseUrl": "http://127.0.0.1:${MOCK_PORT}/v1",
  "model": "mock-model",
  "preferences": "",
  "thinkingLevel": "",
  "modelOverrides": {}
}
EOF

pids=()
kill_port() {
  local port="$1" pid
  pid="$(ss -ltnp "sport = :$port" 2>/dev/null | grep -oP 'pid=\K[0-9]+' | head -1 || true)"
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
}
cleanup() {
  for pid in "${pids[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  # tauri-driver can leave the app as an orphan; only kill instances that use
  # our isolated data dir, never a user's running app.
  for pid in $(pgrep -f "$APP" 2>/dev/null || true); do
    if tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -q "$DATA_DIR"; then
      kill "$pid" 2>/dev/null || true
    fi
  done
  kill_port "$MOCK_PORT"
  kill_port "$WD_PORT"
  kill_port "$VITE_PORT"
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

wait_port() {
  local port="$1" i
  for i in $(seq 1 100); do
    if ss -ltn 2>/dev/null | grep -qE "[:.]${port}[[:space:]]"; then return 0; fi
    sleep 0.2
  done
  echo "timed out waiting for port $port" >&2
  return 1
}

echo "→ mock LLM on :$MOCK_PORT"
MOCK_PORT="$MOCK_PORT" node "$E2E_DIR/mock-llm.mjs" & pids+=($!)
wait_port "$MOCK_PORT"

echo "→ Vite dev server on :$VITE_PORT"
node "$ROOT/node_modules/.bin/vite" & pids+=($!)
wait_port "$VITE_PORT"

echo "→ tauri-driver on :$WD_PORT"
XDG_DATA_HOME="$DATA_DIR" NO_AT_BRIDGE=1 tauri-driver --port "$WD_PORT" & pids+=($!)
wait_port "$WD_PORT"

echo "→ driving the app (screenshots → e2e/screenshots/)"
APP="$APP" WD_URL="http://127.0.0.1:$WD_PORT" OUT="$E2E_DIR/screenshots" \
  node "$E2E_DIR/e2e.mjs"
