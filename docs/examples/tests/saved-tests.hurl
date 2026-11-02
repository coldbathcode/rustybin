# Saved tests (docs/src/saved-tests.md): a captured request saved as a
# test, a written one, a run (JSON and JUnit), the Hurl export, cleanup.

# ANCHOR: capture
GET {{base_url}}/anything/orders?page=2
X-Rustybin-Session: docs-tests
X-Api-Key: docs-secret-value
HTTP 200

GET {{base_url}}/_rustybin/requests?session=docs-tests&limit=1
HTTP 200
[Captures]
capture: jsonpath "$.requests[0].id"
# ANCHOR_END: capture

# ANCHOR: save_capture
POST {{base_url}}/_rustybin/tests
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-tests
{"name": "orders-page-2", "capture": "{{capture}}"}
HTTP 201
[Asserts]
jsonpath "$.request.path" == "/anything/orders?page=2"
jsonpath "$.request.headers['x-api-key']" endsWith "X_API_KEY}}"
body not contains "docs-secret-value"
jsonpath "$.introduced_variables" contains "X_API_KEY"
jsonpath "$.assert[0].status" == 200
# ANCHOR_END: save_capture

# ANCHOR: save_written
POST {{base_url}}/_rustybin/tests
Authorization: Bearer {{admin_token}}
{
  "name": "created-order",
  "request": {"method": "POST", "path": "/status/201", "body": "{\"id\": 1}"},
  "assert": [{"status": 201}, {"max_ms": 5000}]
}
HTTP 201
# ANCHOR_END: save_written

# ANCHOR: run
POST {{base_url}}/_rustybin/tests/run
Authorization: Bearer {{admin_token}}
{"variables": {"X_API_KEY": "docs-secret-value"}}
HTTP 200
[Asserts]
jsonpath "$.passed" == true
jsonpath "$.total" == 2
# ANCHOR_END: run

# ANCHOR: missing_variable
POST {{base_url}}/_rustybin/tests/run
Authorization: Bearer {{admin_token}}
{"names": ["orders-page-2"]}
HTTP 200
[Asserts]
jsonpath "$.passed" == false
jsonpath "$.results[0].error" contains "needs the variable X_API_KEY"
# ANCHOR_END: missing_variable

# ANCHOR: junit
POST {{base_url}}/_rustybin/tests/run?format=junit
Authorization: Bearer {{admin_token}}
{"names": ["created-order"]}
HTTP 200
[Asserts]
header "Content-Type" startsWith "application/xml"
xpath "string(//testsuite/@tests)" == "1"
xpath "count(//failure)" == 0
# ANCHOR_END: junit

# ANCHOR: export
GET {{base_url}}/_rustybin/tests/export
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
body contains "POST "
body contains "/status/201"
body contains "status == 201"
body contains "duration <= 5000"
# ANCHOR_END: export

# ANCHOR: delete
DELETE {{base_url}}/_rustybin/tests/orders-page-2
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/tests/created-order
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: delete
