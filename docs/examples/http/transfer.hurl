# Compression, ranges and data transfer (docs/src/reference/transfer.md)

# ANCHOR: gzip
# Always compressed, whatever Accept-Encoding says: a gateway can be shown
# decompressing (or passing through) the body.
GET {{base_url}}/gzip
HTTP 200
[Asserts]
header "Content-Encoding" == "gzip"
header "Vary" contains "Accept-Encoding"
jsonpath "$.gzipped" == true
jsonpath "$.method" == "GET"
# ANCHOR_END: gzip

# ANCHOR: brotli
GET {{base_url}}/brotli
HTTP 200
[Asserts]
header "Content-Encoding" == "br"
jsonpath "$.brotli" == true
# ANCHOR_END: brotli

# ANCHOR: deflate_zstd
GET {{base_url}}/deflate
HTTP 200
[Asserts]
header "Content-Encoding" == "deflate"

GET {{base_url}}/zstd
HTTP 200
[Asserts]
header "Content-Encoding" == "zstd"
# ANCHOR_END: deflate_zstd

# ANCHOR: utf8
GET {{base_url}}/encoding/utf8
HTTP 200
[Asserts]
header "Content-Type" == "text/html; charset=utf-8"
body contains "Grüße"
# ANCHOR_END: utf8

# ANCHOR: range
GET {{base_url}}/range/100
Range: bytes=0-9
HTTP 206
[Asserts]
header "Content-Range" == "bytes 0-9/100"
header "Accept-Ranges" == "bytes"
header "ETag" == "\"range100\""
body == "abcdefghij"
# ANCHOR_END: range

# ANCHOR: range_suffix
GET {{base_url}}/range/100
Range: bytes=-4
HTTP 206
[Asserts]
header "Content-Range" == "bytes 96-99/100"
body == "stuv"
# ANCHOR_END: range_suffix

# ANCHOR: range_if_range
# If-Range with a different ETag: the full body comes back.
GET {{base_url}}/range/100
Range: bytes=0-9
If-Range: "something-else"
HTTP 200
[Asserts]
bytes count == 100
# ANCHOR_END: range_if_range

# ANCHOR: range_416
GET {{base_url}}/range/100
Range: bytes=500-
HTTP 416
[Asserts]
header "Content-Range" == "bytes */100"
# ANCHOR_END: range_416

# ANCHOR: bytes
GET {{base_url}}/bytes/16?seed=42
HTTP 200
[Captures]
first: bytes
[Asserts]
header "Content-Type" == "application/octet-stream"
bytes count == 16

# Same seed, same bytes.
GET {{base_url}}/bytes/16?seed=42
HTTP 200
[Asserts]
bytes == {{first}}
# ANCHOR_END: bytes

# ANCHOR: bytes_capped
GET {{base_url}}/bytes/500000
HTTP 200
[Asserts]
header "X-Rustybin-Capped" == "102400"
bytes count == 102400
# ANCHOR_END: bytes_capped

# ANCHOR: stream
GET {{base_url}}/stream/3?trace=on
HTTP 200
[Asserts]
header "Content-Type" == "application/json"
body split "\n" count == 4
body contains "\"id\":2"
body contains "\"args\":{\"trace\":\"on\"}"
# ANCHOR_END: stream

# ANCHOR: stream_bytes
GET {{base_url}}/stream-bytes/4096?chunk_size=1024&seed=1
HTTP 200
[Asserts]
bytes count == 4096
# ANCHOR_END: stream_bytes

# ANCHOR: drip
GET {{base_url}}/drip?duration=0.5&numbytes=5&delay=0.1&code=202
HTTP 202
[Asserts]
header "Content-Length" == "5"
body == "*****"
duration >= 500
# ANCHOR_END: drip

# ANCHOR: links
GET {{base_url}}/links/3
HTTP 302
[Asserts]
header "Location" == "/links/3/0"

GET {{base_url}}/links/3/1
HTTP 200
[Asserts]
header "Content-Type" contains "text/html"
body contains "<a href='/links/3/0'>0</a>"
# ANCHOR_END: links

# ANCHOR: base64
GET {{base_url}}/base64/SFRUUEJJTiBpcyBhd2Vzb21l
HTTP 200
[Asserts]
body == "HTTPBIN is awesome"
# ANCHOR_END: base64
