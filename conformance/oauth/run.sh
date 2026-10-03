#!/usr/bin/env bash
# Build Rustybin, start it on port 18470 and run the OAuth / OIDC walk-through.
# Usage: PYTHON=/path/to/venv/bin/python conformance/oauth/run.sh   (needs httpx)
set -euo pipefail
cd "$(dirname "$0")/../.."
PYTHON="${PYTHON:-python3}"
cargo build --quiet
# Run a renamed copy from a temp dir: the demo PKI is written to the working
# directory, and a unique name keeps unrelated `pkill rustybin` away.
TMP="$(mktemp -d)"
cp target/debug/rustybin "$TMP/oauth-conformance-server"
(cd "$TMP" && exec env RUSTYBIN_HTTP_PORT=18470 RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn ./oauth-conformance-server) &
SERVER=$!
trap 'kill "$SERVER" 2>/dev/null || true; rm -rf "$TMP"' EXIT
for _ in $(seq 1 50); do
  curl -fs -o /dev/null http://127.0.0.1:18470/ && break
  sleep 0.2
done
"$PYTHON" conformance/oauth/oauth_flow.py http://127.0.0.1:18470
