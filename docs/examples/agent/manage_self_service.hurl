# The management MCP server on a self-service demo (docs/src/agent-mcp.md):
# a visitor's agent creates a mock, which starts a visit, and the manage
# token scopes what it may do afterwards.

# ANCHOR: ss_agent_create
POST {{ss_url}}/_rustybin/mcp
X-Rustybin-Session: docs-agent-visitor
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: create_mock
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "create_mock", "arguments": {
  "name": "orders",
  "spec": {"openapi": "3.0.3", "info": {"title": "Orders", "version": "1"}, "paths": {
    "/orders/{id}": {"get": {"operationId": "getOrder",
      "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}],
      "responses": {"200": {"description": "ok", "content": {"application/json": {
        "schema": {"type": "object", "properties": {"id": {"type": "integer"}}}}}}}}}}}
}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Captures]
agent_mock: jsonpath "$.result.structuredContent.name"
agent_mock_token: jsonpath "$.result.structuredContent.manage_token"
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.name" matches /^orders-[a-z2-7]{6}$/
jsonpath "$.result.structuredContent.owner" == "visit"
jsonpath "$.result.structuredContent.url" contains ".mocks.example"
jsonpath "$.result.content[0].text" contains "manage_token"
# ANCHOR_END: ss_agent_create

# ANCHOR: ss_agent_token
POST {{ss_url}}/_rustybin/mcp
X-Rustybin-Mock-Token: {{agent_mock_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_visit
{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "get_visit", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.mocks" includes "{{agent_mock}}"
jsonpath "$.result.structuredContent.next_extension" == "free"

# Without the token the mock does not exist for the caller.
POST {{ss_url}}/_rustybin/mcp
X-Rustybin-Session: docs-agent-other
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_mock
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_mock", "arguments": {"name": "{{agent_mock}}"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.structuredContent.status" == 404
# ANCHOR_END: ss_agent_token

# ANCHOR: ss_agent_cleanup
POST {{ss_url}}/_rustybin/mcp
X-Rustybin-Mock-Token: {{agent_mock_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: delete_mock
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "delete_mock", "arguments": {"name": "{{agent_mock}}", "confirm": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
# ANCHOR_END: ss_agent_cleanup
