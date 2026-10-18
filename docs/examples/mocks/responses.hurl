# OpenAPI mocks: choosing responses, modes, rules, validation, deploy checks
# (docs/src/mocks/responses.md). Creates the mock docs-shop and deletes it at
# the end.

DELETE {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
HTTP *

# ANCHOR: create_shop
POST {{base_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-shop",
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Shop", "version": "1.0"},
    "paths": {
      "/products/{productId}": {
        "get": {
          "operationId": "getProduct",
          "parameters": [
            {"name": "productId", "in": "path", "required": true, "schema": {"type": "integer", "minimum": 1}},
            {"name": "currency", "in": "query", "schema": {"type": "string", "enum": ["EUR", "USD"]}}
          ],
          "responses": {
            "200": {"description": "A product", "content": {"application/json": {
              "schema": {"$ref": "#/components/schemas/Product"},
              "examples": {
                "mug": {"value": {"productId": 1, "name": "Mug", "price": 9.5}},
                "lamp": {"value": {"productId": 2, "name": "Lamp", "price": 49}}
              }}}},
            "404": {"description": "Unknown product", "content": {"application/json": {
              "schema": {"$ref": "#/components/schemas/Error"},
              "examples": {"gone": {"value": {"message": "product discontinued"}}}}}}
          }
        }
      },
      "/carts": {
        "post": {
          "operationId": "createCart",
          "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Cart"}}}},
          "responses": {
            "201": {"description": "Created", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Cart"}}}},
            "409": {"description": "Coupon refused", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}
          }
        }
      }
    },
    "components": {"schemas": {
      "Product": {"type": "object", "required": ["productId", "name", "price"], "properties": {
        "productId": {"type": "integer"}, "name": {"type": "string"}, "price": {"type": "number", "minimum": 0}}},
      "Cart": {"type": "object", "required": ["items"], "properties": {
        "cartId": {"type": "string", "format": "uuid", "readOnly": true},
        "items": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["productId", "quantity"],
          "properties": {"productId": {"type": "integer"}, "quantity": {"type": "integer", "minimum": 1}}}},
        "coupon": {"type": "string"}}},
      "Error": {"type": "object", "required": ["message"], "properties": {"message": {"type": "string"}}}
    }}
  }
}
HTTP 201
[Asserts]
jsonpath "$.url" == "{{base_url}}/mock/docs-shop"
jsonpath "$.settings.responses.mode" == "examples"
# ANCHOR_END: create_shop

# ANCHOR: default_example
# Mode "examples": the first named example of the lowest documented 2xx.
GET {{base_url}}/mock/docs-shop/products/5
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "example:mug"
jsonpath "$.name" == "Mug"
# ANCHOR_END: default_example

# ANCHOR: prefer
GET {{base_url}}/mock/docs-shop/products/5
Prefer: example=lamp
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "prefer example:lamp"
jsonpath "$.name" == "Lamp"

GET {{base_url}}/mock/docs-shop/products/5
Prefer: code=404
HTTP 404
[Asserts]
jsonpath "$.message" == "product discontinued"

# The same with headers, for clients that cannot set Prefer.
GET {{base_url}}/mock/docs-shop/products/5
X-Rustybin-Mock-Status: 404
X-Rustybin-Mock-Example: gone
HTTP 404
[Asserts]
header "X-Rustybin-Mock-Source" == "prefer example:gone"

# dynamic=true generates from the schema; productId echoes the path.
GET {{base_url}}/mock/docs-shop/products/5
Prefer: dynamic=true
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "generated"
jsonpath "$.productId" == 5
jsonpath "$.price" >= 0
# ANCHOR_END: prefer

# ANCHOR: undocumented
GET {{base_url}}/mock/docs-shop/products/5
Prefer: code=418
HTTP 418
[Asserts]
header "Content-Type" == "application/problem+json"
header "X-Rustybin-Mock-Source" == "prefer undocumented"
jsonpath "$.status" == 418
# ANCHOR_END: undocumented

# ANCHOR: mode_generated
# PATCH replaces whole sections: unspecified response settings take their
# defaults. "deploy": true publishes the draft in the same call.
PATCH {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"responses": {"mode": "generated", "seed": 7}, "deploy": true}
HTTP 200
[Asserts]
jsonpath "$.settings.responses.mode" == "generated"
jsonpath "$.settings.responses.validate_requests" == "warn"
jsonpath "$.has_draft_changes" == false

GET {{base_url}}/mock/docs-shop/products/5
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "generated"
jsonpath "$.productId" == 5
[Captures]
first_name: jsonpath "$.name"

