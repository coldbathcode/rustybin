# The MCP inspector's control-plane API (docs/src/mcp/inspector.md): the
# server list, the relay, the fetch helper for OAuth discovery, check runs
# with a JSON or JUnit report, and saved connections.

# ANCHOR: servers
GET {{base_url}}/_rustybin/mcp-servers
HTTP 200
[Asserts]
jsonpath "$.servers[?(@.path == '/mcp')].kind" contains "mock-mcp"
jsonpath "$.servers[?(@.path == '/mcp/protected')].auth.kind" contains "oauth"
jsonpath "$.servers[?(@.path == '/mcp/sse')].transports[0]" contains "legacy"
jsonpath "$.servers[?(@.path == '/_rustybin/mcp')].kind" contains "manage"
jsonpath "$.servers[?(@.path == '/mock/petstore/_rustybin/mcp')].kind" contains "mock-api"
jsonpath "$.relay.allow_loopback" isBoolean
jsonpath "$.checks[*].id" contains "tools_call"
# ANCHOR_END: servers

# ANCHOR: relay
POST {{base_url}}/_rustybin/mcp-relay
Content-Type: application/json
Accept: text/event-stream
{
  "target": "/mcp",
  "headers": {"MCP-Protocol-Version": "2026-07-28", "Mcp-Method": "tools/list"},
  "message": {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {}}}}
}
HTTP 200
[Asserts]
header "Content-Type" contains "text/event-stream"
body contains "event: relay"
body contains "event: message"
body contains "get_weather"
body contains "event: done"
# ANCHOR_END: relay

# ANCHOR: relay_refused
POST {{base_url}}/_rustybin/mcp-relay
Content-Type: application/json
{"target": "http://10.0.0.1/mcp", "message": {"jsonrpc": "2.0", "id": 1, "method": "ping"}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "egress_refused"
# ANCHOR_END: relay_refused

# ANCHOR: fetch
POST {{base_url}}/_rustybin/mcp-relay/fetch
Content-Type: application/json
{"url": "/.well-known/oauth-protected-resource/mcp/protected"}
HTTP 200
[Asserts]
jsonpath "$.status" == 200
jsonpath "$.json.resource" endsWith "/mcp/protected"
jsonpath "$.json.authorization_servers" count == 1
# ANCHOR_END: fetch

# ANCHOR: checks
POST {{base_url}}/_rustybin/mcp-checks
Content-Type: application/json
{"target": "/mcp", "checks": ["connect", "version", "tools_list", "tools_call", "unknown_tool", "ping"]}
HTTP 200
[Captures]
run_id: jsonpath "$.id"
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.relayed" == false
jsonpath "$.summary.failed" == 0
jsonpath "$.checks[?(@.id == 'tools_list')].status" contains "pass"
jsonpath "$.checks[?(@.id == 'unknown_tool')].status" contains "pass"
jsonpath "$.latency.calls" > 0
# ANCHOR_END: checks

# ANCHOR: junit
GET {{base_url}}/_rustybin/mcp-checks/{{run_id}}?format=junit
HTTP 200
[Asserts]
header "Content-Type" contains "application/xml"
xpath "string(//testsuite/@failures)" == "0"
xpath "count(//testcase)" == 6
# ANCHOR_END: junit

# ANCHOR: connections
PUT {{base_url}}/_rustybin/mcp-connections/apikey-server
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "Mock server behind an API key",
  "target": "/mcp/apikey",
  "transport": "modern",
  "mode": "relay",
  "auth": {"kind": "api_key", "name": "X-API-Key", "location": "header"},
  "secrets": {"api_key": "demo-key-1234"}
}
HTTP 200
[Asserts]
jsonpath "$.secrets.api_key.set" == true
jsonpath "$.secrets.api_key.hint" == "1234"
body not contains "demo-key-1234"

# A check run names the connection: the relay adds the sealed key.
POST {{base_url}}/_rustybin/mcp-checks
Content-Type: application/json
{"target": "/mcp/apikey", "connection": "apikey-server", "checks": ["connect", "tools_list", "auth_refusal"], "save": false}
HTTP 200
[Asserts]
jsonpath "$.ok" == true

DELETE {{base_url}}/_rustybin/mcp-connections/apikey-server
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: connections
