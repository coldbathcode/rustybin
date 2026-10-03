# Info, random values and images (docs/src/reference/transfer.md)

# ANCHOR: ip
GET {{base_url}}/ip
HTTP 200
[Asserts]
jsonpath "$.ipv4" == "127.0.0.1"
jsonpath "$.ipv6" == null
# ANCHOR_END: ip

# ANCHOR: time
GET {{base_url}}/time/Europe/London
HTTP 200
[Asserts]
jsonpath "$.timezone" == "Europe/London"
jsonpath "$.time" isIsoDate

GET {{base_url}}/date/Mars/Olympus_Mons
HTTP 404
[Asserts]
jsonpath "$.error" == "unknown_timezone"
# ANCHOR_END: time

# ANCHOR: random
GET {{base_url}}/uuid
HTTP 200
[Asserts]
jsonpath "$.uuid" matches /^[0-9a-f-]{36}$/

GET {{base_url}}/random/int/1/6
HTTP 200
[Asserts]
jsonpath "$.value" >= 1
jsonpath "$.value" <= 6

GET {{base_url}}/random/lorem-ipsum/2
HTTP 200
[Asserts]
jsonpath "$.paragraphs" count == 2
# ANCHOR_END: random

# ANCHOR: images
GET {{base_url}}/image
Accept: image/png
HTTP 200
[Asserts]
header "Content-Type" == "image/png"

GET {{base_url}}/image/svg
HTTP 200
[Asserts]
header "Content-Type" contains "image/svg+xml"

GET {{base_url}}/image
Accept: text/plain
HTTP 406
# ANCHOR_END: images
