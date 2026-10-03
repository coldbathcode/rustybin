# MCP handshake era (2025-11-25 / 2025-06-18 / 2025-03-26) and the OAuth-protected variant (docs/src/mcp/index.md)

# ANCHOR: initialize
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "docs", "version": "1.0"}}}
HTTP 200
[Captures]
session: header "Mcp-Session-Id"
[Asserts]
jsonpath "$.result.protocolVersion" == "2025-06-18"
jsonpath "$.result.serverInfo.name" == "rustybin-mcp"

POST {{base_url}}/mcp
Mcp-Session-Id: {{session}}
MCP-Protocol-Version: 2025-06-18
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "method": "notifications/initialized"}
HTTP 202
# ANCHOR_END: initialize

# ANCHOR: session_call
POST {{base_url}}/mcp
Mcp-Session-Id: {{session}}
MCP-Protocol-Version: 2025-06-18
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "calculate", "arguments": {"expression": "(2 + 3) * 4"}}}
HTTP 200
[Asserts]
jsonpath "$.result.content[0].text" contains "20"
# ANCHOR_END: session_call

# ANCHOR: session_end
DELETE {{base_url}}/mcp
Mcp-Session-Id: {{session}}
HTTP 200

# The session is gone afterwards.
POST {{base_url}}/mcp
Mcp-Session-Id: {{session}}
MCP-Protocol-Version: 2025-06-18
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 3, "method": "tools/list"}
HTTP 404
# ANCHOR_END: session_end

# ANCHOR: protected_challenge
POST {{base_url}}/mcp/protected
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "docs", "version": "1.0"}}}
HTTP 401
[Asserts]
header "WWW-Authenticate" contains "resource_metadata=\"{{base_url}}/.well-known/oauth-protected-resource/mcp/protected\""
header "WWW-Authenticate" contains "scope=\"mcp:tools\""
# ANCHOR_END: protected_challenge

# ANCHOR: protected_metadata
GET {{base_url}}/.well-known/oauth-protected-resource/mcp/protected
HTTP 200
[Asserts]
jsonpath "$.resource" == "{{base_url}}/mcp/protected"
jsonpath "$.authorization_servers[0]" == "{{base_url}}"
jsonpath "$.scopes_supported" contains "mcp:tools:write"
# ANCHOR_END: protected_metadata

# ANCHOR: protected_token
# A token for this resource (RFC 8707) from the built-in identity provider.
POST {{base_url}}/oauth/token
[BasicAuth]
rustybin: secret
[Form]
grant_type: client_credentials
scope: mcp:tools
resource: {{base_url}}/mcp/protected
HTTP 200
[Captures]
mcp_token: jsonpath "$.access_token"

POST {{base_url}}/mcp/protected
Authorization: Bearer {{mcp_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: inspect_request
{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "inspect_request", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.tokenClaims.aud" == "{{base_url}}/mcp/protected"
jsonpath "$.result.structuredContent.tokenClaims.scope" == "mcp:tools"
jsonpath "$.result.structuredContent.headers.authorization" contains "masked"
# ANCHOR_END: protected_token

# ANCHOR: protected_step_up
# cancel_order needs mcp:tools:write: 403 with an insufficient_scope challenge.
POST {{base_url}}/mcp/protected
Authorization: Bearer {{mcp_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: cancel_order
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "cancel_order", "arguments": {"order_id": "o1"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 403
[Asserts]
header "WWW-Authenticate" contains "error=\"insufficient_scope\""
header "WWW-Authenticate" contains "scope=\"mcp:tools:write\""
# ANCHOR_END: protected_step_up
