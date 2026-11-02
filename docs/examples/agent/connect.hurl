# Connecting a coding agent (docs/src/coding-agents.md), on the secured
# documentation instance with the admin token: the first call a connected
# agent makes, the agent in the agents list, and a ceiling that fits every
# tool on one tools/list page. The rights are reset at the end, since rights
# follow the credential and later examples use the same admin token.

# ANCHOR: connect_check
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_instance
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "get_instance", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-coding-agent", "version": "1.0.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.content[0].text" contains "Caller rights: read, traffic, manage, admin"
jsonpath "$.result.structuredContent.caller.admin" == true
jsonpath "$.result.structuredContent.urls.console" == "{{secured_url}}/ui/"
# ANCHOR_END: connect_check

# ANCHOR: connect_seen
GET {{secured_url}}/_rustybin/agents
Authorization: Bearer {{admin_token}}
HTTP 200
[Captures]
agent_id: jsonpath "$.agents[?(@.name == 'docs-coding-agent')].id"
[Asserts]
jsonpath "$.agents[?(@.name == 'docs-coding-agent')].credential.kind" == "admin"
jsonpath "$.agents[?(@.name == 'docs-coding-agent')].calls.ok" >= 1
# ANCHOR_END: connect_seen

# ANCHOR: connect_ceiling
# Every tool an admin may use takes two pages of tools/list.
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-coding-agent", "version": "1.0.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools" count == 100
jsonpath "$.result.nextCursor" exists

# The manage ceiling: reads and mock changes, nothing instance-wide.
PATCH {{secured_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"rights": {"ceiling": "manage"}, "note": "builds mocks for the checkout team"}
HTTP 200
[Asserts]
jsonpath "$.rights.ceiling" == "manage"

# Now one page holds them all.
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-coding-agent", "version": "1.0.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools" count < 100
jsonpath "$.result.nextCursor" not exists
jsonpath "$.result.tools[*].name" contains "create_mock"
jsonpath "$.result.tools[*].name" contains "generate_examples"
jsonpath "$.result.tools[*].name" not contains "update_settings"
jsonpath "$.result.tools[*].name" not contains "create_run"
# ANCHOR_END: connect_ceiling

# Back to full rights for the examples that follow.
PATCH {{secured_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"rights": null, "note": null}
HTTP 200
