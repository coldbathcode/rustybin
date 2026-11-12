# A secret set at an agent's request (docs/src/agent-mcp.md, "A secret at an
# agent's request"), on the password instance of docs/examples/run.sh
# (rustybin / rustybin). An agent asks for two secrets with request_secret, a
# person reads the requests, fulfils one and declines the other with a note,
# and the agent reads the state of each: never the value. Then a user without
# secrets.write reads and declines the request of their own agent, and is
# refused the fulfilling. Leaves the instance as it found it.

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
admin: jsonpath "$.token"

# The agent signs in on a token of its own.
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
agent: jsonpath "$.token"

# Off until someone turns it on: the agent is told, nobody is asked.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: request_secret
{"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "request_secret", "arguments": {"setting": "notify.webhook_token", "reason": "the nightly schedule posts its failures to the team webhook"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "secret requests are off"

# ANCHOR: enable
# Let agents ask (a setting of the agents group: settings.security).
PATCH {{password_url}}/_rustybin/settings
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "values": { "agents.secret_requests": true } }
HTTP 200
[Asserts]
jsonpath "$.changed" contains "agents.secret_requests"
# ANCHOR_END: enable

# ANCHOR: ask
# The agent asks: the setting's name and a reason. Never a value.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: request_secret
{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "request_secret", "arguments": {"setting": "notify.webhook_token", "reason": "the nightly schedule posts its failures to the team webhook"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Captures]
request: jsonpath "$.result.structuredContent.request.id"
[Asserts]
jsonpath "$.result.isError" == false
jsonpath "$.result.structuredContent.request.state" == "pending"
jsonpath "$.result.structuredContent.request.setting" == "notify.webhook_token"
jsonpath "$.result.structuredContent.request.set" == false
jsonpath "$.result.structuredContent.cli" == "rustybin settings fulfil {{request}}"
# ANCHOR_END: ask

# A second one, to decline below.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: request_secret
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "request_secret", "arguments": {"setting": "git.token", "reason": "the configuration repository"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Captures]
other: jsonpath "$.result.structuredContent.request.id"

# A secret is never a tool input: update_settings still refuses one.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: update_settings
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "update_settings", "arguments": {"values": {"notify.webhook_token": "never-through-an-agent"}}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.isError" == true
jsonpath "$.result.content[0].text" contains "an agent never sets secrets"

# ANCHOR: list
# A person with secrets.write reads what waits: the setting, the agent and the
# user it acts for, and the agent's reason as untrusted_reason (its claim).
GET {{password_url}}/_rustybin/secret-requests?state=pending
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.enabled" == true
jsonpath "$.pending" == 2
jsonpath "$.requests[?(@.id == '{{request}}')].setting.key" == "notify.webhook_token"
jsonpath "$.requests[?(@.id == '{{request}}')].setting.set" == false
jsonpath "$.requests[?(@.id == '{{request}}')].agent.untrusted_name" == "docs-secret-agent"
jsonpath "$.requests[?(@.id == '{{request}}')].agent.owner_label" == "rustybin"
jsonpath "$.requests[?(@.id == '{{request}}')].untrusted_reason" == "the nightly schedule posts its failures to the team webhook"
# ANCHOR_END: list

# ANCHOR: one
GET {{password_url}}/_rustybin/secret-requests/{{request}}
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.state" == "pending"
jsonpath "$.setting.needs" contains "secrets.write"
jsonpath "$.expires_in" > 0
# ANCHOR_END: one

# ANCHOR: fulfil
# The person types the value: it goes to the settings store, as a PATCH of
# /_rustybin/settings would put it there, and the answer never carries it.
POST {{password_url}}/_rustybin/secret-requests/{{request}}/fulfil
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "value": "whk_docs-example-value-k3Qx" }
HTTP 200
[Asserts]
jsonpath "$.state" == "fulfilled"
jsonpath "$.setting.set" == true
jsonpath "$.resolved_by" == "rustybin"
body not contains "whk_docs-example-value"
# ANCHOR_END: fulfil

# A request is answered once.
POST {{password_url}}/_rustybin/secret-requests/{{request}}/fulfil
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "value": "another" }
HTTP 410
[Asserts]
jsonpath "$.state" == "fulfilled"

# ANCHOR: state
# The agent reads how its request stands: fulfilled, and that the setting is
# set. Never the value, and no hint of it.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_secret_request
{"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "get_secret_request", "arguments": {"id": "{{request}}"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.structuredContent.request.state" == "fulfilled"
jsonpath "$.result.structuredContent.request.set" == true
body not contains "whk_docs-example-value"
body not contains "k3Qx"
# ANCHOR_END: state

# ANCHOR: decline
# Declined, with a few words for the agent (optional; never the secret):
# nothing is set.
POST {{password_url}}/_rustybin/secret-requests/{{other}}/decline
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "note": "the repository is read with the deploy key that is already set" }
HTTP 200
[Asserts]
jsonpath "$.state" == "declined"
jsonpath "$.setting.set" == false
jsonpath "$.note" == "the repository is read with the deploy key that is already set"
# ANCHOR_END: decline

