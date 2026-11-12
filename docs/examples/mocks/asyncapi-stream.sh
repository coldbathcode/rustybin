#!/usr/bin/env bash
# The channels of an event mock over WebSocket and server-sent events
# (docs/src/mocks/asyncapi.md). Needs websocat and curl. Run through
# docs/examples/run.sh (exit code 77 = tool missing, skipped).
set -euo pipefail
command -v websocat >/dev/null || exit 77
BASE="${BASE:-http://localhost:8080}"
WS="${BASE/http:/ws:}"
ADMIN_TOKEN="${ADMIN_TOKEN:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }
remove() { curl -s -o /dev/null -X DELETE "$BASE/_rustybin/mocks/order-events" -H "Authorization: Bearer $ADMIN_TOKEN"; }
remove
trap remove EXIT

out=$(curl -s -X POST "$BASE/_rustybin/mocks/apply" -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/yaml' --data-binary "@$HERE/order-events.yaml")
expect "$out" 'order-events'

out=$(
# ANCHOR: websocket
# Rustybin publishes an orderCreated every half second while you listen.
sleep 3 | websocat "$WS/events/order-events/orders-created"
# ANCHOR_END: websocket
)
expect "$out" '"id":1001'
expect "$out" '"id":1002'

out=$(
# ANCHOR: sse
curl -sN --max-time 3 -H 'Accept: text/event-stream' \
  "$BASE/events/order-events/orders-created" || true
# ANCHOR_END: sse
)
expect "$out" 'event: orderCreated'
expect "$out" 'data: {"id":'

out=$(
# ANCHOR: invalid
# A message that is not valid is answered with an error event.
(
  echo '{"id": 1001}'
  sleep 1
) | websocat "$WS/events/order-events/orders-cancel"
# ANCHOR_END: invalid
)
expect "$out" '"type":"invalid_message"'
expect "$out" 'missing required property'

out=$(
# ANCHOR: reply
# The document declares a reply: a quoteRequest is answered with a quote.
(
  echo '{"sku": "SKU-7"}'
  sleep 1
) | websocat "$WS/events/order-events/quotes"
# ANCHOR_END: reply
)
expect "$out" '"price":'
