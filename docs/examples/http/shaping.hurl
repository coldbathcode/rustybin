# Delays, response headers and caching (docs/src/reference/http-basics.md)

# ANCHOR: delay
GET {{base_url}}/delay/250ms
HTTP 200
[Asserts]
jsonpath "$.delay_ms" == 250
jsonpath "$.requested_ms" == 250
jsonpath "$.max_delay_ms" == 60000
duration >= 250
# ANCHOR_END: delay

# ANCHOR: delay_units
# Plain milliseconds, `ms` or seconds (fractions allowed); ?jitter=true adds +-20%.
POST {{base_url}}/delay/0.1s?jitter=true
HTTP 200
[Asserts]
jsonpath "$.requested_ms" == 100
jsonpath "$.delay_ms" >= 80
jsonpath "$.delay_ms" <= 120
jsonpath "$.method" == "POST"
# ANCHOR_END: delay_units

# ANCHOR: delay_too_long
GET {{base_url}}/delay/61s
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid_delay"
# ANCHOR_END: delay_too_long

# ANCHOR: response_headers
GET {{base_url}}/response-headers?X-Demo=gateway&Cache-Control=no-store&X-Multi=a&X-Multi=b
HTTP 200
[Asserts]
header "X-Demo" == "gateway"
header "Cache-Control" == "no-store"
header "X-Multi" count == 2
jsonpath "$['x-demo']" == "gateway"
jsonpath "$['x-multi']" count == 2
# ANCHOR_END: response_headers

# ANCHOR: response_headers_forbidden
GET {{base_url}}/response-headers?Transfer-Encoding=chunked
HTTP 400
[Asserts]
jsonpath "$.error" == "forbidden_header"
# ANCHOR_END: response_headers_forbidden

# ANCHOR: cache
GET {{base_url}}/cache/60
HTTP 200
[Captures]
etag: header "ETag"
[Asserts]
header "Cache-Control" == "public, max-age=60"
header "ETag" exists
header "Last-Modified" exists
jsonpath "$.cached" == true
jsonpath "$.ttl" == 60

GET {{base_url}}/cache/60
If-None-Match: {{etag}}
HTTP 304
[Asserts]
header "ETag" == "{{etag}}"
bytes count == 0
# ANCHOR_END: cache

# ANCHOR: cache_ims
GET {{base_url}}/cache/30
If-Modified-Since: Fri, 01 Jan 2100 00:00:00 GMT
HTTP 304
# ANCHOR_END: cache_ims
