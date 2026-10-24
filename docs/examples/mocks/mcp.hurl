# OpenAPI mocks: the MCP server of a mock (docs/src/mocks/mcp.md). Uses the
# built-in sample; switches its security on and back off, and excludes one
# operation and restores it.

# ANCHOR: discover
POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: server/discover
{"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.capabilities.tools" exists
jsonpath "$.result.capabilities.resources" exists
jsonpath "$.result._meta['io.modelcontextprotocol/serverInfo'].name" == "rustybin-mock-petstore"
jsonpath "$.result.instructions" contains "Petstore"
# ANCHOR_END: discover

# ANCHOR: list
POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools" count == 6
jsonpath "$.result.tools[*].name" contains "getPetById"
jsonpath "$.result.tools[2].inputSchema.required[0]" == "petId"
jsonpath "$.result.tools[2].annotations.readOnlyHint" == true
jsonpath "$.result.tools[4].annotations.destructiveHint" == true
jsonpath "$.result.tools[2]._meta['rustybin.dev/operation'].path" == "/pets/{petId}"
# ANCHOR_END: list

# ANCHOR: call
POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: getPetById
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
  "name": "getPetById", "arguments": {"petId": 42}, "_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.status" == 200
jsonpath "$.result.structuredContent.body.id" == 42
jsonpath "$.result.content[0].type" == "text"
# ANCHOR_END: call

# ANCHOR: call_error
# The sample's rule answers 404 for pet 0: a tool error with the problem body.
POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: getPetById
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
  "name": "getPetById", "arguments": {"petId": 0}, "_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.structuredContent.status" == 404
jsonpath "$.result.structuredContent.body.message" == "pet not found"
# ANCHOR_END: call_error

# ANCHOR: captured
# The call is in the mock's traffic, marked as an MCP tool call.
GET {{base_url}}/_rustybin/mocks/petstore/requests?limit=1
HTTP 200
[Asserts]
jsonpath "$.requests[0].channel" == "mcp"
jsonpath "$.requests[0].tool" == "getPetById"
jsonpath "$.requests[0].path" == "/v1/pets/0"
jsonpath "$.requests[0].status" == 404
# ANCHOR_END: captured

# ANCHOR: secured
PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": true}, "deploy": true}
HTTP 200

# Without credentials the call is the 401 the REST call gets.
POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: createPet
{"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
  "name": "createPet", "arguments": {"body": {"name": "Rex"}}, "_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.structuredContent.status" == 401

# The MCP request's API key header is passed to the call.
POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: createPet
X-API-Key: demo-key
{"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
  "name": "createPet", "arguments": {"body": {"name": "Rex"}}, "_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.status" == 201
jsonpath "$.result.structuredContent.body.name" == "Rex"
# ANCHOR_END: secured

PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": false}, "deploy": true}
HTTP 200

# ANCHOR: detail
GET {{base_url}}/_rustybin/mocks/petstore
HTTP 200
[Asserts]
jsonpath "$.mcp.url" == "{{base_url}}/mock/petstore/_rustybin/mcp"
jsonpath "$.mcp.serving" == true
jsonpath "$.mcp.tools" count == 6
jsonpath "$.mcp.skipped" count == 0
# ANCHOR_END: detail

# ANCHOR: exclude
PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"mcp": {"exclude": ["deletePet"]}, "deploy": true}
HTTP 200
[Asserts]
jsonpath "$.mcp.tools" count == 5
jsonpath "$.mcp.skipped[0].operation" == "deletePet"
jsonpath "$.mcp.skipped[0].reason" contains "mcp.exclude"
# ANCHOR_END: exclude

# ANCHOR: disable
PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"mcp": {"enabled": false}, "deploy": true}
HTTP 200

POST {{base_url}}/mock/petstore/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {"_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28",
  "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 404
[Asserts]
jsonpath "$.error.message" contains "mcp.enabled"
# ANCHOR_END: disable

PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"mcp": {"enabled": true, "exclude": []}, "deploy": true}
HTTP 200
