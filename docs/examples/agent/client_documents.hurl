# Client ID metadata documents at the OAuth sign-in to the management MCP
# server (docs/src/agent-mcp.md), on the secured documentation instance: off
# by default, turned on by the operator with the hosts it may fetch from, the
# consent API naming the document's host, and off again. An agent's document
# lives on the agent's own site; here a mock API of the same instance serves
# one, which is why the example also allows loopback.

# The agent's document, served at {{secured_url}}/mock/docs-agent-site/client.json.
POST {{secured_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-agent-site",
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "An agent's site", "version": "1.0"},
    "paths": {
      "/client.json": {
        "get": {"operationId": "clientDocument", "responses": {"200": {"description": "ok", "content": {"application/json": {
          "example": {
            "client_id": "{{secured_url}}/mock/docs-agent-site/client.json",
            "client_name": "Docs agent",
            "redirect_uris": ["http://127.0.0.1:33418/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_method": "none"
          }}}}}}
      }
    }
  },
  "deploy": true
}
HTTP 201

# ANCHOR: off
GET {{secured_url}}/.well-known/oauth-authorization-server/_rustybin/oauth
HTTP 200
[Asserts]
jsonpath "$.client_id_metadata_document_supported" == false

# A client id that is a URL is no client while the setting is off, and
# nothing is fetched.
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: {{secured_url}}/mock/docs-agent-site/client.json
redirect_uri: http://127.0.0.1:40123/callback
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
HTTP 400
[Asserts]
header "Content-Type" contains "text/html"
body contains "oauth.client_metadata_documents"
# ANCHOR_END: off

# ANCHOR: on
PATCH {{secured_url}}/_rustybin/settings
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "values": {
    "oauth.client_metadata_documents": true,
    "egress.allow": ["127.0.0.1"],
    "egress.allow_loopback": true
  }
}
HTTP 200
[Asserts]
jsonpath "$.changed" contains "oauth.client_metadata_documents"

GET {{secured_url}}/.well-known/oauth-authorization-server/_rustybin/oauth
HTTP 200
[Asserts]
jsonpath "$.client_id_metadata_document_supported" == true
# ANCHOR_END: on

# ANCHOR: authorize
# No registration: the client id is the document's URL. The instance fetches
# it (once; it is kept for five minutes) and checks it before anyone is asked.
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: {{secured_url}}/mock/docs-agent-site/client.json
redirect_uri: http://127.0.0.1:40123/callback
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
scope: console
state: docs-state
HTTP 302
[Captures]
document_request: header "Location" regex "^/ui/#/authorize/(.+)$"

# The consent page is told where the name comes from: the client's own claim.
GET {{secured_url}}/_rustybin/oauth/requests/{{document_request}}
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.client_name" == "Docs agent"
jsonpath "$.client_document.url" == "{{secured_url}}/mock/docs-agent-site/client.json"
jsonpath "$.client_document.host" matches "^127\\.0\\.0\\.1:\\d+$"

POST {{secured_url}}/_rustybin/oauth/requests/{{document_request}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"decision": "approve", "scope": "console"}
HTTP 200
[Captures]
document_code: jsonpath "$.redirect_to" regex "[?&]code=([^&]+)"

# The token is issued to the document's URL as the client.
POST {{secured_url}}/_rustybin/oauth/token
[Form]
grant_type: authorization_code
client_id: {{secured_url}}/mock/docs-agent-site/client.json
code: {{document_code}}
code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
redirect_uri: http://127.0.0.1:40123/callback
HTTP 200
[Asserts]
jsonpath "$.scope" == "console"
jsonpath "$.access_token" exists
# ANCHOR_END: authorize

# ANCHOR: refused
# A host the operator did not name is refused before any call.
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: https://192.0.2.10/client.json
redirect_uri: http://127.0.0.1:40123/callback
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
HTTP 400
[Asserts]
body contains "outbound policy"

# Plain http is for a document on this computer only.
GET {{secured_url}}/_rustybin/oauth/authorize
[Query]
response_type: code
client_id: http://agent.example/client.json
redirect_uri: http://127.0.0.1:40123/callback
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
HTTP 400
[Asserts]
body contains "must be an https URL"
# ANCHOR_END: refused

# Back to how the instance was.
DELETE {{secured_url}}/_rustybin/settings/oauth.client_metadata_documents
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{secured_url}}/_rustybin/settings/egress.allow
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{secured_url}}/_rustybin/settings/egress.allow_loopback
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{secured_url}}/_rustybin/mocks/docs-agent-site
Authorization: Bearer {{admin_token}}
HTTP 200

GET {{secured_url}}/.well-known/oauth-authorization-server/_rustybin/oauth
HTTP 200
[Asserts]
jsonpath "$.client_id_metadata_document_supported" == false
