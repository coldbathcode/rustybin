#!/usr/bin/env bash
# WebSocket examples (docs/src/reference/websocket.md). Needs websocat.
# Run through docs/examples/run.sh (exit code 77 = tool missing, skipped).
set -euo pipefail
command -v websocat >/dev/null || exit 77
WS="${BASE/http:/ws:}"   # ws://127.0.0.1:18800 under run.sh
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }

out=$(
# ANCHOR: ws_echo
printf 'hello\nworld\n' | websocat "$WS/ws"
# ANCHOR_END: ws_echo
)
expect "$out" hello

out=$(
# ANCHOR: ws_ticker
websocat -n "$WS/ws/time?interval_ms=200&count=3" </dev/null
# ANCHOR_END: ws_ticker
)
expect "$out" '"tick":2'

out=$(
# ANCHOR: ws_subprotocol
curl -si --http1.1 --max-time 1 "$BASE/ws" \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
  -H 'Sec-WebSocket-Protocol: chat.v1, chat.v2' || true
# ANCHOR_END: ws_subprotocol
)
expect "$out" '101 Switching Protocols'
expect "$out" 'sec-websocket-protocol: chat.v1'

out=$(
# ANCHOR: graphql_subscription
printf '%s\n' \
  '{"type":"connection_init"}' \
  '{"id":"1","type":"subscribe","payload":{"query":"subscription { ticker(count: 3, intervalMs: 200) { sequence timestamp } }"}}' \
  | websocat -n --max-messages-rev 5 --protocol graphql-transport-ws "$WS/graphql/ws"
# ANCHOR_END: graphql_subscription
)
expect "$out" '"type":"connection_ack"'
expect "$out" '"sequence":2'
expect "$out" '"type":"complete"'
