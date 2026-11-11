# A mock recorded from a real API (docs/src/mocks/record.md): a recording
# started (its proxy URL, and the outbound policy's answer for the upstream),
# a request through the proxy that the policy refuses (the docs instance
# allows no outbound host), a build refused while nothing is recorded, and
# the recording deleted.

# ANCHOR: start
POST {{base_url}}/_rustybin/recordings
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"name": "orders", "upstream": "https://api.example.com/v1"}
HTTP 201
[Captures]
proxy_url: jsonpath "$.proxy_url"
[Asserts]
jsonpath "$.state" == "recording"
jsonpath "$.proxy_url" contains "/record/"
jsonpath "$.proxy_url" not contains "orders"
jsonpath "$.mock" == "orders"
jsonpath "$.egress.allowed" == false
jsonpath "$.limits.exchanges" == 500
# ANCHOR_END: start

# ANCHOR: proxy
GET {{proxy_url}}/orders/42
HTTP 403
[Asserts]
jsonpath "$.reason" == "egress_refused"
jsonpath "$.purpose" == "record"
# ANCHOR_END: proxy

# ANCHOR: list
GET {{base_url}}/_rustybin/recordings
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.recordings[0].name" == "orders"
jsonpath "$.recordings[0].refused" == 1
jsonpath "$.recordings[0].exchanges" == 0
# ANCHOR_END: list

# ANCHOR: build
POST {{base_url}}/_rustybin/recordings/orders/build
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"dry_run": true}
HTTP 409
[Asserts]
jsonpath "$.error" contains "nothing recorded yet"
# ANCHOR_END: build

# ANCHOR: delete
DELETE {{base_url}}/_rustybin/recordings/orders
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.deleted" == "orders"
# ANCHOR_END: delete
