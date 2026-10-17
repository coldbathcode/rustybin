# OpenAPI mocks as code (docs/src/mocks/config.md). run.sh starts the main
# instance with RUSTYBIN_CONFIG_FILE=docs/examples/mocks/rustybin.yaml,
# RUSTYBIN_MOCKS_DIR=docs/examples/mocks/specs-dir and
# INVENTORY_API_KEY=docs-inventory-key.

# ANCHOR: file_mocks
GET {{base_url}}/_rustybin/mocks/inventory
HTTP 200
[Asserts]
jsonpath "$.origin" == "file"
jsonpath "$.state" == "live"
jsonpath "$.url" == "{{base_url}}/mock/inventory"
jsonpath "$.settings.responses.mode" == "generated"
jsonpath "$.settings.security.mode" == "api_key"

GET {{base_url}}/_rustybin/mocks/weather
HTTP 200
[Asserts]
jsonpath "$.origin" == "file"
jsonpath "$.title" == "Weather"
# ANCHOR_END: file_mocks

# ANCHOR: call_inventory
# The key comes from ${INVENTORY_API_KEY:-inventory-key}.
GET {{base_url}}/mock/inventory/items
x-api-key: docs-inventory-key
HTTP 200
[Asserts]
jsonpath "$" isCollection
jsonpath "$[0].sku" matches /^SKU-[0-9]+$/

GET {{base_url}}/mock/inventory/items/SKU-0
x-api-key: docs-inventory-key
HTTP 404
[Asserts]
header "X-Rustybin-Mock-Source" startsWith "rule:sold out"

# validate_requests: enforce
GET {{base_url}}/mock/inventory/items/42
x-api-key: docs-inventory-key
HTTP 400
[Asserts]
jsonpath "$.errors[0].pointer" == "/path/sku"

# health is public
GET {{base_url}}/mock/inventory/health
HTTP 200

GET {{base_url}}/mock/weather/forecast/oslo
HTTP 200
[Asserts]
jsonpath "$.city" == "oslo"
# ANCHOR_END: call_inventory

# ANCHOR: reload
POST {{base_url}}/_rustybin/mocks/reload
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.report.unchanged" contains "inventory"
jsonpath "$.report.unchanged" contains "weather"
jsonpath "$.problems" isEmpty
# ANCHOR_END: reload

# ANCHOR: schema
GET {{base_url}}/_rustybin/mocks/config.schema.json
HTTP 200
[Asserts]
jsonpath "$.title" == "Rustybin mocks file (rustybin.yaml)"
jsonpath "$.properties.mocks.items.required" contains "name"
# ANCHOR_END: schema

DELETE {{base_url}}/_rustybin/mocks/docs-hello
Authorization: Bearer {{admin_token}}
HTTP *

DELETE {{base_url}}/_rustybin/mocks/docs-goodbye
Authorization: Bearer {{admin_token}}
HTTP *

# ANCHOR: apply
POST {{base_url}}/_rustybin/mocks/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
```
version: 1
mocks:
  - name: docs-hello
    responses: { mode: examples }
    spec_inline:
      openapi: 3.1.0
      info: { title: Hello, version: "1" }
      paths:
        /hello:
          get:
            responses:
              "200":
                description: A greeting
                content:
                  application/json:
                    example: { message: hello }
  - name: docs-goodbye
    enabled: false
    spec_inline: |
      openapi: 3.0.3
      info: { title: Goodbye, version: "1" }
      paths: {}
```
HTTP 200
[Asserts]
jsonpath "$.created" contains "docs-hello"
jsonpath "$.created" contains "docs-goodbye"
jsonpath "$.errors" isEmpty

GET {{base_url}}/mock/docs-hello/hello
HTTP 200
[Asserts]
header "X-Rustybin-Mock-Operation" == "get_hello"
jsonpath "$.message" == "hello"

GET {{base_url}}/mock/docs-goodbye/anything
HTTP 503
# ANCHOR_END: apply

# ANCHOR: apply_file_path
# Documents sent to the API cannot reference files on the server.
POST {{base_url}}/_rustybin/mocks/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
```
mocks:
  - name: docs-nope
    spec: /etc/passwd
```
HTTP 200
[Asserts]
jsonpath "$.created" isEmpty
jsonpath "$.errors[0].name" == "docs-nope"
jsonpath "$.errors[0].error" contains "send spec_inline"
# ANCHOR_END: apply_file_path

# ANCHOR: export
GET {{base_url}}/_rustybin/mocks/docs-hello/export
HTTP 200
[Asserts]
header "Content-Type" == "application/yaml; charset=utf-8"
header "Content-Disposition" == "inline; filename=\"docs-hello.rustybin.yaml\""
body startsWith "# Rustybin mocks (rustybin.yaml)"
body contains "- name: docs-hello"
body contains "spec_inline:"
body contains "validate_requests: warn"

GET {{base_url}}/_rustybin/mocks/export
HTTP 200
[Asserts]
body contains "- name: docs-hello"
body contains "- name: inventory"
body not contains "- name: petstore"
# ANCHOR_END: export

# ANCHOR: prune
# Only docs-hello is listed: prune deletes the other mocks created through
# the API (file-defined mocks and the sample are never pruned).
POST {{base_url}}/_rustybin/mocks/apply?prune=true
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"version": 1, "mocks": [{"name": "docs-hello", "spec_inline": {"openapi": "3.1.0", "info": {"title": "Hello", "version": "1"}, "paths": {}}}]}
HTTP 200
[Asserts]
jsonpath "$.updated" contains "docs-hello"
jsonpath "$.deleted" contains "docs-goodbye"
jsonpath "$.deleted" not contains "inventory"
# ANCHOR_END: prune

DELETE {{base_url}}/_rustybin/mocks/docs-hello
Authorization: Bearer {{admin_token}}
HTTP 200
