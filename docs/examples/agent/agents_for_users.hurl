# Agents act for a user (docs/src/agent-mcp.md, "Agents act for a user"), on the
# password instance of docs/examples/run.sh (rustybin / rustybin). A user with a
# custom role connects an agent on their sign-in token and another through the
# OAuth sign-in; neither does more than the user; the user limits and revokes
# their own; disabling the user revokes both. Leaves the instance as it found it
# (the agents stay listed, revoked, for a user who is gone).

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
admin: jsonpath "$.token"

PUT {{password_url}}/_rustybin/roles/docs-agent-builder
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "description": "Builds mocks", "permissions": ["instance.read", "mocks.read", "mocks.write"] }
HTTP 200

POST {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "name": "docs-dana", "display_name": "Dana Builder", "roles": ["docs-agent-builder"] }
HTTP 201
[Captures]
invitation: jsonpath "$.invitation.token"
dana_id: jsonpath "$.user.id"

POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "dana-secret-k3Qx" }
HTTP 200
[Captures]
dana: jsonpath "$.token"

# His agent gets a sign-in of its own (revoking an agent revokes the tokens it
# used, so Dana keeps his console).
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-dana", "password": "dana-secret-k3Qx" }
HTTP 200
[Captures]
dana_agent: jsonpath "$.token"

# ANCHOR: user_agent
# Dana's agent on his own sign-in token: the tools of his role, never more.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{dana_agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-dana-agent", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" contains "create_mock"
jsonpath "$.result.tools[*].name" contains "list_agents"
jsonpath "$.result.tools[*].name" not contains "apply_config"
jsonpath "$.result.tools[*].name" not contains "list_users"
# ANCHOR_END: user_agent

# ANCHOR: owner
# The admin sees whose agent it is; ?owner= keeps one user's.
GET {{password_url}}/_rustybin/agents?owner=docs-dana
Authorization: Bearer {{admin}}
HTTP 200
[Captures]
agent_id: jsonpath "$.agents[?(@.name == 'docs-dana-agent')].id"
[Asserts]
jsonpath "$.owner" == "{{dana_id}}"
jsonpath "$.agents[?(@.name == 'docs-dana-agent')].credential.kind" == "user"
jsonpath "$.agents[?(@.name == 'docs-dana-agent')].credential.via" == "sign-in"
jsonpath "$.agents[?(@.name == 'docs-dana-agent')].owner.name" == "docs-dana"
jsonpath "$.agents[?(@.name == 'docs-dana-agent')].owner_label" == "docs-dana"

# Dana holds no agents.read: he sees his own agents, nobody else's.
GET {{password_url}}/_rustybin/agents
Authorization: Bearer {{dana}}
HTTP 200
[Asserts]
jsonpath "$.owner" == "{{dana_id}}"
jsonpath "$.agents" count == 1

