# Server-Sent Events (docs/src/reference/sse.md)
# Finite streams end on their own, so Hurl can read the whole body.

# ANCHOR: sse
GET {{base_url}}/sse?count=3&interval_ms=100&event=tick&retry=2000
HTTP 200
[Asserts]
header "Content-Type" == "text/event-stream"
header "Cache-Control" == "no-cache"
body contains "id: 1\nevent: tick\n"
body contains "retry: 2000"
body contains "\"message\":\"event 3 of 3\""
# ANCHOR_END: sse

# ANCHOR: sse_resume
GET {{base_url}}/sse?count=5&interval_ms=50
Last-Event-ID: 3
HTTP 200
[Asserts]
body not contains "id: 3\n"
body contains "id: 4\n"
body contains "id: 5\n"
# ANCHOR_END: sse_resume

# ANCHOR: sse_text
GET {{base_url}}/sse?count=2&interval_ms=50&mode=text
HTTP 200
[Asserts]
body contains "data: event 1 of 2"
# ANCHOR_END: sse_text

# ANCHOR: sse_chat
POST {{base_url}}/sse/chat
Content-Type: application/json
{"prompt": "Tell me a story", "max_tokens": 5, "delay_ms": 20}
HTTP 200
[Asserts]
header "Content-Type" == "text/event-stream"
body contains "\"object\":\"chat.chunk\""
body contains "\"finish_reason\":\"length\""
body contains "\"usage\":{\"completion_tokens\":5"
body endsWith "data: [DONE]\n\n"
# ANCHOR_END: sse_chat
