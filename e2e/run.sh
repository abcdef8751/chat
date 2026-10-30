#!/usr/bin/env bash
# GUI end-to-end test for Pi Chat.
#
# Starts a mock OpenAI-compatible SSE server, the Vite dev server, and
# tauri-driver (WebKitWebDriver) pointed at the debug build, then drives the real
# webview with e2e.mjs. The app runs with an isolated XDG_DATA_HOME so your real
# chats/config are not touched, AND with PI_SECRETS_DIR pointing into that same
# isolated dir, so every keychain read/write the app makes (provider keys, Brave
# key, sync session) lands in plain files under the temp dir instead of your
# real OS secret service. e2e.mjs snapshots the real keychain before and after
# the run and asserts it is byte-identical.
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
# The debug app loads its frontend from `devUrl` in src-tauri/tauri.conf.json,
# which is fixed at http://localhost:1420 — so the harness MUST serve on this
# port and must never silently test a server it did not start.
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
# endpoint + provider + model before it starts. `model` is deliberately a value
# the mock does NOT serve: the harness asserts "mock-model" only appears in the
# picker after the provider key is set and the endpoint fetch actually ran.
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pi-chat-e2e.XXXXXX")"
mkdir -p "$DATA_DIR/com.rp.chat" "$DATA_DIR/secrets"
cat > "$DATA_DIR/com.rp.chat/config.json" <<EOF
{
  "baseUrl": "http://127.0.0.1:${MOCK_PORT}/v1",
  "model": "e2e-never-listed",
  "providers": [
    {
      "id": "e2e-mock",
      "name": "E2E Mock",
      "baseUrl": "http://127.0.0.1:${MOCK_PORT}/v1",
      "defaultModel": null,
      "catalogId": null
    }
  ],
  "activeProviderId": "e2e-mock",
  "preferences": "",
  "thinkingLevel": "",
  "modelOverrides": {}
}
EOF
# Never touch the user's real keychain: redirect ALL secret storage into the
# isolated data dir (see src-tauri/src/secrets.rs). Inherited by the app,
# which tauri-driver spawns.
export PI_SECRETS_DIR="$DATA_DIR/secrets"

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

port_in_use() {
  ss -ltn 2>/dev/null | grep -qE "[:.]${1}[[:space:]]"
}

# Fail fast if any of our three ports is already occupied: the app's devUrl is
# fixed at :1420 and we must never silently test a server we did not start, and
# stale mock/WebDriver processes would be equally misleading.
for port in "$VITE_PORT" "$MOCK_PORT" "$WD_PORT"; do
  if port_in_use "$port"; then
    echo "error: port $port is already in use — another e2e run or a dev server is" >&2
    echo "       running. Stop it first and retry." >&2
    exit 1
  fi
done

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
# A deliberate, generous gap between reasoning chunks: the "(live)" reasoning
# popup and the mid-stream node-identity check both need the reasoning phase to
# still be in flight when they run, and the default 700 ms put the first content
# chunk inside the same second as those assertions.
MOCK_PORT="$MOCK_PORT" MOCK_THINK_GAP="${MOCK_THINK_GAP:-1200}" \
  node "$E2E_DIR/mock-llm.mjs" & pids+=($!)
wait_port "$MOCK_PORT"

echo "→ Vite dev server on :$VITE_PORT"
# --strictPort: vite must exit when the port is taken, never silently move to
# another port while the app connects to the fixed devUrl.
node "$ROOT/node_modules/.bin/vite" --port "$VITE_PORT" --strictPort & pids+=($!)
wait_port "$VITE_PORT"

echo "→ tauri-driver on :$WD_PORT"
XDG_DATA_HOME="$DATA_DIR" NO_AT_BRIDGE=1 tauri-driver --port "$WD_PORT" & pids+=($!)
wait_port "$WD_PORT"

echo "→ driving the app (screenshots → e2e/screenshots/)"
APP="$APP" WD_URL="http://127.0.0.1:$WD_PORT" OUT="$E2E_DIR/screenshots" \
  node "$E2E_DIR/e2e.mjs"