#!/usr/bin/env bash
# A harness scenario in a file of its own next to rustybin.yaml
# (docs/src/configuration-as-code.md): an instance in load test mode loads
# the document, reads the scenario's file and saves the scenario; the same
# document sent to the API is told to carry the scenario inline. Run through
# docs/examples/run.sh, which exports RUSTYBIN_BIN and BASE.
set -euo pipefail
BIN="${RUSTYBIN_BIN:-$(command -v rustybin || true)}"
[[ -x "$BIN" ]] || { echo "set RUSTYBIN_BIN to a rustybin binary" >&2; exit 1; }
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }
work="$(mktemp -d)"
# A port above the runner's and the source control example's, skipping any
# that something else holds.
busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
PORT=$(( ${BASE##*:} + 20 ))
while busy "$PORT"; do PORT=$(( PORT + 1 )); done
ADMIN="docs-scenario-admin"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; wait "${pids[@]}" 2>/dev/null || true; rm -rf "$work"' EXIT
cd "$work"

# ANCHOR: files
# The scenario in its own file (the one `rustybin harness run` takes), and a
# document that names it instead of holding it.
mkdir scenarios
cat > scenarios/checkout.yaml <<'YAML'
name: checkout
description: A steady trickle through the gateway's checkout route.
target: http://127.0.0.1:8080
source:
  request:
    method: GET
    path: /status/200
pattern:
  type: constant
  rate: 5
  for: 10s
YAML
cat > rustybin.yaml <<'YAML'
version: 2
harness:
  scenarios:
    - name: checkout
      file: scenarios/checkout.yaml
YAML
# ANCHOR_END: files

# ANCHOR: start
RUSTYBIN_HTTP_PORT=$PORT RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open RUSTYBIN_ADMIN_TOKEN=$ADMIN \
  RUSTYBIN_HARNESS=on RUSTYBIN_HARNESS_TARGETS=127.0.0.1 \
  RUSTYBIN_CONFIG_FILE="$work/rustybin.yaml" \
  "$BIN" &
# ANCHOR_END: start
pids+=($!)
A="http://127.0.0.1:$PORT"
for _ in $(seq 1 100); do curl -fs -o /dev/null "$A/" && break; sleep 0.1; done

out=$(
# ANCHOR: saved
# The scenario is saved under the entry's name, read from its file.
curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/harness/scenarios/checkout"
# ANCHOR_END: saved
)
echo "$out"
expect "$out" 'A steady trickle'
expect "$out" '/status/200'

out=$(
# ANCHOR: api_inline
# A document sent to the API never makes the server read a path: the entry is
# a problem that says to send the scenario inline, as a mock's spec: is.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"document": "version: 2\nharness:\n  scenarios:\n    - name: checkout\n      file: scenarios/checkout.yaml\n", "dry_run": true}' \
  "$A/_rustybin/config/apply"
# ANCHOR_END: api_inline
)
echo "$out"
expect "$out" 'send scenario inline'
expect "$out" '"ok":false'
echo "ok"
