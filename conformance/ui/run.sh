#!/usr/bin/env bash
# Build Rustybin, start it on ports 18700-18702, run the browser end-to-end
# check of the web console (light and dark mode), then stop the server.
#
# Usage: PYTHON=/path/to/venv/bin/python conformance/ui/run.sh
# (the venv needs `pip install playwright`; Chromium comes from
# $PLAYWRIGHT_BROWSERS_PATH or `python -m playwright install chromium`).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PYTHON="${PYTHON:-python3}"
export RUSTYBIN_URL="http://127.0.0.1:18700"
export SCREENSHOT_DIR="${SCREENSHOT_DIR:-$HERE/screenshots}"

(cd "$ROOT" && cargo build --quiet)
# Run a renamed copy from a temp dir: demo certificates are written to the
# working directory, and a unique name keeps unrelated `pkill rustybin` away.
TMP="$(mktemp -d)"
BIN="$TMP/ui-conformance-server"
cp "$ROOT/target/debug/rustybin" "$BIN"

(cd "$TMP" && exec env RUSTYBIN_HTTP_PORT=18700 RUSTYBIN_HTTPS_PORT=18701 RUSTYBIN_GRPC_PORT=18702 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn "$BIN") &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true; rm -rf "$TMP"' EXIT

for _ in $(seq 1 50); do
  curl -sf -o /dev/null "$RUSTYBIN_URL/" && break
  sleep 0.2
done

"$PYTHON" "$HERE/console_e2e.py"
