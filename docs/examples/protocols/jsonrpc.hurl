# Generic JSON-RPC 2.0 (docs/src/reference/jsonrpc.md)

# ANCHOR: single
POST {{base_url}}/jsonrpc
Content-Type: application/json
{"jsonrpc": "2.0", "method": "subtract", "params": {"minuend": 42, "subtrahend": 23}, "id": 1}
HTTP 200
[Asserts]
jsonpath "$.result" == 19
jsonpath "$.id" == 1
# ANCHOR_END: single

# ANCHOR: batch
POST {{base_url}}/jsonrpc
Content-Type: application/json
[
  {"jsonrpc": "2.0", "method": "add", "params": [1, 2, 4], "id": "1"},
  {"jsonrpc": "2.0", "method": "echo", "params": {"hello": "world"}},
  {"jsonrpc": "2.0", "method": "echo", "params": ["x"], "id": "2"},
  {"jsonrpc": "2.0", "method": "nope", "id": "3"}
]
HTTP 200
[Asserts]
jsonpath "$" count == 3
jsonpath "$[0].result" == 7
jsonpath "$[1].result[0]" == "x"
jsonpath "$[2].error.code" == -32601
# ANCHOR_END: batch

# ANCHOR: notification
POST {{base_url}}/jsonrpc
Content-Type: application/json
{"jsonrpc": "2.0", "method": "echo", "params": ["fire and forget"]}
HTTP 204
# ANCHOR_END: notification

# ANCHOR: requested_error
POST {{base_url}}/jsonrpc
Content-Type: application/json
{"jsonrpc": "2.0", "method": "error", "params": {"code": -32001, "message": "Rate limited", "data": {"retry_after": 1}}, "id": 7}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32001
jsonpath "$.error.message" == "Rate limited"
jsonpath "$.error.data.retry_after" == 1
# ANCHOR_END: requested_error

# ANCHOR: sleep
POST {{base_url}}/jsonrpc
Content-Type: application/json
{"jsonrpc": "2.0", "method": "sleep", "params": {"ms": 200}, "id": "slow"}
HTTP 200
[Asserts]
duration >= 200
# ANCHOR_END: sleep

# ANCHOR: parse_error
POST {{base_url}}/jsonrpc
Content-Type: application/json
```
{"jsonrpc": "2.0", "method":
```
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32700
jsonpath "$.id" == null
# ANCHOR_END: parse_error
