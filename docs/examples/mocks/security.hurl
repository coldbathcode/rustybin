# OpenAPI mocks: mocked security (docs/src/mocks/security.md). Switches
# security on for the built-in sample (and back off), then creates the mock
# docs-secure and deletes it at the end.

# ANCHOR: petstore_on
PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": true}, "deploy": true}
HTTP 200
[Asserts]
jsonpath "$.settings.security.enabled" == true
jsonpath "$.settings.security.mode" == "spec"
jsonpath "$.credentials.api_key.keys" contains "demo-key"
jsonpath "$.credentials.bearer.tokens" contains "demo-token"
jsonpath "$.credentials.token_endpoint" == "/oauth/token"
# ANCHOR_END: petstore_on

# ANCHOR: credentials_hidden
# Without admin rights (no admin token here) the secrets are placeholders.
GET {{base_url}}/_rustybin/mocks/petstore
HTTP 200
[Asserts]
jsonpath "$.secrets_hidden" == true
jsonpath "$.credentials.api_key.keys[0]" == "<redacted>"
jsonpath "$.settings.security.basic.users.demo" == "<redacted>"

GET {{base_url}}/_rustybin/mocks/petstore/operations/createPet
HTTP 200
[Asserts]
jsonpath "$.sample_request.headers[*][1]" contains "<api-key>"
# ANCHOR_END: credentials_hidden

# ANCHOR: petstore_401
# createPet declares `apiKey` OR `bearer`.
POST {{base_url}}/mock/petstore/v1/pets
Content-Type: application/json
{"name": "Rex"}
HTTP 401
[Asserts]
header "Content-Type" == "application/problem+json"
header "WWW-Authenticate" == "Bearer realm=\"mock\""
jsonpath "$.detail" == "missing API key (header X-API-Key); missing bearer token"
jsonpath "$.operation" == "createPet"

# listPets declares no security: still open.
GET {{base_url}}/mock/petstore/v1/pets
HTTP 200
# ANCHOR_END: petstore_401

# ANCHOR: petstore_credentials
POST {{base_url}}/mock/petstore/v1/pets
X-API-Key: demo-key
Content-Type: application/json
{"name": "Rex"}
HTTP 201

POST {{base_url}}/mock/petstore/v1/pets
Authorization: Bearer demo-token
Content-Type: application/json
{"name": "Rex"}
HTTP 201

POST {{base_url}}/mock/petstore/v1/pets
X-API-Key: wrong-key
Content-Type: application/json
{"name": "Rex"}
HTTP 401
[Asserts]
jsonpath "$.detail" contains "invalid API key"
# ANCHOR_END: petstore_credentials

# ANCHOR: petstore_capture
GET {{base_url}}/_rustybin/mocks/petstore/requests?limit=1
HTTP 200
[Asserts]
jsonpath "$.requests[0].outcome" == "unauthorized"
jsonpath "$.requests[0].auth" startsWith "failed: invalid API key"
# ANCHOR_END: petstore_capture

