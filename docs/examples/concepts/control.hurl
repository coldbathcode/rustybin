# Control plane and request ids (docs/src/concepts/control-plane.md)

# ANCHOR: config
GET {{base_url}}/_rustybin/config
HTTP 200
[Asserts]
jsonpath "$.instance_id" == "docs-examples"
jsonpath "$.admin_token_configured" == true
jsonpath "$.admin_token" not exists
jsonpath "$.mtls_in_header" == "X-Client-Cert"
jsonpath "$.max_delay_ms" == 30000
jsonpath "$.inspector_capacity" == 500
# ANCHOR_END: config

# ANCHOR: version
GET {{base_url}}/_rustybin/version
HTTP 200
[Asserts]
jsonpath "$.name" == "rustybin"
jsonpath "$.version" exists
jsonpath "$.profile" exists
# ANCHOR_END: version

# ANCHOR: request_id_generated
GET {{base_url}}/uuid
HTTP 200
[Asserts]
header "X-Request-Id" matches /^[0-9a-f-]{36}$/
# ANCHOR_END: request_id_generated

# ANCHOR: not_found
GET {{base_url}}/no/such/route
HTTP 404
[Asserts]
jsonpath "$.error" == "not found"
jsonpath "$.path" == "/no/such/route"
jsonpath "$.hint" contains "openapi.json"
# ANCHOR_END: not_found

# ANCHOR: cors_preflight
OPTIONS {{base_url}}/echo
Origin: https://app.example.com
Access-Control-Request-Method: PUT
Access-Control-Request-Headers: x-custom
HTTP 200
[Asserts]
header "Access-Control-Allow-Origin" == "*"
header "Access-Control-Allow-Methods" == "PUT"
header "Access-Control-Allow-Headers" == "x-custom"
header "Access-Control-Max-Age" == "600"
# ANCHOR_END: cors_preflight
