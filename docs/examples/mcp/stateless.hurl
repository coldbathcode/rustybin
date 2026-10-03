# MCP 2026-07-28 (stateless) over Streamable HTTP (docs/src/mcp/index.md)
# Every request carries the protocol version in params._meta and in the
# MCP-Protocol-Version header, plus Mcp-Method (and Mcp-Name where it applies).

# ANCHOR: discover
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: server/discover
{
  "jsonrpc": "2.0", "id": 1, "method": "server/discover",
  "params": {"_meta": {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {}
  }}
}
HTTP 200
[Asserts]
header "X-Rustybin-Mcp-Session" == "stateless"
jsonpath "$.result.supportedVersions" contains "2026-07-28"
jsonpath "$.result.supportedVersions" contains "2025-06-18"
jsonpath "$.result.capabilities.tools" exists
# ANCHOR_END: discover

# ANCHOR: tools_list
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" contains "get_weather"
jsonpath "$.result.tools[*].name" contains "inspect_request"
jsonpath "$.result.ttlMs" exists
jsonpath "$.result.cacheScope" exists
# ANCHOR_END: tools_list

# ANCHOR: tools_call
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_weather
Mcp-Param-City: Paris
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_weather", "arguments": {"city": "Paris"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
header "X-Rustybin-Mcp-Method" == "tools/call"
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.city" == "Paris"
jsonpath "$.result.content[0].type" == "text"
# ANCHOR_END: tools_call

# ANCHOR: header_mismatch
# The routing headers must match the body (gateways route on them).
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: calculate
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "echo", "arguments": {"message": "hi"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 400
[Asserts]
jsonpath "$.error.code" == -32020
jsonpath "$.error.message" contains "Mcp-Name"
# ANCHOR_END: header_mismatch

# ANCHOR: resources_prompts
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: resources/read
Mcp-Name: rustybin://docs/readme
{"jsonrpc": "2.0", "id": 5, "method": "resources/read", "params": {"uri": "rustybin://docs/readme", "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.contents[0].mimeType" == "text/markdown"

POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: prompts/get
Mcp-Name: summarize
{"jsonrpc": "2.0", "id": 6, "method": "prompts/get", "params": {"name": "summarize", "arguments": {"text": "hello", "style": "bullets"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.messages[0].role" == "user"
# ANCHOR_END: resources_prompts

# ANCHOR: named_server
POST {{base_url}}/mcp/servers/crm
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
header "X-Rustybin-Mcp-Server" == "crm"
jsonpath "$.result.tools[*].name" contains "lookup_customer"
jsonpath "$.result.tools[*].name" not contains "get_weather"
# ANCHOR_END: named_server

# ANCHOR: api_key
POST {{base_url}}/mcp/apikey
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: server/discover
{"jsonrpc": "2.0", "id": 8, "method": "server/discover", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 401
[Asserts]
jsonpath "$.error_description" == "X-API-Key header required"

POST {{base_url}}/mcp/apikey
X-API-Key: injected-by-gateway
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: server/discover
{"jsonrpc": "2.0", "id": 9, "method": "server/discover", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
# ANCHOR_END: api_key

# ANCHOR: progress
# With a progressToken, slow_task answers as an SSE stream: progress
# notifications first, then the result.
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: slow_task
{"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "slow_task", "arguments": {"duration_ms": 300, "steps": 3}, "_meta": {"progressToken": "p1", "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
header "Content-Type" contains "text/event-stream"
body contains "notifications/progress"
body contains "\"progressToken\":\"p1\""
body contains "\"id\":10"
# ANCHOR_END: progress

# ANCHOR: pagination
POST {{base_url}}/mcp?page_size=3
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 11, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools" count == 3
jsonpath "$.result.nextCursor" exists
# ANCHOR_END: pagination