# ANCHOR: petstore_off
PATCH {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": false}, "deploy": true}
HTTP 200

POST {{base_url}}/mock/petstore/v1/pets
Content-Type: application/json
{"name": "Rex"}
HTTP 201
# ANCHOR_END: petstore_off

DELETE {{base_url}}/_rustybin/mocks/docs-secure
Authorization: Bearer {{admin_token}}
HTTP *

# ANCHOR: create_secure
POST {{base_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-secure",
  "security": {"enabled": true},
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Orders", "version": "1.0"},
    "security": [{"oauth": ["orders:read"]}],
    "paths": {
      "/health": {"get": {"operationId": "health", "security": [],
        "responses": {"200": {"description": "Up", "content": {"application/json": {"example": {"status": "up"}}}}}}},
      "/orders": {
        "get": {"operationId": "listOrders",
          "responses": {"200": {"description": "Orders", "content": {"application/json": {"example": [{"orderId": 1}]}}}}},
        "post": {"operationId": "createOrder", "security": [{"oauth": ["orders:write"]}],
          "responses": {"201": {"description": "Created", "content": {"application/json": {"example": {"orderId": 2}}}}}}
      },
      "/orders/{orderId}": {"get": {"operationId": "getOrder",
        "security": [{"apiKey": []}, {"oauth": ["orders:read"]}],
        "parameters": [{"name": "orderId", "in": "path", "required": true, "schema": {"type": "integer"}}],
        "responses": {"200": {"description": "An order", "content": {"application/json": {"example": {"orderId": 1}}}}}}},
      "/reports": {"get": {"operationId": "getReport", "security": [{"clientCert": []}],
        "responses": {"200": {"description": "A report", "content": {"application/json": {"example": {"rows": 3}}}}}}}
    },
    "components": {"securitySchemes": {
      "apiKey": {"type": "apiKey", "in": "query", "name": "api_key"},
      "oauth": {"type": "oauth2", "flows": {"clientCredentials": {
        "tokenUrl": "https://idp.example.com/oauth/token",
        "scopes": {"orders:read": "Read orders", "orders:write": "Create orders"}}}},
      "clientCert": {"type": "mutualTLS"}
    }}
  }
}
HTTP 201
[Asserts]
jsonpath "$.security.enabled" == true
jsonpath "$.credentials.schemes[*].type" contains "oauth2"
# ANCHOR_END: create_secure

# ANCHOR: served_flows
GET {{base_url}}/mock/docs-secure/_rustybin/openapi.json
HTTP 200
[Asserts]
jsonpath "$.components.securitySchemes.oauth.flows.clientCredentials.tokenUrl" == "{{base_url}}/oauth/token"
# ANCHOR_END: served_flows

# ANCHOR: oauth_flow
GET {{base_url}}/mock/docs-secure/orders
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Bearer realm=\"mock\""

POST {{base_url}}/oauth/token
[BasicAuth]
rustybin: secret
[Form]
grant_type: client_credentials
scope: orders:read
HTTP 200
[Captures]
read_token: jsonpath "$.access_token"

GET {{base_url}}/mock/docs-secure/orders
Authorization: Bearer {{read_token}}
HTTP 200

# createOrder needs orders:write.
POST {{base_url}}/mock/docs-secure/orders
Authorization: Bearer {{read_token}}
HTTP 403
[Asserts]
jsonpath "$.detail" == "insufficient scope (needs orders:write)"

POST {{base_url}}/oauth/token
[BasicAuth]
rustybin: secret
[Form]
grant_type: client_credentials
scope: orders:read orders:write
HTTP 200
[Captures]
write_token: jsonpath "$.access_token"

POST {{base_url}}/mock/docs-secure/orders
Authorization: Bearer {{write_token}}
HTTP 201
# ANCHOR_END: oauth_flow

# ANCHOR: alternatives
# getOrder: an API key in the query OR a token with orders:read.
GET {{base_url}}/mock/docs-secure/orders/7?api_key=demo-key
HTTP 200

GET {{base_url}}/mock/docs-secure/orders/7
HTTP 401
[Asserts]
jsonpath "$.detail" == "missing API key (query api_key); missing bearer token"

# health has `security: []` in the spec.
GET {{base_url}}/mock/docs-secure/health
HTTP 200
# ANCHOR_END: alternatives

# ANCHOR: mutual_tls
# run.sh sets RUSTYBIN_MTLS_IN_HEADER=X-Client-Cert, as a gateway that
# terminates mTLS would forward the certificate.
GET {{base_url}}/auth/mtls/get-client-cert
HTTP 200
[Captures]
client_cert: jsonpath "$.cert_pem" urlEncode

GET {{base_url}}/mock/docs-secure/reports
HTTP 401
[Asserts]
jsonpath "$.detail" == "no client certificate from the demo CA"

GET {{base_url}}/mock/docs-secure/reports
X-Client-Cert: {{client_cert}}
HTTP 200
# ANCHOR_END: mutual_tls

# ANCHOR: public_operation
PUT {{base_url}}/_rustybin/mocks/docs-secure/operations/listOrders
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"public": true}
HTTP 200

POST {{base_url}}/_rustybin/mocks/docs-secure/deploy
Authorization: Bearer {{admin_token}}
HTTP 200

GET {{base_url}}/mock/docs-secure/orders
HTTP 200
# ANCHOR_END: public_operation

# ANCHOR: fixed_basic
# A fixed mode ignores the spec's requirements: every operation that is not
# public needs these credentials.
PATCH {{base_url}}/_rustybin/mocks/docs-secure
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": true, "mode": "basic", "basic": {"users": {"alice": "wonderland"}}}, "deploy": true}
HTTP 200

GET {{base_url}}/mock/docs-secure/health
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Basic realm=\"mock\""

GET {{base_url}}/mock/docs-secure/health
[BasicAuth]
alice: wonderland
HTTP 200

GET {{base_url}}/mock/docs-secure/orders
HTTP 200
# ANCHOR_END: fixed_basic

# ANCHOR: fixed_api_key
PATCH {{base_url}}/_rustybin/mocks/docs-secure
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": true, "mode": "api_key", "api_key": {"header": "apikey", "keys": ["k-123", "k-456"]}}, "deploy": true}
HTTP 200

GET {{base_url}}/mock/docs-secure/orders/7
apikey: k-456
HTTP 200
# ANCHOR_END: fixed_api_key

# ANCHOR: fixed_jwt
# jwt: only tokens signed by the built-in IdP (static bearer tokens are refused),
# here also with a required audience.
PATCH {{base_url}}/_rustybin/mocks/docs-secure
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"security": {"enabled": true, "mode": "jwt", "jwt": {"audience": "rustybin"}}, "deploy": true}
HTTP 200

GET {{base_url}}/mock/docs-secure/orders/7
Authorization: Bearer demo-token
HTTP 401
[Asserts]
jsonpath "$.detail" startsWith "invalid token"

GET {{base_url}}/mock/docs-secure/orders/7
Authorization: Bearer {{read_token}}
HTTP 200
# ANCHOR_END: fixed_jwt

DELETE {{base_url}}/_rustybin/mocks/docs-secure
Authorization: Bearer {{admin_token}}
HTTP 200
