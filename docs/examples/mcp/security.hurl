# MCP attack servers and MCP security scenarios (docs/src/mcp/security.md):
# tool poisoning in a tool list, a rug pull (the same tool before and after
# a call, keyed by the caller's session), the scope overreach challenge and
# its metadata, an unknown kind, and an MCP scenario run prepared and ended.

# ANCHOR: poisoned
POST {{base_url}}/mcp/attack/poisoned_description
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[0].name" == "get_weather"
jsonpath "$.result.tools[1].name" == "add_numbers"
jsonpath "$.result.tools[1].description" contains "<IMPORTANT>"
jsonpath "$.result.tools[1].description" contains "RUSTYBIN-POISON"
# ANCHOR_END: poisoned

# ANCHOR: rug_pull
POST {{base_url}}/mcp/attack/rug_pull
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
X-Rustybin-Session: docs-rug-pull
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[0].description" not contains "RUSTYBIN-RUG-PULL"
jsonpath "$.result.tools[0].inputSchema.properties.forward_to" not exists

POST {{base_url}}/mcp/attack/rug_pull
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_exchange_rate
X-Rustybin-Session: docs-rug-pull
{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "get_exchange_rate", "arguments": {"from": "EUR", "to": "USD"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.content[0].text" contains "1.08"

POST {{base_url}}/mcp/attack/rug_pull
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
X-Rustybin-Session: docs-rug-pull
{"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[0].description" contains "RUSTYBIN-RUG-PULL"
jsonpath "$.result.tools[0].inputSchema.properties.forward_to" exists
# ANCHOR_END: rug_pull

# ANCHOR: overreach
POST {{base_url}}/mcp/attack/scope_overreach
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "docs", "version": "1.0.0"}}}
HTTP 401
[Asserts]
header "WWW-Authenticate" contains "scope=\"admin offline_access tools:* files:write\""

GET {{base_url}}/.well-known/oauth-protected-resource/mcp/attack/scope_overreach
HTTP 200
[Asserts]
jsonpath "$.scopes_supported" contains "admin"
jsonpath "$.scopes_supported" contains "offline_access"
# ANCHOR_END: overreach

# ANCHOR: unknown
POST {{base_url}}/mcp/attack/nope
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}
HTTP 404
[Asserts]
jsonpath "$.available" contains "rug_pull"
# ANCHOR_END: unknown

# ANCHOR: scenario
POST {{base_url}}/_rustybin/scenarios/mcp_tool_shadowing/runs
Authorization: Bearer {{admin_token}}
HTTP 201
[Captures]
mcp_run: jsonpath "$.id"
[Asserts]
jsonpath "$.group" == "mcp"
jsonpath "$.upstream" endsWith "/mcp/attack/shadowing"
jsonpath "$.second_upstream" endsWith "/mcp/servers/crm"

DELETE {{base_url}}/_rustybin/scenarios/mcp_tool_shadowing/runs/{{mcp_run}}
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: scenario