GET {{password_url}}/_rustybin/agents?owner=rustybin
Authorization: Bearer {{dana}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "permission_required"
jsonpath "$.permission" == "agents.read"
# ANCHOR_END: owner

# ANCHOR: role_ceiling
# A ceiling may name a role: the agent keeps the tools whose permission the
# role grants, within what Dana holds.
PATCH {{password_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{admin}}
Content-Type: application/json
{"rights": {"ceiling": "viewer"}}
HTTP 200
[Asserts]
jsonpath "$.rights.ceiling" == "viewer"
jsonpath "$.rights.ceiling_kind" == "role"

POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{dana_agent}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-dana-agent", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" contains "list_mocks"
jsonpath "$.result.tools[*].name" not contains "create_mock"
# ANCHOR_END: role_ceiling

# ANCHOR: own_limits
# Dana may limit his own agent further, never widen it again.
PATCH {{password_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{dana}}
Content-Type: application/json
{"rights": {"ceiling": "viewer", "deny": ["get_mock_spec"]}}
HTTP 200

PATCH {{password_url}}/_rustybin/agents/{{agent_id}}
Authorization: Bearer {{dana}}
Content-Type: application/json
{"rights": null}
HTTP 403
[Asserts]
jsonpath "$.permission" == "agents.manage"
jsonpath "$.error" contains "not widen it"

# He revokes it and restores it (he revoked it himself).
POST {{password_url}}/_rustybin/agents/{{agent_id}}/revoke
Authorization: Bearer {{dana}}
HTTP 200
[Asserts]
jsonpath "$.revoked" == true

POST {{password_url}}/_rustybin/agents/{{agent_id}}/restore
Authorization: Bearer {{dana}}
HTTP 200
[Asserts]
jsonpath "$.restored" == true

# An agent the admin revoked is the admin's to restore.
POST {{password_url}}/_rustybin/agents/{{agent_id}}/revoke
Authorization: Bearer {{admin}}
HTTP 200

POST {{password_url}}/_rustybin/agents/{{agent_id}}/restore
Authorization: Bearer {{dana}}
HTTP 403
[Asserts]
jsonpath "$.permission" == "agents.manage"

POST {{password_url}}/_rustybin/agents/{{agent_id}}/restore
Authorization: Bearer {{admin}}
HTTP 200
# ANCHOR_END: own_limits

# ANCHOR: consent
# An OAuth sign-in Dana approves: the consent page lists what the agent will
# hold, Dana's permissions within the scope, and the token acts for Dana.
POST {{password_url}}/_rustybin/oauth/register
Content-Type: application/json
{"client_name": "Dana's agent", "redirect_uris": ["http://127.0.0.1:33419/callback"]}
HTTP 201
[Captures]
client_id: jsonpath "$.client_id"

GET {{password_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: {{client_id}}
redirect_uri: http://127.0.0.1:33419/callback
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
scope: admin
resource: {{password_url}}/_rustybin/mcp
HTTP 302
[Captures]
request_id: header "Location" regex "^/ui/#/authorize/(.+)$"

GET {{password_url}}/_rustybin/oauth/requests/{{request_id}}
Authorization: Bearer {{dana}}
HTTP 200
[Asserts]
jsonpath "$.approver.name" == "docs-dana"
jsonpath "$.acts_for_user" == true
jsonpath "$.scopes_grantable" contains "admin"
jsonpath "$.permissions" contains "mocks.write"
jsonpath "$.permissions" not contains "settings.write"
jsonpath "$.grants[0].scope" == "admin"

POST {{password_url}}/_rustybin/oauth/requests/{{request_id}}
Authorization: Bearer {{dana}}
Content-Type: application/json
{"decision": "approve", "scope": "admin"}
HTTP 200
[Captures]
code: jsonpath "$.redirect_to" regex "[?&]code=([^&]+)"

POST {{password_url}}/_rustybin/oauth/token
[Form]
grant_type: authorization_code
client_id: {{client_id}}
code: {{code}}
code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
redirect_uri: http://127.0.0.1:33419/callback
HTTP 200
[Captures]
oauth_token: jsonpath "$.access_token"

# An admin scope, and still only what Dana may do.
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{oauth_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "docs-dana-oauth", "version": "1.0"}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" contains "create_mock"
jsonpath "$.result.tools[*].name" not contains "apply_config"
# ANCHOR_END: consent

# ANCHOR: user_removed
# Disabling Dana revokes every agent acting for him and ends their sessions;
# his OAuth token is refused at once.
PATCH {{password_url}}/_rustybin/users/docs-dana
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "disabled": true }
HTTP 200
[Asserts]
jsonpath "$.status" == "disabled"
jsonpath "$.agents_revoked" == 2

POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{oauth_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 401

GET {{password_url}}/_rustybin/agents?owner={{dana_id}}
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.agents[*].status" not contains "active"
jsonpath "$.agents[*].status" contains "revoked"
# ANCHOR_END: user_removed

DELETE {{password_url}}/_rustybin/users/docs-dana
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/roles/docs-agent-builder
Authorization: Bearer {{admin}}
HTTP 200

POST {{password_url}}/_rustybin/logout
Authorization: Bearer {{admin}}
HTTP 200
