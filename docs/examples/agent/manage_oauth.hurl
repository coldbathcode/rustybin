# OAuth sign-in to the management MCP server (docs/src/agent-mcp.md), on the
# secured documentation instance (control auth jwt, with an admin token). An
# MCP client does these steps itself; here the console's part (the consent
# page) is the consent API called with the admin token, and the PKCE pair is
# the one from RFC 7636 appendix B.

# ANCHOR: oauth_challenge
POST {{secured_url}}/_rustybin/mcp
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 401
[Asserts]
header "WWW-Authenticate" contains "resource_metadata=\"{{secured_url}}/.well-known/oauth-protected-resource/_rustybin/mcp\""
# ANCHOR_END: oauth_challenge

# ANCHOR: oauth_metadata
GET {{secured_url}}/.well-known/oauth-protected-resource/_rustybin/mcp
HTTP 200
[Asserts]
jsonpath "$.resource" == "{{secured_url}}/_rustybin/mcp"
jsonpath "$.authorization_servers[0]" == "{{secured_url}}/_rustybin/oauth"
jsonpath "$.scopes_supported" contains "admin"
jsonpath "$.scopes_supported" contains "console"
jsonpath "$.scopes_supported" contains "inspector"

GET {{secured_url}}/.well-known/oauth-authorization-server/_rustybin/oauth
HTTP 200
[Asserts]
jsonpath "$.issuer" == "{{secured_url}}/_rustybin/oauth"
jsonpath "$.authorization_endpoint" == "{{secured_url}}/_rustybin/oauth/authorize"
jsonpath "$.token_endpoint" == "{{secured_url}}/_rustybin/oauth/token"
jsonpath "$.registration_endpoint" == "{{secured_url}}/_rustybin/oauth/register"
jsonpath "$.code_challenge_methods_supported" contains "S256"
jsonpath "$.token_endpoint_auth_methods_supported" contains "none"
# ANCHOR_END: oauth_metadata

# ANCHOR: oauth_register
POST {{secured_url}}/_rustybin/oauth/register
Content-Type: application/json
{"client_name": "Docs agent", "redirect_uris": ["http://127.0.0.1:33418/callback"]}
HTTP 201
[Captures]
client_id: jsonpath "$.client_id"
[Asserts]
jsonpath "$.client_id" startsWith "rbc."
jsonpath "$.token_endpoint_auth_method" == "none"
jsonpath "$.grant_types" contains "refresh_token"

# Plain http only for loopback addresses.
POST {{secured_url}}/_rustybin/oauth/register
Content-Type: application/json
{"client_name": "Somewhere", "redirect_uris": ["http://agent.example/callback"]}
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid_redirect_uri"
# ANCHOR_END: oauth_register

# ANCHOR: oauth_authorize
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: {{client_id}}
redirect_uri: http://127.0.0.1:40123/callback
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
scope: admin console inspector
state: docs-state
resource: {{secured_url}}/_rustybin/mcp
HTTP 302
[Captures]
request_id: header "Location" regex "^/ui/#/authorize/(.+)$"
[Asserts]
header "Location" startsWith "/ui/#/authorize/"
# ANCHOR_END: oauth_authorize

# ANCHOR: oauth_consent
GET {{secured_url}}/_rustybin/oauth/requests/{{request_id}}
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.client_name" == "Docs agent"
jsonpath "$.redirect_kind" == "loopback"
jsonpath "$.scopes_requested" count == 3
jsonpath "$.scopes_grantable" count == 3

POST {{secured_url}}/_rustybin/oauth/requests/{{request_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"decision": "approve", "scope": "console"}
HTTP 200
[Captures]
code: jsonpath "$.redirect_to" regex "[?&]code=([^&]+)"
[Asserts]
jsonpath "$.redirect_to" startsWith "http://127.0.0.1:40123/callback?code="
jsonpath "$.redirect_to" contains "state=docs-state"
# ANCHOR_END: oauth_consent

# ANCHOR: oauth_token
POST {{secured_url}}/_rustybin/oauth/token
[Form]
grant_type: authorization_code
client_id: {{client_id}}
code: {{code}}
code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
redirect_uri: http://127.0.0.1:40123/callback
HTTP 200
[Captures]
access_token: jsonpath "$.access_token"
refresh_token: jsonpath "$.refresh_token"
[Asserts]
header "Cache-Control" == "no-store"
jsonpath "$.token_type" == "Bearer"
jsonpath "$.scope" == "console"
jsonpath "$.expires_in" == 3600

# A code works once.
POST {{secured_url}}/_rustybin/oauth/token
[Form]
grant_type: authorization_code
client_id: {{client_id}}
code: {{code}}
code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid_grant"
# ANCHOR_END: oauth_token

# ANCHOR: oauth_use
POST {{secured_url}}/_rustybin/mcp
Authorization: Bearer {{access_token}}
Accept: application/json, text/event-stream
Content-Type: application/json
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" contains "list_mocks"
jsonpath "$.result.tools[*].name" not contains "create_mock"

# The token opens the management MCP server only.
GET {{secured_url}}/_rustybin/config
Authorization: Bearer {{access_token}}
HTTP 401
# ANCHOR_END: oauth_use

# ANCHOR: oauth_refresh
POST {{secured_url}}/_rustybin/oauth/token
[Form]
grant_type: refresh_token
client_id: {{client_id}}
refresh_token: {{refresh_token}}
HTTP 200
[Asserts]
jsonpath "$.scope" == "console"
jsonpath "$.access_token" exists

# A refresh can narrow the scopes, never widen them.
POST {{secured_url}}/_rustybin/oauth/token
[Form]
grant_type: refresh_token
client_id: {{client_id}}
refresh_token: {{refresh_token}}
scope: admin
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid_scope"
# ANCHOR_END: oauth_refresh

# ANCHOR: oauth_errors
# Without PKCE the request goes back to a loopback client with an error.
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: {{client_id}}
redirect_uri: http://127.0.0.1:40123/callback
state: docs-state
HTTP 302
[Asserts]
header "Location" startsWith "http://127.0.0.1:40123/callback?error=invalid_request"

# An unknown client never gets a redirect: a page explains the error.
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: rbc.not-a-client.x
redirect_uri: http://127.0.0.1:40123/callback
HTTP 400
[Asserts]
header "Content-Type" contains "text/html"
body contains "Unknown client"
# ANCHOR_END: oauth_errors
