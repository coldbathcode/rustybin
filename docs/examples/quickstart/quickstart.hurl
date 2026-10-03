# The first requests of the quick start (docs/src/quick-start.md and README.md)

# ANCHOR: echo
GET {{base_url}}/anything/hello?demo=1
X-Demo: rustybin
HTTP 200
[Asserts]
jsonpath "$.path" == "/anything/hello"
jsonpath "$.headers['x-demo'][0]" == "rustybin"
# ANCHOR_END: echo

# ANCHOR: chat
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.choices[0].message.content" startsWith "Hello! I'm Rustybin"
# ANCHOR_END: chat

# ANCHOR: mcp
POST {{base_url}}/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "curl", "version": "1"}}}
HTTP 200
[Asserts]
header "Mcp-Session-Id" exists
jsonpath "$.result.serverInfo.name" == "rustybin-mcp"
# ANCHOR_END: mcp

# ANCHOR: a2a
GET {{base_url}}/.well-known/agent-card.json
HTTP 200
[Asserts]
jsonpath "$.name" == "Echo Agent"
# ANCHOR_END: a2a

# ANCHOR: fault
GET {{base_url}}/echo
X-Rustybin-Fail: 503
HTTP 503
# ANCHOR_END: fault

# ANCHOR: token
POST {{base_url}}/oauth/token
[Form]
grant_type: client_credentials
client_id: rustybin
client_secret: secret
HTTP 200
[Asserts]
jsonpath "$.token_type" == "Bearer"
# ANCHOR_END: token
