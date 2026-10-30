#!/usr/bin/env bash
# Never-ending SSE feeds (docs/src/concepts/inspector.md, docs/src/reference/request-bin.md).
# Run through docs/examples/run.sh. Each feed is read for two seconds while a
# request is sent in the background.
set -euo pipefail
BASE="${BASE:-http://localhost:8080}"
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }

(sleep 0.5; curl -s -o /dev/null -H 'X-Rustybin-Session: docs-feed' "$BASE/anything/feed-demo") &
out=$(
# ANCHOR: inspector_feed
curl -sN --max-time 2 "$BASE/_rustybin/requests/stream?session=docs-feed"
# ANCHOR_END: inspector_feed
) || true
expect "$out" 'event: request'
expect "$out" '"path":"/anything/feed-demo"'

bin=$(curl -s -X POST "$BASE/bin" | sed -E 's/.*"id":"([0-9a-f]+)".*/\1/')
(sleep 0.5; curl -s -o /dev/null -X POST "$BASE/bin/$bin/orders" -d 'hello') &
out=$(
# ANCHOR: bin_feed
curl -sN --max-time 2 "$BASE/bin/$bin/requests/stream"
# ANCHOR_END: bin_feed
) || true
expect "$out" 'event: request'
expect "$out" 'id: 1'

# The AI agents feed (docs/src/agent-mcp.md, "Managing agents"): a management
# MCP call is made in the background and shows up as a `call` event.
ADMIN_TOKEN="${ADMIN_TOKEN:-docs-admin-token}"
(sleep 0.5; curl -s -o /dev/null -X POST "$BASE/_rustybin/mcp" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' -H 'MCP-Protocol-Version: 2026-07-28' \
  -H 'Mcp-Method: tools/call' -H 'Mcp-Name: get_instance' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_instance","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"docs-feed-agent","version":"1.0.0"}}}}') &
out=$(
# ANCHOR: agents_feed
curl -sN --max-time 2 -H "Authorization: Bearer $ADMIN_TOKEN" "$BASE/_rustybin/agents/events"
# ANCHOR_END: agents_feed
) || true
expect "$out" 'event: call'
expect "$out" 'get_instance'
expect "$out" 'docs-feed-agent'
