# Single sign-on with OpenID Connect (docs/src/sso.md). Runs against the sso
# instance of docs/examples/run.sh, whose identity provider is the first
# instance's demo one ({{base_url}}/oauth/*: alice is in the groups users and
# admins, bob in users), set up from the environment:
#   RUSTYBIN_SSO_ENABLED=true RUSTYBIN_SSO_NAME="Demo IdP"
#   RUSTYBIN_SSO_ISSUER={{base_url}} RUSTYBIN_SSO_CLIENT_ID=rustybin
#   RUSTYBIN_SSO_CLIENT_SECRET=secret
#   RUSTYBIN_SSO_SCOPES="openid profile email groups"
#   RUSTYBIN_SSO_ROLE_MAP="admins=admin,users=viewer"
# It leaves the instance as it found it.

# ANCHOR: status
# The sign-in screen asks (open, never a secret).
GET {{sso_url}}/_rustybin/sso/status
HTTP 200
[Asserts]
jsonpath "$.enabled" == true
jsonpath "$.ready" == true
jsonpath "$.name" == "Demo IdP"
jsonpath "$.password_sign_in" == true
# ANCHOR_END: status

# ANCHOR: test
# Discovery and the keys, fetched now through the outbound policy (purpose
# sso), and the redirect URI to register at the provider.
POST {{sso_url}}/_rustybin/sso/test
Authorization: Bearer {{sso_admin_token}}
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.redirect_uri" == "{{sso_url}}/_rustybin/sso/callback"
jsonpath "$.discovery.issuer" == "{{base_url}}"
jsonpath "$.discovery.token_endpoint" == "{{base_url}}/oauth/token"
jsonpath "$.keys" count > 0
jsonpath "$.client_secret.set" == true
jsonpath "$.unknown_roles" count == 0
# ANCHOR_END: test

# ANCHOR: role_map
# The role map takes {group, roles} objects too (kept as group=role role).
PATCH {{sso_url}}/_rustybin/settings
Authorization: Bearer {{sso_admin_token}}
Content-Type: application/json
{ "values": { "sso.role_map": [{ "group": "admins", "roles": ["admin"] }, "users=viewer"] } }
HTTP 200
# ANCHOR_END: role_map

# ANCHOR: start
# The console (or the CLI) starts with the S256 challenge of a verifier it
# keeps; the browser goes to the provider.
GET {{sso_url}}/_rustybin/sso/start?return_to=console&challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
HTTP 302
[Captures]
state: header "Location" regex "[?&]state=([A-Za-z0-9_-]+)"
nonce: header "Location" regex "[?&]nonce=([A-Za-z0-9_-]+)"
code_challenge: header "Location" regex "[?&]code_challenge=([A-Za-z0-9_-]+)"
[Asserts]
header "Location" startsWith "{{base_url}}/oauth/authorize?"
header "Location" contains "response_type=code"
header "Location" contains "client_id=rustybin"
header "Location" contains "code_challenge_method=S256"
# ANCHOR_END: start

# ANCHOR: provider
# The person signs in at the provider, which sends the browser to the
# instance's callback with a code.
POST {{base_url}}/oauth/authorize
[FormParams]
client_id: rustybin
redirect_uri: {{sso_url}}/_rustybin/sso/callback
response_type: code
scope: openid profile email groups
state: {{state}}
nonce: {{nonce}}
code_challenge: {{code_challenge}}
code_challenge_method: S256
username: alice
password: alice
action: approve
HTTP 303
[Captures]
callback: header "Location"
[Asserts]
header "Location" startsWith "{{sso_url}}/_rustybin/sso/callback?code="
# ANCHOR_END: provider

# ANCHOR: callback
# The instance exchanges the code, verifies the ID token, maps the groups and
# sends the browser back to the console with a one-time code.
GET {{callback}}
HTTP 302
[Captures]
code: header "Location" regex "#/sso/(rbs_[A-Za-z0-9_-]+)"
[Asserts]
header "Location" startsWith "/ui/#/sso/rbs_"
# ANCHOR_END: callback

# ANCHOR: token
# The console trades the code and its verifier for a sign-in token.
POST {{sso_url}}/_rustybin/sso/token
Content-Type: application/json
{ "code": "{{code}}", "verifier": "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk" }
HTTP 200
[Captures]
alice: jsonpath "$.token"
[Asserts]
jsonpath "$.obtained" == "sso"
jsonpath "$.user" == "alice"
jsonpath "$.roles" includes "admin"
jsonpath "$.roles" includes "viewer"
jsonpath "$.token_type" == "Bearer"
# ANCHOR_END: token

# The code works once.
POST {{sso_url}}/_rustybin/sso/token
Content-Type: application/json
{ "code": "{{code}}", "verifier": "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk" }
HTTP 401
[Asserts]
jsonpath "$.reason" == "invalid_code"

# ANCHOR: session
GET {{sso_url}}/_rustybin/session
Authorization: Bearer {{alice}}
HTTP 200
[Asserts]
jsonpath "$.obtained" == "sso"
jsonpath "$.user" == "alice"
jsonpath "$.permissions" includes "users.manage"
# ANCHOR_END: session

# ANCHOR: user
# alice is a single sign-on user: no password, no invitation.
GET {{sso_url}}/_rustybin/users/alice
Authorization: Bearer {{sso_admin_token}}
HTTP 200
[Captures]
alice_id: jsonpath "$.id"
[Asserts]
jsonpath "$.source" == "sso"
jsonpath "$.sso.issuer" == "{{base_url}}"
jsonpath "$.sso.mapped_roles" includes "admin"

POST {{sso_url}}/_rustybin/users/{{alice_id}}/invite
Authorization: Bearer {{sso_admin_token}}
HTTP 409
[Asserts]
jsonpath "$.reason" == "sso_user"
# ANCHOR_END: user

# ANCHOR: pin
# An admin pins a role on top of the groups' (a PATCH of roles is refused:
# the provider's groups give those).
PATCH {{sso_url}}/_rustybin/users/{{alice_id}}
Authorization: Bearer {{sso_admin_token}}
Content-Type: application/json
{ "pinned_roles": ["tester"] }
HTTP 200
[Asserts]
jsonpath "$.pinned_roles" includes "tester"
jsonpath "$.roles" includes "tester"
jsonpath "$.roles" includes "admin"

PATCH {{sso_url}}/_rustybin/users/{{alice_id}}
Authorization: Bearer {{sso_admin_token}}
Content-Type: application/json
{ "roles": ["viewer"] }
HTTP 409
[Asserts]
jsonpath "$.reason" == "sso_user"
# ANCHOR_END: pin

# ANCHOR: password_off
# Only single sign-on and the admin token: the password sign-in is refused.
PATCH {{sso_url}}/_rustybin/settings
Authorization: Bearer {{sso_admin_token}}
Content-Type: application/json
{ "values": { "sso.password_sign_in": false } }
HTTP 200

POST {{sso_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 403
[Asserts]
jsonpath "$.reason" == "password_sign_in_off"
# ANCHOR_END: password_off

# Back as it was: the password sign-in on, alice deleted, the role map from
# the environment.
PATCH {{sso_url}}/_rustybin/settings
Authorization: Bearer {{sso_admin_token}}
Content-Type: application/json
{ "values": { "sso.password_sign_in": null, "sso.role_map": null } }
HTTP 200

DELETE {{sso_url}}/_rustybin/users/{{alice_id}}
Authorization: Bearer {{sso_admin_token}}
HTTP 200
