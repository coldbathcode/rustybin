#!/usr/bin/env bash
# The realtime API of the mock LLM (docs/src/ai/realtime.md). Needs websocat
# and curl. Run through docs/examples/run.sh (exit code 77 = tool missing,
# skipped).
set -euo pipefail
command -v websocat >/dev/null || exit 77
BASE="${BASE:-http://localhost:8080}"
WS="${BASE/http:/ws:}"
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }

out=$(
# ANCHOR: session
# One event a line; the sleep keeps the socket open for the answer.
(
  echo '{"type": "session.update", "session": {"instructions": "Be terse."}}'
  echo '{"type": "conversation.item.create", "item": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "What did you receive?"}]}}'
  echo '{"type": "response.create"}'
  sleep 1
) | websocat --header='X-Request-Id: docs-realtime-1' \
  "$WS/ai/openai/v1/realtime?model=rustybin-echo&tokens_per_second=0"
# ANCHOR_END: session
)
expect "$out" '"type":"session.created"'
expect "$out" '"type":"session.updated"'
expect "$out" '"type":"response.text.delta"'
expect "$out" '"type":"response.done"'
expect "$out" '"status":"completed"'
expect "$out" '"type":"rate_limits.updated"'

out=$(
# ANCHOR: log
curl -s "$BASE/ai/requests/docs-realtime-1"
# ANCHOR_END: log
)
expect "$out" '"kind":"realtime"'
expect "$out" '"type":"response.done"'

out=$(
# ANCHOR: tool
(
  echo '{"type": "session.update", "session": {"tools": [{"type": "function", "name": "get_weather", "description": "Get the current weather for a location", "parameters": {"type": "object", "properties": {"location": {"type": "string"}}, "required": ["location"]}}]}}'
  echo '{"type": "conversation.item.create", "item": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "What is the weather in Paris?"}]}}'
  echo '{"type": "response.create"}'
  sleep 1
) | websocat "$WS/ai/openai/v1/realtime?tokens_per_second=0"
# ANCHOR_END: tool
)
expect "$out" '"type":"response.function_call_arguments.done"'
expect "$out" '"name":"get_weather"'

out=$(
# ANCHOR: refused
curl -s -o /dev/null -w '%{http_code}' --http1.1 "$BASE/ai/openai/v1/realtime?fail=429" \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=='
# ANCHOR_END: refused
)
expect "$out" 429

out=$(
# ANCHOR: drop
# The connection is dropped after the third server event.
(
  echo '{"type": "conversation.item.create", "item": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]}}'
  echo '{"type": "response.create"}'
  sleep 1
) | websocat "$WS/ai/openai/v1/realtime?fail=drop_after:3&tokens_per_second=0" || true
# ANCHOR_END: drop
)
expect "$out" '"type":"response.created"'
if grep -q '"type":"response.done"' <<<"$out"; then
  echo "the session went on after the drop" >&2
  exit 1
fi
