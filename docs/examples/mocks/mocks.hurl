# OpenAPI mocks: the built-in sample, the control API, lifecycle
# (docs/src/mocks/index.md). Mutations carry the admin token of the run.sh
# server; the last entries run against its secured instance.

# ANCHOR: sample_list
GET {{base_url}}/mock/petstore/v1/pets
HTTP 200
[Asserts]
header "Content-Type" == "application/json"
header "X-Rustybin-Mock" == "petstore"
header "X-Rustybin-Mock-Operation" == "listPets"
header "X-Rustybin-Mock-Source" == "example:three"
header "X-Total-Count" == "3"
jsonpath "$" count == 3
jsonpath "$[0].name" == "Rex"
# ANCHOR_END: sample_list

# ANCHOR: sample_get
# No example for this 200: the body is generated from the Pet schema, and
# `id` echoes the path parameter.
GET {{base_url}}/mock/petstore/v1/pets/42
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Operation" == "getPetById"
header "X-Rustybin-Mock-Source" == "generated"
jsonpath "$.id" == 42
jsonpath "$.name" isString
jsonpath "$.status" matches /^(available|pending|sold)$/
# ANCHOR_END: sample_get

# ANCHOR: sample_rule
# The sample ships one rule: petId 0 answers the spec's 404 example.
GET {{base_url}}/mock/petstore/v1/pets/0
HTTP 404
[Asserts]
header "X-Rustybin-Mock-Source" == "rule:unknown pet example:notFound"
jsonpath "$.message" == "pet not found"
# ANCHOR_END: sample_rule

# ANCHOR: sample_prefer
GET {{base_url}}/mock/petstore/v1/pets/1
Prefer: code=404
HTTP 404
[Asserts]
header "X-Rustybin-Mock-Source" == "prefer example:notFound"
jsonpath "$.code" == 404
# ANCHOR_END: sample_prefer

# ANCHOR: sample_create
POST {{base_url}}/mock/petstore/v1/pets
Content-Type: application/json
{"name": "Rex", "tag": "dog"}
HTTP 201
[Asserts]
header "Location" == "/v1/pets/4"
header "X-Rustybin-Mock-Validation" not exists
jsonpath "$.id" == 4
# ANCHOR_END: sample_create

# ANCHOR: sample_unmatched
GET {{base_url}}/mock/petstore/v1/owners
HTTP 404
[Asserts]
header "Content-Type" == "application/problem+json"
jsonpath "$.detail" == "no operation of this mock matches GET /v1/owners"
jsonpath "$.base_path" == "/mock/petstore/v1"

PATCH {{base_url}}/mock/petstore/v1/pets
HTTP 405
[Asserts]
header "Allow" == "GET, POST"

GET {{base_url}}/mock/no-such-mock/anything
HTTP 404
[Asserts]
jsonpath "$.error" == "mock not found"
# ANCHOR_END: sample_unmatched

# ANCHOR: served_spec
GET {{base_url}}/mock/petstore/_rustybin/openapi.json
HTTP 200
[Asserts]
jsonpath "$.info.title" == "Petstore"
jsonpath "$.servers" count == 1
jsonpath "$.servers[0].url" == "{{base_url}}/mock/petstore/v1"

GET {{base_url}}/mock/petstore/_rustybin/openapi.yaml
HTTP 200
[Asserts]
header "Content-Type" == "application/yaml"
body contains "url: {{base_url}}/mock/petstore/v1"
# ANCHOR_END: served_spec

# ANCHOR: list
GET {{base_url}}/_rustybin/mocks
HTTP 200
[Asserts]
jsonpath "$.mocks[*].name" contains "petstore"
jsonpath "$.limits.max_mocks" == 100
jsonpath "$.load_errors" isCollection

