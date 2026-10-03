# Landing page, OpenAPI and collection exports (docs/src/exports.md)

# ANCHOR: landing
GET {{base_url}}/
HTTP 200
[Asserts]
header "Content-Type" contains "text/html"
body contains "/openapi.json"
# ANCHOR_END: landing

# ANCHOR: openapi
GET {{base_url}}/openapi.json
HTTP 200
[Asserts]
jsonpath "$.openapi" == "3.0.3"
jsonpath "$.paths['/echo']" exists
jsonpath "$.paths['/mcp']" exists

GET {{base_url}}/openapi.yaml
HTTP 200
[Asserts]
body contains "\nopenapi: 3.0.3"
# ANCHOR_END: openapi

# ANCHOR: api_reference
GET {{base_url}}/docs
HTTP 200
[Asserts]
header "Content-Type" contains "text/html"
# ANCHOR_END: api_reference

# ANCHOR: postman
GET {{base_url}}/export/postman.json
HTTP 200
[Asserts]
jsonpath "$.info.schema" contains "v2.1.0"
jsonpath "$.variable[0].key" == "base_url"
jsonpath "$.variable[0].value" == "{{base_url}}"
# ANCHOR_END: postman

# ANCHOR: base_url_override
GET {{base_url}}/export/postman.json?base_url=https://gateway.example.com/rustybin
HTTP 200
[Asserts]
jsonpath "$.variable[0].value" == "https://gateway.example.com/rustybin"

GET {{base_url}}/export/curl.sh?base_url=not-a-url
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid base_url"
# ANCHOR_END: base_url_override

# ANCHOR: other_exports
GET {{base_url}}/export/insomnia.json
HTTP 200
[Asserts]
jsonpath "$.__export_format" == 4

GET {{base_url}}/export/bruno.json
HTTP 200

GET {{base_url}}/export/curl.sh
HTTP 200
[Asserts]
body startsWith "#!"

GET {{base_url}}/export/requests.http
HTTP 200

GET {{base_url}}/export/requests.hurl
HTTP 200
[Asserts]
body contains "HTTP"

GET {{base_url}}/export/k6.js
HTTP 200
[Asserts]
body contains "k6/http"

GET {{base_url}}/export/har.json
HTTP 200
[Asserts]
jsonpath "$.log.version" == "1.2"
# ANCHOR_END: other_exports
