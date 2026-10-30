# Build an instance with an agent (docs/src/agent-mcp.md): the calls the
# build_instance prompt walks an agent through, on the open documentation
# instance (mock AI provider, an admin token). Every call is a tools/call of
# the management MCP server at /_rustybin/mcp.

# ANCHOR: build_assist_status
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: assist_status
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "assist_status", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.enabled" == true
jsonpath "$.result.structuredContent.provider" == "mock"
jsonpath "$.result.structuredContent.providers[0].key.set" exists
# ANCHOR_END: build_assist_status

# ANCHOR: build_generate_spec
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: generate_spec
{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "generate_spec", "arguments": {"description": "a parcel tracking API: shipments with a tracking number, status and events; list, get and create shipments, list a shipment's events"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.text" contains "openapi"
jsonpath "$.result.structuredContent.preview.operations" count > 0
jsonpath "$.result.structuredContent.preview.report.errors" == 0
jsonpath "$.result.structuredContent.mock" not exists
jsonpath "$.result.content[0].text" contains "Nothing was created"
# ANCHOR_END: build_generate_spec

# ANCHOR: build_create
# Creating from the generated document is confirmed: without elicitation the
# agent passes confirm true once the user agreed.
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: generate_spec
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "generate_spec", "arguments": {"description": "a parcel tracking API: shipments with a tracking number, status and events", "create": true, "name": "docs-parcels"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "confirm"

POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: generate_spec
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "generate_spec", "arguments": {"description": "a parcel tracking API: shipments with a tracking number, status and events", "create": true, "name": "docs-parcels", "confirm": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.mock.name" == "docs-parcels"
jsonpath "$.result.structuredContent.mock.url" contains "/mock/docs-parcels"
# ANCHOR_END: build_create

# ANCHOR: build_coverage
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: check_mock
{"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "check_mock", "arguments": {"name": "docs-parcels"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.coverage" exists
jsonpath "$.result.structuredContent.coverage.without" exists
# ANCHOR_END: build_coverage

# ANCHOR: build_examples
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: generate_examples
{"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "generate_examples", "arguments": {"name": "docs-parcels", "source": "generated", "operations": ["listShipments"], "apply": true, "confirm": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.applied" == true
jsonpath "$.result.structuredContent.source" == "generated"

POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: deploy_mock
{"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "deploy_mock", "arguments": {"name": "docs-parcels"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
# ANCHOR_END: build_examples

# ANCHOR: build_publish
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: publish_portal_item
{"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "publish_portal_item", "arguments": {"id": "mock:docs-parcels", "title": "Parcels", "summary": "Track shipments and their events.", "tags": ["logistics"], "confirm": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.published.title" == "Parcels"

POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: list_portal_items
{"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "list_portal_items", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.enabled" == true
jsonpath "$.result.structuredContent.items[?(@.id == 'mock:docs-parcels')].published.title" contains "Parcels"
# ANCHOR_END: build_publish

# ANCHOR: build_export
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: export_config
{"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "export_config", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.version" == 2
jsonpath "$.result.structuredContent.yaml" contains "docs-parcels"
jsonpath "$.result.structuredContent.sections" contains "portal"
jsonpath "$.result.content[0].text" contains "never values"
# ANCHOR_END: build_export

# ANCHOR: build_git
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: git_status
{"jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": {"name": "git_status", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.commits" exists
jsonpath "$.result.content[0].text" contains "No repository configured"

POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: git_push
{"jsonrpc": "2.0", "id": 12, "method": "tools/call", "params": {"name": "git_push", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "confirm"
# ANCHOR_END: build_git

# ANCHOR: build_agents
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: list_agents
{"jsonrpc": "2.0", "id": 13, "method": "tools/call", "params": {"name": "list_agents", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.agents" count > 0
jsonpath "$.result.structuredContent.policy.default_ceiling" exists
# ANCHOR_END: build_agents

# ANCHOR: build_mcp_call
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: mcp_call
{"jsonrpc": "2.0", "id": 14, "method": "tools/call", "params": {"name": "mcp_call", "arguments": {"target": "/mock/docs-parcels/_rustybin/mcp", "method": "tools/list"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.untrusted_result.tools" count > 0
jsonpath "$.result.structuredContent.result" not exists
# ANCHOR_END: build_mcp_call

# ANCHOR: build_cleanup
POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: unpublish_portal_item
{"jsonrpc": "2.0", "id": 15, "method": "tools/call", "params": {"name": "unpublish_portal_item", "arguments": {"id": "mock:docs-parcels"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false

POST {{base_url}}/_rustybin/mcp
Authorization: Bearer {{admin_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: delete_mock
{"jsonrpc": "2.0", "id": 16, "method": "tools/call", "params": {"name": "delete_mock", "arguments": {"name": "docs-parcels", "confirm": true}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
# ANCHOR_END: build_cleanup
