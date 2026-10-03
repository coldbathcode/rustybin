# Fault injection headers (docs/src/concepts/fault-injection.md)

# ANCHOR: fail
GET {{base_url}}/echo
X-Rustybin-Fail: 503
HTTP 503
[Asserts]
header "X-Rustybin-Fault" == "fail"
jsonpath "$.error" == "injected fault"
jsonpath "$.status" == 503
jsonpath "$.percent" == 100
jsonpath "$.injected_by" == "X-Rustybin-Fail"
# ANCHOR_END: fail

# ANCHOR: fail_percent
# 0 percent never fails; 503:50 fails about half of the requests.
GET {{base_url}}/echo
X-Rustybin-Fail: 503:0
HTTP 200
# ANCHOR_END: fail_percent

# ANCHOR: delay
POST {{base_url}}/anything/orders
X-Rustybin-Delay: 300
Content-Type: application/json
{"id": 1}
HTTP 200
[Asserts]
header "X-Rustybin-Fault" == "delay"
duration >= 300
# ANCHOR_END: delay

# ANCHOR: delay_and_fail
GET {{base_url}}/status/200
X-Rustybin-Delay: 200
X-Rustybin-Fail: 504
HTTP 504
[Asserts]
duration >= 200
# ANCHOR_END: delay_and_fail

# ANCHOR: invalid_ignored
# Values that do not parse are ignored (the request is served normally).
GET {{base_url}}/echo
X-Rustybin-Fail: 200
HTTP 200
# ANCHOR_END: invalid_ignored

# ANCHOR: control_exempt
GET {{base_url}}/_rustybin/version
X-Rustybin-Fail: 500
HTTP 200
[Asserts]
jsonpath "$.name" == "rustybin"
# ANCHOR_END: control_exempt