# Same seed, same request, same body.
GET {{base_url}}/mock/docs-shop/products/5
HTTP 200
[Asserts]
jsonpath "$.name" == "{{first_name}}"
# ANCHOR_END: mode_generated

# ANCHOR: mode_static
PATCH {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"responses": {"mode": "static"}, "deploy": true}
HTTP 200

GET {{base_url}}/mock/docs-shop/products/5
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "empty"
bytes count == 0

# Back to the defaults (mode examples).
PATCH {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"responses": {}, "deploy": true}
HTTP 200
[Asserts]
jsonpath "$.settings.responses.mode" == "examples"
# ANCHOR_END: mode_static

# ANCHOR: operation_settings
PUT {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "rules": [
    {
      "name": "discontinued",
      "when": {"path.productId": {"equals": 13}},
      "respond": {"status": 404, "example": "gone"}
    },
    {
      "name": "gold customers",
      "when": {"header.x-tier": {"equals": "gold"}, "query.currency": {"exists": true}},
      "respond": {"status": 200, "body": {"productId": 7, "name": "Gold lamp", "price": 0}, "headers": {"X-Discount": "100%"}}
    }
  ],
  "respond": {"example": "lamp"},
  "delay_ms": 100
}
HTTP 200
[Asserts]
jsonpath "$.settings.rules" count == 2
jsonpath "$.report.status" == "ok"

POST {{base_url}}/_rustybin/mocks/docs-shop/deploy
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: operation_settings

# ANCHOR: rules
GET {{base_url}}/mock/docs-shop/products/13
HTTP 404
[Asserts]
header "X-Rustybin-Mock-Source" == "rule:discontinued example:gone"
jsonpath "$.message" == "product discontinued"
duration >= 100

GET {{base_url}}/mock/docs-shop/products/5?currency=EUR
X-Tier: gold
HTTP 200
[Asserts]
header "X-Discount" == "100%"
header "X-Rustybin-Mock-Source" == "rule:gold customers static"
jsonpath "$.name" == "Gold lamp"

# No rule matches: the operation's respond override.
GET {{base_url}}/mock/docs-shop/products/5
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "override example:lamp"
jsonpath "$.name" == "Lamp"

# Prefer wins over rules and overrides.
GET {{base_url}}/mock/docs-shop/products/13
Prefer: code=200
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Source" == "prefer example:mug"
# ANCHOR_END: rules

# ANCHOR: body_rule
PUT {{base_url}}/_rustybin/mocks/docs-shop/operations/createCart
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "rules": [
    {
      "when": {"body.coupon": {"matches": "^FREE"}},
      "respond": {"status": 409, "body": {"message": "coupon refused"}}
    },
    {
      "when": {"body.items.0.quantity": {"not_equals": 1}, "header.x-dry-run": {"exists": true}},
      "respond": {"status": 201, "headers": {"X-Dry-Run": "true"}}
    }
  ]
}
HTTP 200

POST {{base_url}}/_rustybin/mocks/docs-shop/deploy
Authorization: Bearer {{admin_token}}
HTTP 200

POST {{base_url}}/mock/docs-shop/carts
Content-Type: application/json
{"items": [{"productId": 1, "quantity": 2}], "coupon": "FREE-LUNCH"}
HTTP 409
[Asserts]
header "X-Rustybin-Mock-Source" == "rule:1 static"
jsonpath "$.message" == "coupon refused"

POST {{base_url}}/mock/docs-shop/carts
Content-Type: application/json
X-Dry-Run: yes
{"items": [{"productId": 1, "quantity": 2}]}
HTTP 201
[Asserts]
header "X-Dry-Run" == "true"
header "X-Rustybin-Mock-Source" == "rule:2 generated"
jsonpath "$.cartId" isUuid
jsonpath "$.items" count >= 1
# ANCHOR_END: body_rule

