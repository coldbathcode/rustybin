# The console sign-in (docs/src/concepts/control-plane-security.md,
# docs/src/quick-start.md). Runs against the password instance of
# docs/examples/run.sh: nothing set, so RUSTYBIN_CONTROL_AUTH is password and
# the credentials are rustybin / rustybin.

# ANCHOR: locked
GET {{password_url}}/_rustybin/status
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Bearer realm=\"rustybin-control\""
jsonpath "$.reason" == "missing_token"
jsonpath "$.control_auth" == "password"
jsonpath "$.error" contains "/_rustybin/login"

GET {{password_url}}/_rustybin/ready
HTTP 200

GET {{password_url}}/echo
HTTP 200

GET {{password_url}}/ui/
HTTP 200
[Asserts]
body contains "<meta name=\"rustybin-control-auth\" content=\"password\">"
# ANCHOR_END: locked

# ANCHOR: wrong_password
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "letmein" }
HTTP 401
[Asserts]
jsonpath "$.reason" == "invalid_credentials"
# ANCHOR_END: wrong_password

# ANCHOR: login
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
token: jsonpath "$.token"
[Asserts]
jsonpath "$.token_type" == "Bearer"
jsonpath "$.expires_in" == 43200
jsonpath "$.scopes" contains "admin"
jsonpath "$.user" == "rustybin"
jsonpath "$.default_password" == true
# ANCHOR_END: login

# ANCHOR: use_token
GET {{password_url}}/_rustybin/status
Authorization: Bearer {{token}}
HTTP 200
[Asserts]
jsonpath "$.control_auth" == "password"
jsonpath "$.default_password" == true
jsonpath "$.signin.user" == "rustybin"

DELETE {{password_url}}/_rustybin/requests
Authorization: Bearer {{token}}
HTTP 200

GET {{password_url}}/_rustybin/session
Authorization: Bearer {{token}}
HTTP 200
[Asserts]
jsonpath "$.obtained" == "password"
jsonpath "$.user" == "rustybin"
jsonpath "$.scopes" contains "admin"
jsonpath "$.expires_at" exists
# ANCHOR_END: use_token

# ANCHOR: mcp
POST {{password_url}}/_rustybin/mcp
Authorization: Bearer {{token}}
Content-Type: application/json
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2026-07-28
Mcp-Method: tools/list
{
  "jsonrpc": "2.0", "id": 1, "method": "tools/list",
  "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} } }
}
HTTP 200
[Asserts]
jsonpath "$.result.tools[*].name" contains "set_health"
# ANCHOR_END: mcp

# ANCHOR: sessions
GET {{password_url}}/_rustybin/sessions
Authorization: Bearer {{token}}
HTTP 200
[Asserts]
jsonpath "$.count" == 1
jsonpath "$.sessions[0].user" == "rustybin"
jsonpath "$.sessions[0].id" exists
jsonpath "$.current" exists
jsonpath "$.sessions[0].ip" == "127.0.0.1"
# ANCHOR_END: sessions

# ANCHOR: end_session
# A second sign-in (another browser, say) is a second session. Its own
# session list names it as the current one; end it by that id.
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
other_token: jsonpath "$.token"

GET {{password_url}}/_rustybin/sessions
Authorization: Bearer {{other_token}}
HTTP 200
[Captures]
other_id: jsonpath "$.current"
[Asserts]
jsonpath "$.count" == 2

DELETE {{password_url}}/_rustybin/sessions/{{other_id}}
Authorization: Bearer {{token}}
HTTP 200
[Asserts]
jsonpath "$.ended" == true
jsonpath "$.id" == "{{other_id}}"

GET {{password_url}}/_rustybin/status
Authorization: Bearer {{other_token}}
HTTP 401
[Asserts]
jsonpath "$.reason" == "revoked"

GET {{password_url}}/_rustybin/sessions
Authorization: Bearer {{token}}
HTTP 200
[Asserts]
jsonpath "$.count" == 1
# ANCHOR_END: end_session

# ANCHOR: change_password
PUT {{password_url}}/_rustybin/password
Authorization: Bearer {{token}}
Content-Type: application/json
{ "current_password": "rustybin", "new_password": "short" }
HTTP 400
[Asserts]
jsonpath "$.reason" == "invalid_password"

PUT {{password_url}}/_rustybin/password
Authorization: Bearer {{token}}
Content-Type: application/json
{ "current_password": "rustybin", "new_password": "a-longer-secret-k3Qx" }
HTTP 200
[Captures]
fresh: jsonpath "$.token"
[Asserts]
jsonpath "$.changed" == true
jsonpath "$.user" == "rustybin"
jsonpath "$.default_password" == false
jsonpath "$.token" exists

# The old token ended with the change; the fresh one works.
GET {{password_url}}/_rustybin/status
Authorization: Bearer {{token}}
HTTP 401
[Asserts]
jsonpath "$.reason" == "revoked"

GET {{password_url}}/_rustybin/status
Authorization: Bearer {{fresh}}
HTTP 200
[Asserts]
jsonpath "$.default_password" == false

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 401

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "a-longer-secret-k3Qx" }
HTTP 200
[Captures]
second: jsonpath "$.token"
# ANCHOR_END: change_password

# ANCHOR: hash_in_settings
GET {{password_url}}/_rustybin/settings
Authorization: Bearer {{fresh}}
HTTP 200
[Asserts]
jsonpath "$.settings[?(@.key == 'console.password_hash')].secret" == true
jsonpath "$.settings[?(@.key == 'console.password_hash')].value.set" == true
jsonpath "$.settings[?(@.key == 'console.password_hash')].value.source" == "stored"
body not contains "$argon2"
# ANCHOR_END: hash_in_settings

# ANCHOR: logout
POST {{password_url}}/_rustybin/logout
Authorization: Bearer {{second}}
HTTP 200
[Asserts]
jsonpath "$.revoked" == true

GET {{password_url}}/_rustybin/status
Authorization: Bearer {{second}}
HTTP 401
[Asserts]
jsonpath "$.reason" == "revoked"
# ANCHOR_END: logout

# ANCHOR: sign_out_everywhere
DELETE {{password_url}}/_rustybin/sessions
Authorization: Bearer {{fresh}}
HTTP 200
[Asserts]
jsonpath "$.ended" isInteger
jsonpath "$.generation" isInteger

GET {{password_url}}/_rustybin/status
Authorization: Bearer {{fresh}}
HTTP 401
[Asserts]
jsonpath "$.reason" == "revoked"
# ANCHOR_END: sign_out_everywhere

# Back to the default for anyone who runs the examples twice against one
# instance: the password is reset through the settings (admin), which is
# how an operator recovers a forgotten one with the admin token.
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "a-longer-secret-k3Qx" }
HTTP 200
[Captures]
cleanup: jsonpath "$.token"

DELETE {{password_url}}/_rustybin/settings/console.password_hash
Authorization: Bearer {{cleanup}}
HTTP 200

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
