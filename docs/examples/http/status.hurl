# Status codes (docs/src/reference/http-basics.md)

# ANCHOR: status_single
GET {{base_url}}/status/418
HTTP 418
[Asserts]
jsonpath "$.status" == 418
# ANCHOR_END: status_single

# ANCHOR: status_any_method
DELETE {{base_url}}/status/204
HTTP 204
[Asserts]
bytes count == 0
# ANCHOR_END: status_any_method

# ANCHOR: status_extra_headers
GET {{base_url}}/status/503
HTTP 503
[Asserts]
header "Retry-After" == "1"

GET {{base_url}}/status/401
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Basic realm=\"rustybin\""

GET {{base_url}}/status/301
HTTP 301
[Asserts]
header "Location" == "/echo"
# ANCHOR_END: status_extra_headers

# ANCHOR: status_weighted
# Weights are relative: 0 never wins, so this always answers 500.
GET {{base_url}}/status/200:0,500:1
HTTP 500
[Asserts]
jsonpath "$.status" == 500
jsonpath "$.chosen_from" == "200:0,500:1"
# ANCHOR_END: status_weighted

# ANCHOR: status_invalid
GET {{base_url}}/status/101
HTTP 400
[Asserts]
jsonpath "$.error" == "unsupported_status_code"
# ANCHOR_END: status_invalid

# ANCHOR: status_xml
GET {{base_url}}/status/404
Accept: application/xml
HTTP 404
[Asserts]
header "Content-Type" contains "application/xml"
xpath "string(/response/status)" == "404"
# ANCHOR_END: status_xml
