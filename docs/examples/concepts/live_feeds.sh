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