# ANCHOR: note
# The agent reads declined, and the note the person left.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: get_secret_request
{"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "get_secret_request", "arguments": {"id": "{{other}}"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.structuredContent.request.state" == "declined"
jsonpath "$.result.structuredContent.request.note" == "the repository is read with the deploy key that is already set"
jsonpath "$.result.content[0].text" contains "deploy key that is already set"
# ANCHOR_END: note

# A user who may change settings but not set secrets (no secrets.write), and
# an agent on a sign-in of that user.
PUT {{password_url}}/_rustybin/roles/docs-tuner
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "description": "Changes settings, sets no secret", "permissions": ["instance.read", "settings.read", "settings.write"] }
HTTP 200

POST {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "name": "docs-tess", "display_name": "Tess Tuner", "roles": ["docs-tuner"] }
HTTP 201
[Captures]
invitation: jsonpath "$.invitation.token"

POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "tess-secret-k3Qx" }
HTTP 200
[Captures]
tess: jsonpath "$.token"

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-tess", "password": "tess-secret-k3Qx" }
HTTP 200
[Captures]
tess_agent: jsonpath "$.token"

POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{tess_agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: request_secret
{"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "request_secret", "arguments": {"setting": "leads.webhook_token", "reason": "the leads webhook of the demo"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-tess-agent", "version": "1.0"}}}}
HTTP 200
[Captures]
hers: jsonpath "$.result.structuredContent.request.id"
[Asserts]
jsonpath "$.result.isError" == false

# The admin's agent asks too: a request that is not hers.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/call
Mcp-Name: request_secret
{"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "request_secret", "arguments": {"setting": "git.token", "reason": "the configuration repository"}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-secret-agent", "version": "1.0"}}}}
HTTP 200
[Captures]
not_hers: jsonpath "$.result.structuredContent.request.id"

# ANCHOR: own
# Without secrets.write, a user reads the requests of their own agents: the
# same fields, and nobody else's (pending counts what they may read).
GET {{password_url}}/_rustybin/secret-requests?state=pending
Authorization: Bearer {{tess}}
HTTP 200
[Asserts]
jsonpath "$.pending" == 1
jsonpath "$.requests" count == 1
jsonpath "$.requests[0].id" == "{{hers}}"
jsonpath "$.requests[0].agent.owner_label" == "docs-tess"
jsonpath "$.requests[0].setting.needs" contains "secrets.write"

# Another user's request does not exist for them.
GET {{password_url}}/_rustybin/secret-requests/{{not_hers}}
Authorization: Bearer {{tess}}
HTTP 404

POST {{password_url}}/_rustybin/secret-requests/{{not_hers}}/decline
Authorization: Bearer {{tess}}
HTTP 404

# Fulfilling keeps what it needs: owning the agent is not enough.
POST {{password_url}}/_rustybin/secret-requests/{{hers}}/fulfil
Authorization: Bearer {{tess}}
Content-Type: application/json
{ "value": "whk_docs-example-value-k3Qx" }
HTTP 403
[Asserts]
jsonpath "$.reason" == "permission_required"
jsonpath "$.permission" == "secrets.write"

# Declining their own agent's request only takes away, so they may.
POST {{password_url}}/_rustybin/secret-requests/{{hers}}/decline
Authorization: Bearer {{tess}}
Content-Type: application/json
{ "note": "not needed: the demo has no leads webhook" }
HTTP 200
[Asserts]
jsonpath "$.state" == "declined"
jsonpath "$.resolved_by" == "docs-tess"
# ANCHOR_END: own

POST {{password_url}}/_rustybin/secret-requests/{{not_hers}}/decline
Authorization: Bearer {{admin}}
HTTP 200

# The audit log names the request, the setting and the agent; never a value
# or the agent's reason.
GET {{password_url}}/_rustybin/audit?action=secret-requests
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.entries[*].event" contains "secret_fulfilled"
jsonpath "$.entries[*].event" contains "secret_declined"
body not contains "whk_docs-example-value"
body not contains "nightly schedule"
body not contains "deploy key that is already set"
body not contains "no leads webhook"

# Back to how it was.
DELETE {{password_url}}/_rustybin/settings/notify.webhook_token
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/settings/agents.secret_requests
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/users/docs-tess
Authorization: Bearer {{admin}}
HTTP *

DELETE {{password_url}}/_rustybin/roles/docs-tuner
Authorization: Bearer {{admin}}
HTTP *

POST {{password_url}}/_rustybin/logout
Authorization: Bearer {{agent}}
HTTP *

POST {{password_url}}/_rustybin/logout
Authorization: Bearer {{admin}}
HTTP *
