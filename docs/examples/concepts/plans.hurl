# Plan limits (docs/src/concepts/plans-and-limits.md)
#
# These run against the second instance of docs/examples/run.sh ({{plan_url}}):
# RUSTYBIN_PLAN=free with RUSTYBIN_LIMIT_RPS=1, RUSTYBIN_LIMIT_BURST=5,
# RUSTYBIN_LIMIT_REQUESTS=7, RUSTYBIN_LIMIT_EGRESS_MB=0.15,
# RUSTYBIN_LIMIT_STREAMS=1 and RUSTYBIN_LIMIT_STREAM_SECS=2. The free plan
# counts per session, so every example uses its own X-Rustybin-Session.

# ANCHOR: success_headers
GET {{plan_url}}/echo
X-Rustybin-Session: docs-headers
HTTP 200
[Asserts]
header "X-Rustybin-Plan" == "free"
header "X-Rustybin-Quota-Remaining" == "6"
# ANCHOR_END: success_headers

# ANCHOR: usage
GET {{plan_url}}/_rustybin/usage
X-Rustybin-Session: docs-headers
HTTP 200
[Asserts]
jsonpath "$.plan" == "free"
jsonpath "$.scope" == "session"
jsonpath "$.session" == "docs-headers"
jsonpath "$.limits.rps" == 1
jsonpath "$.limits.burst" == 5
jsonpath "$.period.kind" == "day"
jsonpath "$.requests.used" == 1
jsonpath "$.requests.remaining" == 6
jsonpath "$.egress.used_bytes" > 0
jsonpath "$.open_streams.limit" == 1
# ANCHOR_END: usage

# ANCHOR: rps
# The bucket holds 5 tokens and refills 1 per second: the 6th quick request is rejected.
GET {{plan_url}}/echo
X-Rustybin-Session: docs-rps
[Options]
repeat: 5
HTTP 200

GET {{plan_url}}/echo
X-Rustybin-Session: docs-rps
HTTP 429
[Asserts]
header "X-Rustybin-Limit" == "rps"
header "X-Rustybin-Plan" == "free"
header "Retry-After" == "1"
header "RateLimit" == "\"rps\";r=0;t=1"
header "RateLimit-Policy" contains "\"rps\";q=1;w=1;rustybin-burst=5"
header "RateLimit-Policy" contains "\"requests\";q=7;w=86400"
jsonpath "$.error" == "rustybin_plan_limit"
jsonpath "$.limit" == "rps"
jsonpath "$.plan" == "free"
jsonpath "$.message" startsWith "This 429 comes from the Rustybin plan limit, not from your gateway."
# ANCHOR_END: rps

# ANCHOR: admin_bypass
# Control plane requests with the admin token are never rejected...
GET {{plan_url}}/_rustybin/config
X-Rustybin-Session: docs-rps
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.plan" == "free"
jsonpath "$.limits.scope" == "session"

# ...but without it they are limited like any other route.
GET {{plan_url}}/_rustybin/config
X-Rustybin-Session: docs-rps
HTTP 429
# ANCHOR_END: admin_bypass

# ANCHOR: exempt
# The platform health check, the console and the usage endpoint are never
# limited nor counted (5 echoes and the admin call so far, rejections are free).
GET {{plan_url}}/
X-Rustybin-Session: docs-rps
[Options]
repeat: 10
HTTP 200
[Asserts]
header "X-Rustybin-Plan" not exists

GET {{plan_url}}/_rustybin/usage
X-Rustybin-Session: docs-rps
HTTP 200
[Asserts]
jsonpath "$.requests.used" == 6
# ANCHOR_END: exempt

# ANCHOR: quota
# 7 requests per day: after the burst, wait for two tokens, use them, then the
# daily quota is exhausted (Retry-After counts down to midnight UTC).
GET {{plan_url}}/echo
X-Rustybin-Session: docs-quota
[Options]
repeat: 5
HTTP 200

GET {{plan_url}}/echo
X-Rustybin-Session: docs-quota
[Options]
delay: 2200
HTTP 200

GET {{plan_url}}/echo
X-Rustybin-Session: docs-quota
HTTP 200
[Asserts]
header "X-Rustybin-Quota-Remaining" == "0"

GET {{plan_url}}/echo
X-Rustybin-Session: docs-quota
HTTP 429
[Asserts]
header "X-Rustybin-Limit" == "requests"
header "Retry-After" toInt > 0
header "RateLimit" startsWith "\"requests\";r=0;t="
jsonpath "$.limit" == "requests"
# ANCHOR_END: quota

# ANCHOR: egress
# 0.15 MB per day: the second 100 KB response is still sent in full, the third
# request is rejected.
GET {{plan_url}}/bytes/102400
X-Rustybin-Session: docs-egress
[Options]
repeat: 2
HTTP 200
[Asserts]
bytes count == 102400

GET {{plan_url}}/bytes/102400
X-Rustybin-Session: docs-egress
HTTP 429
[Asserts]
header "X-Rustybin-Limit" == "egress"
header "RateLimit-Policy" contains "\"egress\";q=150000;qu=\"content-bytes\";w=86400"

GET {{plan_url}}/_rustybin/usage
X-Rustybin-Session: docs-egress
HTTP 200
[Asserts]
jsonpath "$.egress.used_bytes" >= 204800
jsonpath "$.egress.remaining_bytes" == 0
# ANCHOR_END: egress

# ANCHOR: stream_lifetime
# Streams end cleanly after RUSTYBIN_LIMIT_STREAM_SECS (2 s here): a final SSE
# comment, then the end of the body.
GET {{plan_url}}/sse?count=100&interval_ms=500
X-Rustybin-Session: docs-stream
HTTP 200
[Asserts]
header "Content-Type" startsWith "text/event-stream"
body contains "data:"
body endsWith ": rustybin plan stream lifetime reached\n\n"
duration < 6000
# ANCHOR_END: stream_lifetime
