# Request inspector and sessions (docs/src/concepts/inspector.md)

# ANCHOR: tagged_request
POST {{base_url}}/anything/checkout?step=1
X-Rustybin-Session: docs-inspector
X-Request-Id: docs-inspector-0001
Content-Type: application/json
{"cart": 42}
HTTP 200
# ANCHOR_END: tagged_request

# ANCHOR: list
GET {{base_url}}/_rustybin/requests?session=docs-inspector&limit=10
HTTP 200
[Captures]
entry_id: jsonpath "$.requests[0].id"
[Asserts]
jsonpath "$.public_mode" == false
jsonpath "$.requests[0].method" == "POST"
jsonpath "$.requests[0].path" == "/anything/checkout"
jsonpath "$.requests[0].query" == "step=1"
jsonpath "$.requests[0].request_id" == "docs-inspector-0001"
jsonpath "$.requests[0].session" == "docs-inspector"
jsonpath "$.requests[0].body" == "{\"cart\": 42}"
jsonpath "$.requests[0].status" == 200
jsonpath "$.requests[0].latency_ms" exists
# ANCHOR_END: list

# ANCHOR: get_one
GET {{base_url}}/_rustybin/requests/{{entry_id}}
HTTP 200
[Asserts]
jsonpath "$.id" == "{{entry_id}}"
jsonpath "$.headers[?(@.name == 'x-rustybin-session')].value" nth 0 == "docs-inspector"
# ANCHOR_END: get_one

# ANCHOR: filter_prefix
GET {{base_url}}/_rustybin/requests?path_prefix=/anything/checkout&limit=1
HTTP 200
[Asserts]
jsonpath "$.count" == 1
# ANCHOR_END: filter_prefix

# ANCHOR: clear_session
DELETE {{base_url}}/_rustybin/requests?session=docs-inspector
HTTP 200
[Asserts]
jsonpath "$.session" == "docs-inspector"
jsonpath "$.cleared" >= 1
# ANCHOR_END: clear_session

# ANCHOR: clear_all_needs_admin
# Clearing everything is a global mutation: it needs the admin token when one is configured.
DELETE {{base_url}}/_rustybin/requests
HTTP 401

DELETE {{base_url}}/_rustybin/requests
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.cleared" exists
# ANCHOR_END: clear_all_needs_admin
