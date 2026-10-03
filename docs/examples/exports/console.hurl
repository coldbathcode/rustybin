# Web console and its control-plane endpoints (docs/src/console.md)

# ANCHOR: ui_redirect
GET {{base_url}}/ui
HTTP 307
[Asserts]
header "Location" == "ui/"
# ANCHOR_END: ui_redirect

# ANCHOR: ui_index
GET {{base_url}}/ui/
HTTP 200
[Asserts]
header "Content-Type" == "text/html; charset=utf-8"
header "Cache-Control" == "no-cache"
header "ETag" exists
header "Content-Security-Policy" contains "script-src 'self'"
# ANCHOR_END: ui_index

# ANCHOR: status
GET {{base_url}}/_rustybin/status
HTTP 200
[Asserts]
jsonpath "$.instance_id" == "docs-examples"
jsonpath "$.healthy" == true
jsonpath "$.admin_token_configured" == true
jsonpath "$.inspector.capacity" == 500
jsonpath "$.ports.grpc" exists
# ANCHOR_END: status

# ANCHOR: catalog
GET {{base_url}}/_rustybin/catalog
HTTP 200
[Asserts]
jsonpath "$.count" > 100
jsonpath "$.categories[0].name" == "Echo & Reflection"
jsonpath "$.endpoints[0].path" == "/echo"
jsonpath "$.endpoints[0].examples[0].method" == "GET"
# ANCHOR_END: catalog
