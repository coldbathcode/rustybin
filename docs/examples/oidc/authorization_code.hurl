# Authorization code flow with PKCE, refresh and dynamic registration (docs/src/reference/oidc.md)
# PKCE values are the RFC 7636 appendix B example:
#   code_verifier  dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
#   code_challenge E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM (S256)

# ANCHOR: authorize_page
# A browser would open this URL and see the demo login form.
GET {{base_url}}/oauth/authorize
[Query]
response_type: code
client_id: rustybin-public
redirect_uri: http://localhost:3000/callback
scope: openid profile email
state: xyz
nonce: n-0S6
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
HTTP 200
[Asserts]
header "Content-Type" contains "text/html"
body contains "Rustybin OIDC Login"
# ANCHOR_END: authorize_page

# ANCHOR: login
# Submitting the form (what the browser does) redirects back with a code.
POST {{base_url}}/oauth/authorize
[Form]
response_type: code
client_id: rustybin-public
redirect_uri: http://localhost:3000/callback
scope: openid profile email
state: xyz
nonce: n-0S6
code_challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
code_challenge_method: S256
username: demo
password: demo
action: approve
HTTP 303
[Captures]
code: header "Location" urlQueryParam "code"
[Asserts]
header "Location" startsWith "http://localhost:3000/callback?code="
header "Location" urlQueryParam "state" == "xyz"
# ANCHOR_END: login

# ANCHOR: exchange_code
POST {{base_url}}/oauth/token
[Form]
grant_type: authorization_code
client_id: rustybin-public
code: {{code}}
redirect_uri: http://localhost:3000/callback
code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
HTTP 200
[Captures]
access_token: jsonpath "$.access_token"
id_token: jsonpath "$.id_token"
refresh_token: jsonpath "$.refresh_token"
[Asserts]
jsonpath "$.token_type" == "Bearer"
jsonpath "$.scope" == "openid profile email"

# The ID token carries the nonce and is addressed to the client.
GET {{base_url}}/auth/jwt/decode
Authorization: Bearer {{id_token}}
HTTP 200
[Asserts]
jsonpath "$.claims.nonce" == "n-0S6"
jsonpath "$.claims.aud" == "rustybin-public"
jsonpath "$.claims.preferred_username" == "demo"
# ANCHOR_END: exchange_code

# ANCHOR: code_single_use
POST {{base_url}}/oauth/token
[Form]
grant_type: authorization_code
client_id: rustybin-public
code: {{code}}
redirect_uri: http://localhost:3000/callback
code_verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid_grant"
# ANCHOR_END: code_single_use

# ANCHOR: userinfo
GET {{base_url}}/oauth/userinfo
Authorization: Bearer {{access_token}}
HTTP 200
[Asserts]
jsonpath "$.sub" == "demo"
jsonpath "$.email_verified" == true
# ANCHOR_END: userinfo

# ANCHOR: refresh
# Refresh tokens rotate: the old one stops working.
POST {{base_url}}/oauth/token
[Form]
grant_type: refresh_token
client_id: rustybin-public
refresh_token: {{refresh_token}}
scope: openid
HTTP 200
[Asserts]
jsonpath "$.scope" == "openid"
jsonpath "$.refresh_token" != "{{refresh_token}}"

POST {{base_url}}/oauth/token
[Form]
grant_type: refresh_token
client_id: rustybin-public
refresh_token: {{refresh_token}}
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid_grant"
# ANCHOR_END: refresh

# ANCHOR: pkce_required
# The public client must use PKCE: the error goes back to the redirect URI.
GET {{base_url}}/oauth/authorize
[Query]
response_type: code
client_id: rustybin-public
redirect_uri: http://localhost:3000/callback
state: abc
HTTP 303
[Asserts]
header "Location" urlQueryParam "error" == "invalid_request"
header "Location" urlQueryParam "state" == "abc"
# ANCHOR_END: pkce_required

# ANCHOR: register
# RFC 7591 dynamic client registration, as MCP clients do it.
POST {{base_url}}/oauth/register
Content-Type: application/json
{
  "client_name": "docs demo app",
  "redirect_uris": ["http://localhost:3000/callback"],
  "grant_types": ["authorization_code", "refresh_token"],
  "token_endpoint_auth_method": "client_secret_basic"
}
HTTP 201
[Asserts]
jsonpath "$.client_id" startsWith "dyn-"
jsonpath "$.client_secret" exists
jsonpath "$.client_secret_expires_at" exists
jsonpath "$.token_endpoint_auth_method" == "client_secret_basic"
# ANCHOR_END: register

# ANCHOR: register_public
POST {{base_url}}/oauth/register
Content-Type: application/json
{"client_name": "cli", "redirect_uris": ["http://127.0.0.1:8976/cb"], "token_endpoint_auth_method": "none"}
HTTP 201
[Asserts]
jsonpath "$.client_secret" not exists
jsonpath "$.grant_types" contains "authorization_code"
# ANCHOR_END: register_public
