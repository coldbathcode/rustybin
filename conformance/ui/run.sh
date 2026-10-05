#!/usr/bin/env bash
# Build Rustybin, start it on ports 18700-18702 (control plane open) and a
# second instance on 18703 with RUSTYBIN_CONTROL_AUTH=jwt, run the browser
# end-to-end check of the web console (light and dark mode, plus the sign-in
# flow), then stop the servers.
#
# Usage: PYTHON=/path/to/venv/bin/python conformance/ui/run.sh
# (the venv needs `pip install playwright pyjwt cryptography`; Chromium comes
# from $PLAYWRIGHT_BROWSERS_PATH or `python -m playwright install chromium`).
#
# RUSTYBIN_UI_PORT moves the four ports (default 18700).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PYTHON="${PYTHON:-python3}"
PORT="${RUSTYBIN_UI_PORT:-18700}"
JWT_PORT=$((PORT + 3))
export RUSTYBIN_URL="http://127.0.0.1:$PORT"
export RUSTYBIN_JWT_URL="http://127.0.0.1:$JWT_PORT"
export SCREENSHOT_DIR="${SCREENSHOT_DIR:-$HERE/screenshots}"

(cd "$ROOT" && cargo build --quiet)
# Run a renamed copy from a temp dir: demo certificates are written to the
# working directory, and a unique name keeps unrelated `pkill rustybin` away.
TMP="$(mktemp -d)"
BIN="$TMP/ui-conformance-server"
cp "$ROOT/target/debug/rustybin" "$BIN"

# Signing key of the control-plane JWTs (fresh for every run).
openssl genpkey -algorithm ed25519 -out "$TMP/control.key" 2>/dev/null
openssl pkey -in "$TMP/control.key" -pubout -out "$TMP/control.pub"
export CONTROL_JWT_PRIVATE_KEY="$TMP/control.key"
export CONTROL_JWT_AUDIENCE="ui-conformance"

(cd "$TMP" && exec env RUSTYBIN_HTTP_PORT="$PORT" RUSTYBIN_HTTPS_PORT=$((PORT + 1)) RUSTYBIN_GRPC_PORT=$((PORT + 2)) \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn "$BIN") &
SERVER=$!
mkdir -p "$TMP/jwt"
(cd "$TMP/jwt" && exec env RUSTYBIN_HTTP_PORT="$JWT_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_CONTROL_AUTH=jwt \
  RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="$(cat "$TMP/control.pub")" \
  RUSTYBIN_CONTROL_JWT_AUDIENCE="$CONTROL_JWT_AUDIENCE" \
  RUSTYBIN_ADMIN_TOKEN=ui-admin-token \
  RUSTYBIN_CONSOLE_TITLE="Acme demo pod" \
  RUSTYBIN_CONSOLE_BACKLINK="https://portal.example.com/pods/acme" \
  "$BIN") &
JWT_SERVER=$!
trap 'kill $SERVER $JWT_SERVER 2>/dev/null || true; rm -rf "$TMP"' EXIT

for url in "$RUSTYBIN_URL" "$RUSTYBIN_JWT_URL"; do
  for _ in $(seq 1 50); do
    curl -sf -o /dev/null "$url/" && break
    sleep 0.2
  done
done

"$PYTHON" "$HERE/console_e2e.py"
