# The management MCP server behind control-plane authentication
# (docs/src/agent-mcp.md), on the secured documentation instance
# (RUSTYBIN_HOSTED_MODE=true: control auth jwt).

# ANCHOR: secured_no_token
POST {{secured_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Bearer realm=\"rustybin-control\", resource_metadata=\"{{secured_url}}/.well-known/oauth-protected-resource/_rustybin/mcp\""
# ANCHOR_END: secured_no_token

# ANCHOR: secured_console_scope
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{console_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" includes "list_mocks"
jsonpath "$.result.tools[*].name" includes "try_request"
jsonpath "$.result.tools[*].name" not includes "create_mock"
jsonpath "$.result.tools[*].name" not includes "apply_config"

# A tool the scope does not allow answers a tool error, never a protocol error.
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{console_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: set_health
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "set_health", "arguments": {"healthy": false}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "not allowed"
# ANCHOR_END: secured_console_scope

# ANCHOR: secured_admin
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_instance
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "get_instance", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.structuredContent.caller.admin" == true
jsonpath "$.result.structuredContent.control_auth" == "jwt"
jsonpath "$.result.structuredContent.caller.rights" includes "admin"
# ANCHOR_END: secured_admin
