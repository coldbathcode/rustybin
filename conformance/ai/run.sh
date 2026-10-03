#!/usr/bin/env bash
# Run the mock LLM conformance scripts against fresh Rustybin instances.
#
#   PYTHON=/path/to/venv/bin/python conformance/ai/run.sh
#
# Starts two instances: :18300 (default settings) and :18301 (with
# RUSTYBIN_AI_API_KEY=conformance-secret, for expected-key checks).
# Requires: openai, anthropic, google-genai, httpx, pydantic.
set -euo pipefail

cd "$(dirname "$0")/../.."
PY="${PYTHON:-python3}"
PORT="${RUSTYBIN_CONFORMANCE_PORT:-18300}"
AUTH_PORT=$((PORT + 1))

cargo build --quiet
BIN=target/debug/rustybin

RUSTYBIN_HTTP_PORT="$PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUST_LOG=warn "$BIN" &
P1=$!
RUSTYBIN_HTTP_PORT="$AUTH_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUST_LOG=warn \
  RUSTYBIN_AI_API_KEY=conformance-secret "$BIN" &
P2=$!
trap 'kill "$P1" "$P2" 2>/dev/null || true' EXIT

for port in "$PORT" "$AUTH_PORT"; do
  for _ in $(seq 1 100); do
    if curl -fs "http://127.0.0.1:$port/" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done
done

export RUSTYBIN_URL="http://127.0.0.1:$PORT"
export RUSTYBIN_AUTH_URL="http://127.0.0.1:$AUTH_PORT"
export PYTHONWARNINGS="${PYTHONWARNINGS:-ignore::DeprecationWarning}"

cd conformance/ai
status=0
for t in test_openai.py test_anthropic.py test_gemini.py test_http_providers.py; do
  echo "### $t"
  "$PY" "$t" || status=1
done
exit "$status"
