# The management MCP server (docs/src/agent-mcp.md): JSON-RPC calls an MCP
# client makes to /_rustybin/mcp, on the open documentation instance.

# ANCHOR: discover
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: server/discover
{"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result._meta['io.modelcontextprotocol/serverInfo'].name" == "rustybin-manage"
jsonpath "$.result.instructions" contains "untrusted_*"
jsonpath "$.result.capabilities.tools" exists
jsonpath "$.result.capabilities.prompts" exists
# ANCHOR_END: discover

# ANCHOR: tools_list
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" includes "get_instance"
jsonpath "$.result.tools[*].name" includes "create_mock"
jsonpath "$.result.tools[*].name" includes "try_request"
jsonpath "$.result.tools[*].name" includes "wait_for_requests"
jsonpath "$.result.tools[*].name" includes "apply_config"
jsonpath "$.result.tools[?(@.name == 'delete_mock')].annotations.destructiveHint" includes true
jsonpath "$.result.tools[?(@.name == 'get_mock')].annotations.readOnlyHint" includes true
# ANCHOR_END: tools_list

# ANCHOR: get_instance
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_instance
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_instance", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.instance_id" == "docs-examples"
jsonpath "$.result.structuredContent.caller.admin" == true
jsonpath "$.result.structuredContent.urls.console" == "{{base_url}}/ui/"
jsonpath "$.result.content[0].text" contains "Rustybin"
# ANCHOR_END: get_instance

# ANCHOR: create_mock
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: create_mock
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "create_mock", "arguments": {
  "name": "agent-orders",
  "spec": {"openapi": "3.0.3", "info": {"title": "Orders", "version": "1"}, "paths": {
    "/orders/{id}": {"get": {"operationId": "getOrder",
      "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}],
      "responses": {"200": {"description": "ok", "content": {"application/json": {
        "schema": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}, "total": {"type": "number"}}}}}},
        "404": {"description": "missing"}}}}}},
  "operations": {"getOrder": {"rules": [{"name": "unknown order", "when": {"path.id": {"equals": "0"}}, "respond": {"status": 404}}]}}
}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.content[0].text" startsWith "Created mock \"agent-orders\""
jsonpath "$.result.structuredContent.state" == "live"
jsonpath "$.result.structuredContent.url" == "{{base_url}}/mock/agent-orders"
jsonpath "$.result.structuredContent.operations[0].rules" == 1
# ANCHOR_END: create_mock

# ANCHOR: try_request
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: try_request
{"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "try_request", "arguments": {"path": "/mock/agent-orders/orders/0"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.status" == 404
jsonpath "$.result.structuredContent.session" startsWith "mcp-"
jsonpath "$.result.structuredContent.untrusted_headers" exists
jsonpath "$.result.structuredContent.untrusted_body" exists
jsonpath "$.result.content[0].text" contains "answered 404"
# ANCHOR_END: try_request

# ANCHOR: control_plane_refused
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: try_request
{"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "try_request", "arguments": {"path": "/_rustybin/config"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "control-plane paths"
# ANCHOR_END: control_plane_refused

# ANCHOR: traffic
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_mock_traffic
{"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "get_mock_traffic", "arguments": {"name": "agent-orders", "include_bodies": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.structuredContent.total" == 1
jsonpath "$.result.structuredContent.requests[0].operation" == "getOrder"
jsonpath "$.result.structuredContent.requests[0].status" == 404
jsonpath "$.result.structuredContent.requests[0].source" == "rule"
jsonpath "$.result.structuredContent.requests[0].untrusted_headers" exists
jsonpath "$.result.content[0].text" contains "untrusted_* fields"
# ANCHOR_END: traffic

# ANCHOR: delete_needs_confirmation
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: delete_mock
{"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "delete_mock", "arguments": {"name": "agent-orders"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "\"confirm\": true"

# The mock still exists.
GET {{base_url}}/_rustybin/mocks/agent-orders
HTTP 200
# ANCHOR_END: delete_needs_confirmation

# ANCHOR: delete_confirmed
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: delete_mock
{"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "delete_mock", "arguments": {"name": "agent-orders", "confirm": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.deleted" == "agent-orders"

GET {{base_url}}/_rustybin/mocks/agent-orders
HTTP 404
# ANCHOR_END: delete_confirmed

# ANCHOR: issue_token
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: issue_token
{"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "issue_token", "arguments": {"scope": "mcp:tools"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Captures]
agent_token: jsonpath "$.result.structuredContent.access_token"
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.claims.scope" == "mcp:tools"
jsonpath "$.result.structuredContent.expires_in" == 3600

# The token opens the OAuth-protected mock MCP server.
POST {{base_url}}/mcp/protected
Authorization: Bearer {{agent_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: server/discover
{"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
# ANCHOR_END: issue_token

# ANCHOR: resources
POST {{base_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: resources/read
Mcp-Name: rustybin://config.schema.json
{"jsonrpc": "2.0", "id": 11, "method": "resources/read", "params": {"uri": "rustybin://config.schema.json", "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.contents[0].mimeType" == "application/schema+json"
jsonpath "$.result.contents[0].text" contains "mocks"
# ANCHOR_END: resources
