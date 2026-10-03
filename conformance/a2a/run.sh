#!/bin/sh
# Build rustybin, start it on ports 18500-18502 and run the A2A SDK checks.
# Usage: PYTHON=/path/to/venv/bin/python conformance/a2a/run.sh
set -e
cd "$(dirname "$0")/../.."
PYTHON="${PYTHON:-python3}"
cargo build -q
RUSTYBIN_HTTP_PORT=18500 RUSTYBIN_HTTPS_PORT=18501 RUSTYBIN_GRPC_PORT=18502 RUSTYBIN_LOG_LEVEL=warn \
  ./target/debug/rustybin &
PID=$!
trap 'kill $PID 2>/dev/null' EXIT INT TERM
for _ in $(seq 1 50); do
  curl -sf http://127.0.0.1:18500/a2a >/dev/null 2>&1 && break
  sleep 0.2
done
"$PYTHON" conformance/a2a/a2a_conformance.py --base http://127.0.0.1:18500
