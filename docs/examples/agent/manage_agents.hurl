# Managing the AI agents of the management MCP server (docs/src/agent-mcp.md,
# "Managing agents"), on the secured documentation instance: an agent named by
# its clientInfo, its record, a ceiling, revoke and restore.

# ANCHOR: agents_call
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_instance
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "get_instance", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-agent", "version": "1.0.0"}}}}
HTTP 200
# ANCHOR_END: agents_call

# ANCHOR: agents_list
GET {{secured_url}}/_rustybin/agents
Authorization: Bearer {{admin_token}}
HTTP 200
[Captures]
agent_id: jsonpath "$.agents[?(@.name == 'docs-agent')].id"
[Asserts]
jsonpath "$.agents[?(@.name == 'docs-agent')].credential.kind" == "admin"
jsonpath "$.agents[?(@.name == 'docs-agent')].version" == "1.0.0"
jsonpath "$.agents[?(@.name == 'docs-agent')].calls.ok" >= 1
jsonpath "$.agents[?(@.name == 'docs-agent')].rights.ceiling" == "full"
jsonpath "$.policy.default_ceiling" == "full"
# ANCHOR_END: agents_list

# ANCHOR: agents_rights
PATCH {{secured_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"rights": {"ceiling": "read-only", "deny": ["try_request"]}, "note": "documentation example"}
HTTP 200
[Asserts]
jsonpath "$.rights.ceiling" == "read-only"
jsonpath "$.rights.source" == "agent"
jsonpath "$.note" == "documentation example"

# The same token under the same client name now sees read tools only.
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-agent", "version": "1.0.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" includes "list_mocks"
jsonpath "$.result.tools[*].name" not includes "create_mock"
jsonpath "$.result.tools[*].name" not includes "try_request"
# ANCHOR_END: agents_rights

# ANCHOR: agents_revoke
POST {{secured_url}}/_rustybin/agents/{{agent_id}}/revoke
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.revoked" == true
jsonpath "$.note" contains "RUSTYBIN_ADMIN_TOKEN"

POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: list_mocks
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "list_mocks", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-agent", "version": "1.0.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "revoked"
# ANCHOR_END: agents_revoke

# ANCHOR: agents_restore
POST {{secured_url}}/_rustybin/agents/{{agent_id}}/restore
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.restored" == true

PATCH {{secured_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"rights": null, "note": null}
HTTP 200
[Asserts]
jsonpath "$.rights.source" == "default"
# ANCHOR_END: agents_restore