# ANCHOR: operation
GET {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct
HTTP 200
[Asserts]
jsonpath "$.operation.method" == "GET"
jsonpath "$.operation.path" == "/products/{productId}"
jsonpath "$.default_status" == 200
jsonpath "$.mode" == "examples"
jsonpath "$.settings.delay_ms" == 100
jsonpath "$.sample_request.method" == "GET"
jsonpath "$.url" startsWith "{{base_url}}/mock/docs-shop/products/"
# ANCHOR_END: operation

# ANCHOR: sample
GET {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct/sample?mode=generated&seed=3
HTTP 200
[Asserts]
jsonpath "$.request.method" == "GET"
jsonpath "$.url" startsWith "{{base_url}}/mock/docs-shop/products/"
jsonpath "$.response.status" == 200
jsonpath "$.response.source" == "generated"
jsonpath "$.response.valid" == true
jsonpath "$.response.body.price" isNumber

GET {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct/sample?status=404
HTTP 200
[Asserts]
jsonpath "$.response.status" == 404
jsonpath "$.response.source" == "example:gone"
# ANCHOR_END: sample

# ANCHOR: validate
POST {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct/validate
Content-Type: application/json
{"status": 200, "body": {"productId": "seven", "name": "Mug"}}
HTTP 200
[Asserts]
jsonpath "$.valid" == false
jsonpath "$.issues[*].pointer" contains "/productId"
jsonpath "$.issues[*].message" contains "expected integer, got string \"seven\""
jsonpath "$.issues[*].message" contains "missing required property \"price\""

POST {{base_url}}/_rustybin/mocks/docs-shop/operations/createCart/validate
Content-Type: application/json
{"kind": "request", "body": {"items": [{"productId": 1, "quantity": 1}]}}
HTTP 200
[Asserts]
jsonpath "$.valid" == true
# ANCHOR_END: validate

# ANCHOR: validation_warn
# Default "warn": the mock answers, flags the request and records the issues.
POST {{base_url}}/mock/docs-shop/carts
Content-Type: application/json
{"items": [], "coupon": 42}
HTTP 201
[Asserts]
header "X-Rustybin-Mock-Validation" == "failed"

GET {{base_url}}/_rustybin/mocks/docs-shop/requests?limit=1
HTTP 200
[Asserts]
jsonpath "$.requests[0].validation[*].pointer" contains "/body/items"
jsonpath "$.requests[0].validation[*].pointer" contains "/body/coupon"
# ANCHOR_END: validation_warn

# ANCHOR: validation_enforce
PATCH {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"responses": {"validate_requests": "enforce"}, "deploy": true}
HTTP 200

POST {{base_url}}/mock/docs-shop/carts
Content-Type: application/json
{"items": [], "coupon": 42}
HTTP 400
[Asserts]
header "Content-Type" == "application/problem+json"
jsonpath "$.detail" == "the request does not match the OpenAPI description"
jsonpath "$.operation" == "createCart"
jsonpath "$.errors[*].pointer" contains "/body/coupon"
jsonpath "$.errors[*].pointer" contains "/body/items"
jsonpath "$.errors[*].message" contains "expected string, got integer 42"

GET {{base_url}}/mock/docs-shop/products/abc
HTTP 400
[Asserts]
jsonpath "$.errors[0].pointer" == "/path/productId"

# The operation answers JSON only: an XML-only client gets 406.
GET {{base_url}}/mock/docs-shop/products/7
Accept: application/xml
HTTP 406
[Asserts]
jsonpath "$.available" count == 1
jsonpath "$.available[0]" == "application/json"

# Restore the default (warn).
PATCH {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"responses": {"validate_requests": "warn"}, "deploy": true}
HTTP 200
# ANCHOR_END: validation_enforce

# ANCHOR: checks_block
# A custom body that breaks the schema is a deploy error.
PUT {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"respond": {"body": {"productId": "seven"}}}
HTTP 200
[Asserts]
jsonpath "$.report.status" == "error"

POST {{base_url}}/_rustybin/mocks/docs-shop/deploy
Authorization: Bearer {{admin_token}}
HTTP 422
[Asserts]
jsonpath "$.error" contains "deploy checks found 1 error"
jsonpath "$.report.ok" == false
jsonpath "$.report.errors" == 1
jsonpath "$.report.operations[0].id" == "getProduct"
jsonpath "$.report.operations[0].status" == "error"
jsonpath "$.report.operations[0].findings[0].message" contains "response does not match the schema"

# The live revision still answers; clear the operation settings instead.
PUT {{base_url}}/_rustybin/mocks/docs-shop/operations/getProduct
Authorization: Bearer {{admin_token}}
Content-Type: application/json
null
HTTP 200
[Asserts]
jsonpath "$.report.status" == "ok"
# ANCHOR_END: checks_block

# ANCHOR: report
GET {{base_url}}/_rustybin/mocks/docs-shop/report
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.errors" == 0
jsonpath "$.operations[*].id" contains "createCart"
jsonpath "$.operations[0].id" == "getProduct"
jsonpath "$.operations[0].status" == "ok"
jsonpath "$.operations[0].request.path" matches /^\/products\/\d+$/
jsonpath "$.operations[0].responses[*].status" contains 404
jsonpath "$.operations[1].request.body.items" isCollection
jsonpath "$.operations[1].responses[*].source" contains "rule 1"
# ANCHOR_END: report

DELETE {{base_url}}/_rustybin/mocks/docs-shop
Authorization: Bearer {{admin_token}}
HTTP 200
