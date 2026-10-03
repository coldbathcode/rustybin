# Flaky endpoints (docs/src/reference/reliability.md)
# Counters are per session (X-Rustybin-Session, else client IP) and per route.

# ANCHOR: flaky_rate
GET {{base_url}}/flaky/100
HTTP 503
[Asserts]
header "X-Rustybin-Flaky" == "true"
header "X-Rustybin-Fail-Rate" == "100"
header "Retry-After" == "1"

GET {{base_url}}/flaky/0
HTTP 200
# ANCHOR_END: flaky_rate

# ANCHOR: pattern
GET {{base_url}}/flaky/pattern/SF
X-Rustybin-Session: docs-flaky
HTTP 200
[Asserts]
jsonpath "$.position" == 0
jsonpath "$.current" == "S"

GET {{base_url}}/flaky/pattern/SF
X-Rustybin-Session: docs-flaky
HTTP 503
[Asserts]
header "X-Rustybin-Request-Number" == "2"

# The pattern repeats.
GET {{base_url}}/flaky/pattern/SF
X-Rustybin-Session: docs-flaky
HTTP 200
# ANCHOR_END: pattern

# ANCHOR: after
GET {{base_url}}/flaky/after/1
X-Rustybin-Session: docs-flaky
HTTP 200

GET {{base_url}}/flaky/after/1
X-Rustybin-Session: docs-flaky
HTTP 503
# ANCHOR_END: after

# ANCHOR: recover
GET {{base_url}}/flaky/recover/1
X-Rustybin-Session: docs-flaky
HTTP 503
[Asserts]
jsonpath "$.message" == "failure 1 of 1 before recovering"

GET {{base_url}}/flaky/recover/1
X-Rustybin-Session: docs-flaky
HTTP 200
[Asserts]
jsonpath "$.message" == "recovered after 1 failures"
# ANCHOR_END: recover

# ANCHOR: status_reset
GET {{base_url}}/flaky/status
X-Rustybin-Session: docs-flaky
HTTP 200
[Asserts]
jsonpath "$.session" == "docs-flaky"
jsonpath "$.counters" count == 3

POST {{base_url}}/flaky/reset
X-Rustybin-Session: docs-flaky
HTTP 200
[Asserts]
jsonpath "$.scope" == "session"
jsonpath "$.counters_removed" == 3
# ANCHOR_END: status_reset

# ANCHOR: reset_all
POST {{base_url}}/flaky/reset?scope=all
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.scope" == "all"
# ANCHOR_END: reset_all
