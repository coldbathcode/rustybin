# Built-in OAuth 2.0 / OIDC provider: discovery and simple grants (docs/src/reference/oidc.md)

# ANCHOR: discovery
GET {{base_url}}/.well-known/openid-configuration
HTTP 200
[Asserts]
jsonpath "$.issuer" == "{{base_url}}"
jsonpath "$.token_endpoint" == "{{base_url}}/oauth/token"
jsonpath "$.jwks_uri" == "{{base_url}}/oauth/jwks"
jsonpath "$.code_challenge_methods_supported" contains "S256"
jsonpath "$.grant_types_supported" contains "urn:ietf:params:oauth:grant-type:token-exchange"
jsonpath "$.id_token_signing_alg_values_supported" contains "RS256"

GET {{base_url}}/.well-known/oauth-authorization-server
HTTP 200
[Asserts]
jsonpath "$.registration_endpoint" == "{{base_url}}/oauth/register"
# ANCHOR_END: discovery

# ANCHOR: jwks
GET {{base_url}}/oauth/jwks
HTTP 200
[Asserts]
jsonpath "$.keys[0].kty" == "RSA"
jsonpath "$.keys[0].alg" == "RS256"
jsonpath "$.keys[0].kid" == "rustybin-rs256-key"
# ANCHOR_END: jwks

# ANCHOR: client_credentials
POST {{base_url}}/oauth/token
[BasicAuth]
rustybin: secret
[Form]
grant_type: client_credentials
scope: orders:read
resource: https://api.example.com
HTTP 200
[Captures]
cc_token: jsonpath "$.access_token"
[Asserts]
header "Cache-Control" == "no-store"
jsonpath "$.token_type" == "Bearer"
jsonpath "$.expires_in" == 3600
jsonpath "$.scope" == "orders:read"
jsonpath "$.refresh_token" not exists
# ANCHOR_END: client_credentials

# ANCHOR: introspect
POST {{base_url}}/oauth/introspect
[Form]
token: {{cc_token}}
client_id: rustybin
client_secret: secret
HTTP 200
[Asserts]
jsonpath "$.active" == true
jsonpath "$.aud" == "https://api.example.com"
jsonpath "$.client_id" == "rustybin"
jsonpath "$.sub" == "rustybin"
jsonpath "$.iss" == "{{base_url}}"
# ANCHOR_END: introspect

# ANCHOR: password
POST {{base_url}}/oauth/token
[Form]
grant_type: password
client_id: rustybin
client_secret: secret
username: alice
password: alice
HTTP 200
[Captures]
user_token: jsonpath "$.access_token"
[Asserts]
jsonpath "$.scope" == "openid profile email"
jsonpath "$.id_token" exists
jsonpath "$.refresh_token" startsWith "rt_"

GET {{base_url}}/oauth/userinfo
Authorization: Bearer {{user_token}}
HTTP 200
[Asserts]
jsonpath "$.sub" == "alice"
jsonpath "$.name" == "Alice Admin"
jsonpath "$.email" == "alice@rustybin.local"
jsonpath "$.groups" contains "admins"
# ANCHOR_END: password

# ANCHOR: bad_credentials
POST {{base_url}}/oauth/token
[Form]
grant_type: client_credentials
client_id: rustybin
client_secret: wrong
HTTP 401
[Asserts]
jsonpath "$.error" == "invalid_client"
header "WWW-Authenticate" exists
# ANCHOR_END: bad_credentials

# ANCHOR: token_exchange
# RFC 8693: exchange alice's token for one aimed at another API, with an actor.
POST {{base_url}}/oauth/token
[BasicAuth]
rustybin: secret
[Form]
grant_type: urn:ietf:params:oauth:grant-type:token-exchange
subject_token: {{user_token}}
subject_token_type: urn:ietf:params:oauth:token-type:access_token
actor_token: {{cc_token}}
actor_token_type: urn:ietf:params:oauth:token-type:access_token
audience: https://payments.example.com
scope: openid
HTTP 200
[Captures]
exchanged: jsonpath "$.access_token"
[Asserts]
jsonpath "$.issued_token_type" == "urn:ietf:params:oauth:token-type:access_token"
jsonpath "$.scope" == "openid"

POST {{base_url}}/oauth/introspect
[BasicAuth]
rustybin: secret
[Form]
token: {{exchanged}}
HTTP 200
[Asserts]
jsonpath "$.sub" == "alice"
jsonpath "$.aud" == "https://payments.example.com"
jsonpath "$.act.sub" == "rustybin"
# ANCHOR_END: token_exchange

# ANCHOR: revoke
POST {{base_url}}/oauth/revoke
[BasicAuth]
rustybin: secret
[Form]
token: {{cc_token}}
HTTP 200

POST {{base_url}}/oauth/introspect
[BasicAuth]
rustybin: secret
[Form]
token: {{cc_token}}
HTTP 200
[Asserts]
jsonpath "$.active" == false
# ANCHOR_END: revoke
