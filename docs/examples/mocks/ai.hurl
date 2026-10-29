# Build mocks with AI (docs/src/mocks/ai.md). Runs against the main
# documentation instance, which has RUSTYBIN_ASSIST_PROVIDER=mock: the
# built-in provider answers deterministically. Creates docs-noex and deletes
# it at the end.

DELETE {{base_url}}/_rustybin/mocks/docs-noex
Authorization: Bearer {{admin_token}}
HTTP *

# ANCHOR: describe
POST {{base_url}}/_rustybin/mocks/ai/spec
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "description": "An orders API for a coffee shop: customers, products, orders with line items and a payment status",
  "openapi_version": "3.1.0",
  "style": { "auth": "api_key", "pagination": "cursor", "errors": "problem" },
  "stream": false
}
HTTP 200
[Asserts]
jsonpath "$.preview.openapi" == "3.1.0"
jsonpath "$.preview.title" == "Order API"
jsonpath "$.preview.operations" count == 12
jsonpath "$.preview.declares_security" == true
jsonpath "$.preview.report.errors" == 0
jsonpath "$.preview.coverage.complete" == true
jsonpath "$.preview.suggested_name" exists
jsonpath "$.usage.provider" == "mock"
jsonpath "$.sent.user" contains "cursor"
# ANCHOR_END: describe

# ANCHOR: describe_stream
POST {{base_url}}/_rustybin/mocks/ai/spec
Authorization: Bearer {{admin_token}}
Content-Type: application/json
Accept: text/event-stream
{ "description": "A notes API: notes with tags", "openapi_version": "3.0.3" }
HTTP 200
[Asserts]
header "Content-Type" startsWith "text/event-stream"
body contains "event: progress"
body contains "event: result"
body contains "event: done"
# ANCHOR_END: describe_stream


# ANCHOR: coverage
POST {{base_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-noex",
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Tickets", "version": "1.0"},
    "paths": {
      "/tickets": {
        "get": {"operationId": "listTickets", "responses": {"200": {"description": "ok", "content": {"application/json": {
          "schema": {"type": "array", "items": {"$ref": "#/components/schemas/Ticket"}}}}}}},
        "post": {"operationId": "createTicket",
          "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Ticket"}}}},
          "responses": {"201": {"description": "created", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Ticket"}}}}}}
      }
    },
    "components": {"schemas": {"Ticket": {"type": "object", "required": ["id", "title", "priority"],
      "properties": {"id": {"type": "integer", "minimum": 1}, "title": {"type": "string"}, "priority": {"type": "string", "enum": ["low", "normal", "high"]}}}}}
  },
  "deploy": true
}
HTTP 201
[Asserts]
jsonpath "$.coverage.operations" == 2
jsonpath "$.coverage.with_examples" == 0
jsonpath "$.coverage.without[*].id" contains "listTickets"
jsonpath "$.report.findings[?(@.class == 'no_examples')].level" == "warning"

GET {{base_url}}/_rustybin/mocks/docs-noex/coverage
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.complete" == false
jsonpath "$.without" count == 2
jsonpath "$.live.without" count == 2
# ANCHOR_END: coverage

# ANCHOR: examples_preview
POST {{base_url}}/_rustybin/mocks/docs-noex/ai/examples
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "per_response": 2, "persona": "realistic support tickets", "stream": false }
HTTP 200
[Asserts]
jsonpath "$.applied" == false
jsonpath "$.source" == "ai"
jsonpath "$.examples.listTickets['200']" count == 2
jsonpath "$.examples.createTicket['201'][0].priority" exists
jsonpath "$.usage.provider" == "mock"
# ANCHOR_END: examples_preview

# ANCHOR: examples_apply
POST {{base_url}}/_rustybin/mocks/docs-noex/ai/examples
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "apply": true, "stream": false }
HTTP 200
[Asserts]
jsonpath "$.applied" == true
jsonpath "$.stored" == 2
jsonpath "$.coverage.complete" == true

GET {{base_url}}/_rustybin/mocks/docs-noex
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.settings.operations.listTickets.examples['200'][0].source" == "ai"
jsonpath "$.settings.operations.listTickets.examples['200'][0].name" == "ai-1"
jsonpath "$.has_draft_changes" == true

POST {{base_url}}/_rustybin/mocks/docs-noex/deploy
Authorization: Bearer {{admin_token}}
HTTP 200

# The stored example answers where the spec had none, for REST and the MCP tool alike.
GET {{base_url}}/mock/docs-noex/tickets
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "example:ai:ai-1"
jsonpath "$[0].priority" exists

POST {{base_url}}/mock/docs-noex/_rustybin/mcp
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: listTickets
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "listTickets", "arguments": {}, "_meta": {
  "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.body[0].priority" exists
# ANCHOR_END: examples_apply

# ANCHOR: generated_values
POST {{base_url}}/_rustybin/mocks/docs-noex/ai/examples
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "source": "generated", "operations": ["createTicket"], "per_response": 3, "apply": true, "stream": false }
HTTP 200
[Asserts]
jsonpath "$.source" == "generated"
jsonpath "$.stored" == 3
# ANCHOR_END: generated_values

# ANCHOR: edit
POST {{base_url}}/_rustybin/mocks/docs-noex/ai/edit
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "instruction": "add a 404 response to every operation", "stream": false }
HTTP 200
[Asserts]
jsonpath "$.applied" == false
jsonpath "$.diff.changed" count == 2
jsonpath "$.diff.removed" count == 0
jsonpath "$.preview.report.errors" == 0
jsonpath "$.before_text" not contains "AI edit"
jsonpath "$.text" contains "\"404\""

POST {{base_url}}/_rustybin/mocks/docs-noex/ai/edit
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "instruction": "add a 404 response to every operation", "apply": true, "stream": false }
HTTP 200
[Asserts]
jsonpath "$.applied" == true
jsonpath "$.detail.draft_changes" contains "spec"
# ANCHOR_END: edit

# ANCHOR: nothing_to_do
POST {{base_url}}/_rustybin/mocks/docs-noex/ai/examples
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "stream": false }
HTTP 409
[Asserts]
jsonpath "$.reason" == "nothing_to_do"
# ANCHOR_END: nothing_to_do

DELETE {{base_url}}/_rustybin/mocks/docs-noex
Authorization: Bearer {{admin_token}}
HTTP 200
