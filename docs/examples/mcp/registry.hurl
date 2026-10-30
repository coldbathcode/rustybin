# The mock MCP registry (docs/src/mcp/registry.md): list, page, fetch,
# publish with a token from an auth stub, change a status, validate, the
# scenarios, and browsing a registry through the instance.

# ANCHOR: list
GET {{base_url}}/mcp-registry/v0.1/servers?version=latest&limit=3
HTTP 200
[Asserts]
jsonpath "$.metadata.count" == 3
jsonpath "$.metadata.nextCursor" exists
jsonpath "$.servers[0].server.name" startsWith "io.rustybin/"
jsonpath "$.servers[0]._meta['io.modelcontextprotocol.registry/official'].isLatest" == true
[Captures]
cursor: jsonpath "$.metadata.nextCursor"
# ANCHOR_END: list

# ANCHOR: next_page
GET {{base_url}}/mcp-registry/v0.1/servers?version=latest&limit=3&cursor={{cursor}}
HTTP 200
[Asserts]
jsonpath "$.metadata.count" == 3
# ANCHOR_END: next_page

# ANCHOR: get
GET {{base_url}}/mcp-registry/v0.1/servers/io.rustybin%2Fmock-petstore/versions/latest
HTTP 200
[Asserts]
jsonpath "$.server['$schema']" == "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json"
jsonpath "$.server.remotes[0].type" == "streamable-http"
jsonpath "$.server.remotes[0].url" endsWith "/mock/petstore/_rustybin/mcp"
jsonpath "$._meta['io.modelcontextprotocol.registry/official'].status" == "active"
# ANCHOR_END: get

# ANCHOR: search
GET {{base_url}}/mcp-registry/v0.1/servers?search=ab
HTTP 422
[Asserts]
header "Content-Type" == "application/problem+json"
jsonpath "$.detail" contains "at least 3 characters"
# ANCHOR_END: search

# ANCHOR: token
POST {{base_url}}/mcp-registry/v0.1/auth/dns
Content-Type: application/json
{"domain": "example.com", "timestamp": "2026-10-30T12:00:00Z", "signed_timestamp": "c2lnbmVk"}
HTTP 200
[Asserts]
jsonpath "$.registry_token" startsWith "rbreg_"
jsonpath "$.expires_at" isInteger
[Captures]
token: jsonpath "$.registry_token"
# ANCHOR_END: token

# ANCHOR: publish
POST {{base_url}}/mcp-registry/v0.1/publish
Authorization: Bearer {{token}}
Content-Type: application/json
{
  "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
  "name": "io.example/weather",
  "title": "Weather",
  "description": "Forecasts and alerts for any city",
  "version": "1.0.0",
  "remotes": [{
    "type": "streamable-http",
    "url": "https://weather.example.com/mcp",
    "headers": [{"name": "X-Api-Key", "description": "Your key", "isRequired": true, "isSecret": true}]
  }]
}
HTTP 200
[Asserts]
jsonpath "$.server.name" == "io.example/weather"
jsonpath "$._meta['io.modelcontextprotocol.registry/official'].isLatest" == true
# ANCHOR_END: publish

# ANCHOR: publish_invalid
POST {{base_url}}/mcp-registry/v0.1/publish
Authorization: Bearer {{token}}
Content-Type: application/json
{"name": "weather", "version": "^1.0"}
HTTP 422
[Asserts]
jsonpath "$.errors[*].location" contains "body.name"
jsonpath "$.detail" contains "description"
# ANCHOR_END: publish_invalid

# ANCHOR: deprecate
PATCH {{base_url}}/mcp-registry/v0.1/servers/io.example%2Fweather/versions/1.0.0/status
Authorization: Bearer {{token}}
Content-Type: application/json
{"status": "deprecated", "statusMessage": "Use 2.0.0"}
HTTP 200
[Asserts]
jsonpath "$._meta['io.modelcontextprotocol.registry/official'].status" == "deprecated"
jsonpath "$._meta['io.modelcontextprotocol.registry/official'].statusMessage" == "Use 2.0.0"
# ANCHOR_END: deprecate

# ANCHOR: server_status
PATCH {{base_url}}/mcp-registry/v0.1/servers/io.example%2Fweather/status
Authorization: Bearer {{token}}
Content-Type: application/json
{"status": "active"}
HTTP 200
[Asserts]
jsonpath "$.servers" count == 1
jsonpath "$.servers[0]._meta['io.modelcontextprotocol.registry/official'].status" == "active"
# ANCHOR_END: server_status

# ANCHOR: validate_public
POST {{base_url}}/mcp-registry/v0.1/validate
Content-Type: application/json
{"name": "io.example/x", "description": "One letter is too short a server name", "version": "1.0.0"}
HTTP 200
[Asserts]
jsonpath "$.valid" == false
jsonpath "$.issues[*].reference" contains "invalid-server-name"
# ANCHOR_END: validate_public

# ANCHOR: health
GET {{base_url}}/mcp-registry/v0.1/health
HTTP 200
[Asserts]
jsonpath "$.status" == "ok"

GET {{base_url}}/mcp-registry/v0.1/ping
HTTP 200
[Asserts]
jsonpath "$.pong" == true

GET {{base_url}}/mcp-registry/v0.1/version
HTTP 200
[Asserts]
jsonpath "$.version" startsWith "rustybin-"
# ANCHOR_END: health

# ANCHOR: v0_alias
GET {{base_url}}/mcp-registry/v0/servers?version=latest&limit=1
HTTP 200
[Asserts]
jsonpath "$.servers" count == 1
# ANCHOR_END: v0_alias

# ANCHOR: validate
POST {{base_url}}/_rustybin/registry/validate
Content-Type: application/json
{"name": "io.example/x", "description": "One letter is too short a server name", "version": "1.0.0"}
HTTP 200
[Asserts]
jsonpath "$.valid" == false
jsonpath "$.issues[*].reference" contains "invalid-server-name"
jsonpath "$.issues[*].reference" contains "schema-missing"
# ANCHOR_END: validate

# ANCHOR: state
GET {{base_url}}/_rustybin/registry
HTTP 200
[Asserts]
jsonpath "$.scenario" == "normal"
jsonpath "$.counts.published" == 1
jsonpath "$.entries[?(@.source == 'published')].name" contains "io.example/weather"
jsonpath "$.sync_config.registries[0].api_version" == "v0.1"
# ANCHOR_END: state

# ANCHOR: scenario
PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{"values": {"registry.scenario": "flaky"}}
HTTP 200

GET {{base_url}}/mcp-registry/v0.1/servers?limit=1
HTTP 200

GET {{base_url}}/mcp-registry/v0.1/servers?limit=1
HTTP 200

GET {{base_url}}/mcp-registry/v0.1/servers?limit=1
HTTP 503
[Asserts]
jsonpath "$.detail" contains "flaky"

PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{"values": {"registry.scenario": null}}
HTTP 200
# ANCHOR_END: scenario

# ANCHOR: browse
GET {{base_url}}/_rustybin/registry/browse?url=/mcp-registry&search=weather
HTTP 200
[Asserts]
jsonpath "$.registry" == "/mcp-registry/v0.1"
jsonpath "$.servers[0].name" == "io.example/weather"
jsonpath "$.servers[0].remotes[0].auth.kind" == "api_key"
jsonpath "$.servers[0].remotes[0].auth.name" == "X-Api-Key"
jsonpath "$.servers[0].valid" == true
# ANCHOR_END: browse

# ANCHOR: delete
DELETE {{base_url}}/_rustybin/registry/servers/io.example%2Fweather
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.versions" == 1
# ANCHOR_END: delete
