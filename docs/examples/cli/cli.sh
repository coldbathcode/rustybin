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

out=$(
# ANCHOR: users
# Invite a teammate: the answer is the link to send them (shown once).
rustybin users invite alice --role editor --display-name "Alice Doe"
rustybin roles save catalog-reader --permission mocks.read,traffic.read --description "Reads mocks and traffic"
rustybin users update alice --role catalog-reader
rustybin users list
rustybin me
# ANCHOR_END: users
)
expect "$out" "$URL/ui/#/join/rbi_"
expect "$out" 'catalog-reader'
expect "$out" 'invited'
expect "$out" 'name: rustybin'
rustybin users delete alice --yes --quiet
rustybin roles delete catalog-reader --yes --quiet

# A secret an agent asked for (docs/src/agent-mcp.md, "A secret at an agent's
# request"). The agent here is a curl call on the management server with the
# profile's sign-in token.
token="$(jq -r '.profiles[].token' "$RUSTYBIN_CONFIG_HOME/profiles.json")"
agent_asks() {
  curl -fsS "$URL/_rustybin/mcp" -H "Authorization: Bearer $token" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2026-07-28' -H 'Mcp-Method: tools/call' -H 'Mcp-Name: request_secret' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"request_secret","arguments":{"setting":"'"$1"'","reason":"'"$2"'"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"docs-agent","version":"1.0"}}}}' \
    | jq -r '.result.structuredContent.request.id'
}
rustybin settings set agents.secret_requests true >/dev/null
agent_asks notify.webhook_token "the nightly schedule posts its failures to the team webhook" >/dev/null
declined="$(agent_asks git.token "the configuration repository")"
printf 'whk_docs-example-value-k3Qx\n' > webhook-token.txt

out=$(
# ANCHOR: secret_requests
# What agents asked for: the setting, whose agent, and the agent's reason (its claim).
rustybin settings requests --state pending
request="$(rustybin settings requests --state pending --json | jq -r '.requests[] | select(.setting.key == "notify.webhook_token") | .id')"
# The value on stdin (or prompted without echo), never an argument: it goes to
# the settings store, and the agent reads only that the request was fulfilled.
rustybin settings fulfil "$request" --stdin < webhook-token.txt
rustybin settings get notify.webhook_token --json | jq -c '.value | {set, hint}'
# ANCHOR_END: secret_requests
)
expect "$out" 'docs-agent'
expect "$out" 'the nightly schedule posts its failures'
expect "$out" 'notify.webhook_token is set'
expect "$out" '{"set":true,"hint":"k3Qx"}'
if grep -q 'whk_docs-example-value' <<<"$out"; then echo "the value was printed" >&2; exit 1; fi
out=$(rustybin settings decline "$declined")
expect "$out" 'is declined: nothing was set'
rustybin settings unset agents.secret_requests >/dev/null

# Exit codes: a refused operation is 1, usage 2, a bad credential 3.
set +e
rustybin mocks get nope --quiet; echo "exit $?"
rustybin mocks nope; echo "exit $?"
rustybin status --token wrong --quiet; echo "exit $?"
set -e
rustybin logout --quiet
echo "ok"
