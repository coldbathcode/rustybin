#!/usr/bin/env bash
# The command line (docs/src/cli-guide.md) against an instance in password
# mode started for this script: sign in, a few nouns, --json, a tail. Run
# through docs/examples/run.sh, which exports RUSTYBIN_BIN and BASE.
set -euo pipefail
BIN="${RUSTYBIN_BIN:-$(command -v rustybin || true)}"
[[ -x "$BIN" ]] || { echo "set RUSTYBIN_BIN to a rustybin binary" >&2; exit 1; }
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }
work="$(mktemp -d)"
busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
PORT=$(( ${BASE##*:} + 13 ))
while busy "$PORT"; do PORT=$(( PORT + 1 )); done
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; wait "${pids[@]}" 2>/dev/null || true; rm -rf "$work"' EXIT
cd "$work"
# The profiles stay in this script's temporary directory.
export RUSTYBIN_CONFIG_HOME="$work/config"
rustybin() { "$BIN" "$@"; }

# An instance with the default password sign-in (rustybin / rustybin).
RUSTYBIN_HTTP_PORT=$PORT RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_STATE_DIR="$work/state" "$BIN" &
pids+=($!)
URL="http://127.0.0.1:$PORT"
for _ in $(seq 1 100); do curl -fs -o /dev/null "$URL/" && break; sleep 0.1; done

cat > orders.yaml <<'YAML'
openapi: 3.0.3
info: { title: Orders, version: "1" }
paths:
  /orders:
    get:
      operationId: listOrders
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema: { type: array, items: { type: object, properties: { id: { type: integer } } } }
YAML

out=$(
# ANCHOR: login
# Sign in once; the token lands in a profile and the next commands need no URL.
echo rustybin | rustybin login --url "$URL" --password-stdin
rustybin whoami
# ANCHOR_END: login
)
expect "$out" "Signed in to $URL as rustybin"
expect "$out" 'user: rustybin'

out=$(
# ANCHOR: nouns
rustybin mocks create orders --spec orders.yaml --quiet
rustybin mocks list
rustybin settings set console.title "Payments mocks"
rustybin config export | grep -m1 "^version"
rustybin help mocks | head -4
# ANCHOR_END: nouns
)
expect "$out" 'orders'
expect "$out" 'changed: console.title'
expect "$out" 'version: 2'
expect "$out" 'rustybin mocks: OpenAPI mocks'

out=$(
# ANCHOR: json
# --json for scripts: the instance's answer as it is.
rustybin mocks get orders --json | jq -r '.state'
rustybin status --json | jq -r '.version'
# ANCHOR_END: json
)
expect "$out" 'live'

out=$(
# ANCHOR: tail
# Wait for the one request a test makes: at most one event, at most 5 seconds.
(sleep 0.5; curl -s -o /dev/null "$URL/get?from=the-test") &
rustybin requests tail --max 1 --timeout 5
# ANCHOR_END: tail
)
expect "$out" 'request:'
expect "$out" 'from=the-test'

# Exit codes: a refused operation is 1, usage 2, a bad credential 3.
set +e
rustybin mocks get nope --quiet; echo "exit $?"
rustybin mocks nope; echo "exit $?"
rustybin status --token wrong --quiet; echo "exit $?"
set -e
rustybin logout --quiet
echo "ok"
