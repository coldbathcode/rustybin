#!/usr/bin/env bash
# Drive the server with the MCP Inspector CLI (TypeScript SDK client).
# Needs Node 22 and network access to the npm registry (npx downloads the inspector).
# Usage: MCP_BASE=http://127.0.0.1:18400 ./inspector_cli.sh
set -euo pipefail

BASE="${MCP_BASE:-http://127.0.0.1:18400}"
INSPECTOR=(npx -y @modelcontextprotocol/inspector --cli)
fail=0

expect() {
  local name="$1" needle="$2"
  shift 2
  local out
  if ! out="$(timeout 120 "${INSPECTOR[@]}" "$@" 2>&1)"; then
    echo "  FAIL $name (exit code)"
    echo "$out" | head -20
    fail=1
  elif grep -q -- "$needle" <<<"$out"; then
    echo "  ok   $name"
  else
    echo "  FAIL $name (missing $needle)"
    echo "$out" | head -20
    fail=1
  fi
}

echo "== MCP Inspector CLI against $BASE"
expect "streamable tools/list" '"get_weather"' "$BASE/mcp" --transport http --method tools/list
expect "streamable tools/call get_weather" '"conditions"' "$BASE/mcp" --transport http \
  --method tools/call --tool-name get_weather --tool-arg city=Paris
expect "streamable resources/read" 'Rustybin MCP demo server' "$BASE/mcp" --transport http \
  --method resources/read --uri rustybin://docs/readme
expect "streamable prompts/get" 'Summarize the following text' "$BASE/mcp" --transport http \
  --method prompts/get --prompt-name summarize --prompt-args text=hello
expect "named server tools/list" '"lookup_customer"' "$BASE/mcp/servers/crm" --transport http --method tools/list
expect "api key header" '"echo"' "$BASE/mcp/apikey" --transport http --method tools/list \
  --header "X-API-Key: inspector"
expect "legacy sse tools/list" '"slow_task"' "$BASE/mcp/sse" --transport sse --method tools/list

exit "$fail"
