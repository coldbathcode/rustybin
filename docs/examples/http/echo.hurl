# Echo and reflection (docs/src/reference/http-basics.md)

# ANCHOR: echo_get
GET {{base_url}}/echo/orders/42?expand=items&expand=customer
X-Demo: hello
HTTP 200
[Asserts]
jsonpath "$.method" == "GET"
jsonpath "$.path" == "/echo/orders/42"
jsonpath "$.path_info" count == 3
jsonpath "$.query_params.expand" count == 2
jsonpath "$.headers['x-demo'][0]" == "hello"
jsonpath "$.scheme" == "http"
header "X-Request-Id" exists
# ANCHOR_END: echo_get

# ANCHOR: echo_post
POST {{base_url}}/anything
Content-Type: application/json
{"message": "hello", "number": 42}
HTTP 200
[Asserts]
jsonpath "$.method" == "POST"
jsonpath "$.body.present" == true
jsonpath "$.body.utf8" == true
jsonpath "$.body.bytes" == 34
jsonpath "$.body.body" contains "hello"
# ANCHOR_END: echo_post

# ANCHOR: echo_xml
GET {{base_url}}/echo
Accept: application/xml
HTTP 200
[Asserts]
header "Content-Type" contains "application/xml"
header "Vary" contains "Accept"
xpath "string(/response/method)" == "GET"
# ANCHOR_END: echo_xml

# ANCHOR: request_id
GET {{base_url}}/echo
X-Request-Id: demo-trace-0001
HTTP 200
[Asserts]
header "X-Request-Id" == "demo-trace-0001"
jsonpath "$.headers['x-request-id'][0]" == "demo-trace-0001"
# ANCHOR_END: request_id
