# Request bin (docs/src/reference/request-bin.md)

# ANCHOR: create
POST {{base_url}}/bin
X-Rustybin-Session: docs-bin
Content-Type: application/json
{"status": 202, "headers": {"X-Bin": "demo"}, "body": {"accepted": true}, "delay_ms": 100}
HTTP 201
[Captures]
bin_id: jsonpath "$.id"
[Asserts]
jsonpath "$.url" == "{{base_url}}/bin/{{bin_id}}"
jsonpath "$.requests_url" endsWith "/requests"
jsonpath "$.stream_url" endsWith "/requests/stream"
jsonpath "$.response.status" == 202
jsonpath "$.limits.max_requests" == 100
# ANCHOR_END: create

# ANCHOR: capture
# Point a webhook (or a gateway's logging plugin) at any sub path of the bin.
POST {{base_url}}/bin/{{bin_id}}/hooks/orders?attempt=1
Content-Type: application/json
X-Gateway-Consumer: alice
{"event": "order.created"}
HTTP 202
[Asserts]
header "X-Bin" == "demo"
header "X-Rustybin-Bin-Seq" == "1"
jsonpath "$.accepted" == true
duration >= 100
# ANCHOR_END: capture

# ANCHOR: inspect
GET {{base_url}}/bin/{{bin_id}}/requests
HTTP 200
[Asserts]
jsonpath "$.count" == 1
jsonpath "$.requests[0].method" == "POST"
jsonpath "$.requests[0].path" == "/hooks/orders"
jsonpath "$.requests[0].query" == "attempt=1"
jsonpath "$.requests[0].body" == "{\"event\": \"order.created\"}"
jsonpath "$.requests[0].headers[?(@.name == 'x-gateway-consumer')].value" nth 0 == "alice"

GET {{base_url}}/bin/{{bin_id}}/requests/1
HTTP 200
[Asserts]
jsonpath "$.seq" == 1
jsonpath "$.full_path" == "/bin/{{bin_id}}/hooks/orders"
# ANCHOR_END: inspect

# ANCHOR: list_bins
GET {{base_url}}/bin
X-Rustybin-Session: docs-bin
HTTP 200
[Asserts]
jsonpath "$.bins[*].id" contains "{{bin_id}}"
# ANCHOR_END: list_bins

# ANCHOR: default_response
POST {{base_url}}/bin
HTTP 201
[Captures]
plain_bin: jsonpath "$.id"

PUT {{base_url}}/bin/{{plain_bin}}
`anything at all`
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.seq" == 1
# ANCHOR_END: default_response

# ANCHOR: delete
DELETE {{base_url}}/bin/{{bin_id}}
HTTP 200

GET {{base_url}}/bin/{{bin_id}}/requests
HTTP 404
# ANCHOR_END: delete