# With the admin token the details include the demo credentials (without it
# they are "<redacted>").
GET {{base_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.state" == "live"
jsonpath "$.origin" == "sample"
jsonpath "$.url" == "{{base_url}}/mock/petstore/v1"
jsonpath "$.spec.server_prefix" == "/v1"
jsonpath "$.operations[*].id" contains "getPetById"
jsonpath "$.credentials.api_key.keys[0]" == "demo-key"
# ANCHOR_END: list

# ANCHOR: import
# A preview: nothing is created. The body may also be the spec itself
# (JSON or YAML text).
POST {{base_url}}/_rustybin/mocks/import
Content-Type: application/json
{
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Orders API", "version": "1.0"},
    "servers": [{"url": "https://orders.example.com/api"}],
    "paths": {
      "/orders/{orderId}": {
        "get": {
          "operationId": "getOrder",
          "parameters": [{"name": "orderId", "in": "path", "required": true, "schema": {"type": "integer"}}],
          "responses": {"200": {"description": "An order"}}
        }
      }
    }
  }
}
HTTP 200
[Asserts]
jsonpath "$.title" == "Orders API"
jsonpath "$.suggested_name" == "orders-api"
jsonpath "$.server_prefix" == "/api"
jsonpath "$.operations[0].id" == "getOrder"
jsonpath "$.operations[0].method" == "GET"
jsonpath "$.declares_security" == false
jsonpath "$.report.ok" == true
# ANCHOR_END: import

# Start clean when an earlier run stopped half way.
DELETE {{base_url}}/_rustybin/mocks/docs-orders
Authorization: Bearer {{admin_token}}
HTTP *

# ANCHOR: create
POST {{base_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-orders",
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Orders API", "version": "1.0"},
    "servers": [{"url": "https://orders.example.com/api"}],
    "paths": {
      "/orders": {
        "get": {
          "operationId": "listOrders",
          "responses": {"200": {"description": "Orders", "content": {"application/json": {
            "schema": {"type": "array", "maxItems": 2, "items": {"$ref": "#/components/schemas/Order"}}}}}}
        }
      },
      "/orders/{orderId}": {
        "get": {
          "operationId": "getOrder",
          "parameters": [{"name": "orderId", "in": "path", "required": true, "schema": {"type": "integer"}}],
          "responses": {"200": {"description": "An order", "content": {"application/json": {
            "schema": {"$ref": "#/components/schemas/Order"}}}}}
        }
      }
    },
    "components": {"schemas": {"Order": {
      "type": "object", "required": ["orderId", "total"],
      "properties": {
        "orderId": {"type": "integer"},
        "total": {"type": "number", "minimum": 0},
        "currency": {"type": "string", "enum": ["EUR", "USD"]}
      }
    }}}
  }
}
HTTP 201
[Asserts]
jsonpath "$.name" == "docs-orders"
jsonpath "$.state" == "live"
jsonpath "$.origin" == "api"
jsonpath "$.url" == "{{base_url}}/mock/docs-orders/api"
jsonpath "$.report.ok" == true
jsonpath "$.operations" count == 2
# ANCHOR_END: create

# ANCHOR: call_created
GET {{base_url}}/mock/docs-orders/api/orders/7
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Operation" == "getOrder"
jsonpath "$.orderId" == 7
jsonpath "$.total" isNumber
# ANCHOR_END: call_created

# ANCHOR: traffic
GET {{base_url}}/_rustybin/mocks/docs-orders/requests
HTTP 200
[Asserts]
jsonpath "$.total" == 1
jsonpath "$.requests[0].method" == "GET"
jsonpath "$.requests[0].path" == "/api/orders/7"
jsonpath "$.requests[0].outcome" == "mocked"
jsonpath "$.requests[0].operation" == "getOrder"
jsonpath "$.requests[0].status" == 200
jsonpath "$.requests[0].validation" isEmpty
jsonpath "$.requests[0].response_body" contains "\"orderId\": 7"
[Captures]
seq: jsonpath "$.requests[0].seq"

GET {{base_url}}/_rustybin/mocks/docs-orders/requests/{{seq}}
HTTP 200
[Asserts]
jsonpath "$.full_path" == "/mock/docs-orders/api/orders/7"
# ANCHOR_END: traffic

# ANCHOR: draft
# Edits go to the draft: the live revision keeps answering until a deploy.
PATCH {{base_url}}/_rustybin/mocks/docs-orders
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"strip_server_prefix": true}
HTTP 200
[Asserts]
jsonpath "$.state" == "live"
jsonpath "$.has_draft_changes" == true
jsonpath "$.revisions[0].draft" == true

GET {{base_url}}/mock/docs-orders/api/orders/7
HTTP 200

POST {{base_url}}/_rustybin/mocks/docs-orders/deploy
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.deployed" == true
jsonpath "$.report.ok" == true
jsonpath "$.mock.has_draft_changes" == false

# The server prefix (/api) is gone from the mock URL.
GET {{base_url}}/mock/docs-orders/orders/7
HTTP 200
[Asserts]
jsonpath "$.orderId" == 7
# ANCHOR_END: draft

# ANCHOR: rollback
POST {{base_url}}/_rustybin/mocks/docs-orders/rollback
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.rolled_back" == true

GET {{base_url}}/mock/docs-orders/api/orders/7
HTTP 200
# ANCHOR_END: rollback

# ANCHOR: switch
POST {{base_url}}/_rustybin/mocks/docs-orders/disable
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.state" == "off"

GET {{base_url}}/mock/docs-orders/api/orders/7
HTTP 503
[Asserts]
header "Content-Type" == "application/problem+json"
jsonpath "$.detail" == "this mock is switched off"
jsonpath "$.mock" == "docs-orders"

POST {{base_url}}/_rustybin/mocks/docs-orders/enable
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.state" == "live"

GET {{base_url}}/mock/docs-orders/api/orders/7
HTTP 200
# ANCHOR_END: switch

# ANCHOR: admin_required
# The run.sh server has an admin token: mutations without it are refused.
POST {{base_url}}/_rustybin/mocks/docs-orders/disable
HTTP 401
# ANCHOR_END: admin_required

# ANCHOR: delete
DELETE {{base_url}}/_rustybin/mocks/docs-orders
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.deleted" == "docs-orders"

GET {{base_url}}/mock/docs-orders/api/orders/7
HTTP 404
# ANCHOR_END: delete

# ANCHOR: secured_console
# Secured instance (control auth jwt): a console token reads mocks...
GET {{secured_url}}/_rustybin/mocks
Authorization: Bearer {{console_token}}
HTTP 200
[Asserts]
jsonpath "$.mocks[*].name" contains "petstore"

# ...but cannot change them (every POST, PUT, PATCH and DELETE needs admin).
POST {{secured_url}}/_rustybin/mocks/petstore/disable
Authorization: Bearer {{console_token}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "insufficient_scope"

# The mocks themselves are data plane: no token.
GET {{secured_url}}/mock/petstore/v1/pets
HTTP 200
# ANCHOR_END: secured_console
