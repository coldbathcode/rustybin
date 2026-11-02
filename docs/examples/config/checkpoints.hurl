# Checkpoints (docs/src/checkpoints.md): a mock with stateful data saved in
# a checkpoint, the instance changed, a dry run, a restore, cleanup.

# ANCHOR: setup
POST {{base_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
{
  "name": "docs-cart",
  "responses": {"mode": "stateful"},
  "spec": {"openapi": "3.0.3", "info": {"title": "Cart", "version": "1"}, "paths": {
    "/items": {
      "get": {"operationId": "listItems", "responses": {"200": {"description": "ok", "content": {"application/json": {"example": []}}}}},
      "post": {"operationId": "addItem", "requestBody": {"content": {"application/json": {"schema": {"type": "object"}}}},
               "responses": {"201": {"description": "ok", "content": {"application/json": {"schema": {"type": "object"}}}}}}
    }
  }}
}
HTTP 201

POST {{base_url}}/mock/docs-cart/items
{"sku": "tea"}
HTTP 201
# ANCHOR_END: setup

# ANCHOR: save
POST {{base_url}}/_rustybin/checkpoints
Authorization: Bearer {{admin_token}}
{"name": "docs-demo-start"}
HTTP 201
[Asserts]
jsonpath "$.mocks" contains "docs-cart"
jsonpath "$.healthy" == true
# ANCHOR_END: save

# ANCHOR: change
POST {{base_url}}/mock/docs-cart/items
{"sku": "coffee"}
HTTP 201

GET {{base_url}}/mock/docs-cart/items
HTTP 200
[Asserts]
jsonpath "$" count == 2
# ANCHOR_END: change

# ANCHOR: dry_run
POST {{base_url}}/_rustybin/checkpoints/docs-demo-start/restore?dry_run=true
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.dry_run" == true
jsonpath "$.mocks.data_restored" contains "docs-cart"
# ANCHOR_END: dry_run

# ANCHOR: restore
POST {{base_url}}/_rustybin/checkpoints/docs-demo-start/restore
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.changed" == true
jsonpath "$.mocks.data_restored" contains "docs-cart"
jsonpath "$.health.after" == "healthy"

GET {{base_url}}/mock/docs-cart/items
HTTP 200
[Asserts]
jsonpath "$" count == 1
jsonpath "$[0].sku" == "tea"
# ANCHOR_END: restore

# ANCHOR: cleanup
DELETE {{base_url}}/_rustybin/checkpoints/docs-demo-start
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/mocks/docs-cart
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: cleanup
