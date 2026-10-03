#!/usr/bin/env bash
# Build Rustybin, start it on ports 18400-18402, run the MCP conformance
# scripts with the official Python MCP SDK (and optionally the Inspector CLI),
# then stop the server.
#
# Usage: PYTHON=/path/to/venv/bin/python conformance/mcp/run.sh [--inspector]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PYTHON="${PYTHON:-python3}"
export MCP_BASE="http://127.0.0.1:18400"

(cd "$ROOT" && cargo build --quiet)
# Run a renamed copy from a temp dir: demo certificates are written to the
# working directory, and a unique name keeps unrelated `pkill rustybin` away.
TMP="$(mktemp -d)"
BIN="$TMP/mcp-conformance-server"
cp "$ROOT/target/debug/rustybin" "$BIN"

(cd "$TMP" && exec env RUSTYBIN_HTTP_PORT=18400 RUSTYBIN_HTTPS_PORT=18401 RUSTYBIN_GRPC_PORT=18402 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_MCP_CLOCK_TICK_SECS=1 \
  "$BIN") &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true; rm -rf "$TMP"' EXIT

for _ in $(seq 1 50); do
  curl -sf -o /dev/null "$MCP_BASE/" && break
  sleep 0.2
done

status=0
cd "$HERE"
for script in test_streamable_modern.py test_streamable_legacy.py test_sse_legacy.py test_variants.py; do
  echo "#### $script"
  "$PYTHON" -W ignore "$script" || status=1
done
if [[ "${1:-}" == "--inspector" ]]; then
  echo "#### inspector_cli.sh"
  ./inspector_cli.sh || status=1
fi
exit "$status"
