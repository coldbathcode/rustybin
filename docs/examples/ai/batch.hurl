# Batch APIs of the mock LLM (docs/src/ai/batch.md): an input file, a batch
# through its states, the output file, failing lines, a cancel, and the
# Anthropic shape.

# ANCHOR: upload
POST {{base_url}}/ai/openai/v1/files
X-Rustybin-Session: docs-batch
[Multipart]
purpose: batch
file: file,batch-requests.jsonl;
HTTP 200
[Captures]
file_id: jsonpath "$.id"
[Asserts]
jsonpath "$.object" == "file"
jsonpath "$.purpose" == "batch"
jsonpath "$.filename" == "batch-requests.jsonl"
# ANCHOR_END: upload

# ANCHOR: create
POST {{base_url}}/ai/openai/v1/batches
X-Rustybin-Session: docs-batch
X-Rustybin-Batch-Seconds: 1
X-Request-Id: docs-batch-1
{"input_file_id": "{{file_id}}", "endpoint": "/v1/chat/completions", "completion_window": "24h"}
HTTP 200
[Captures]
batch_id: jsonpath "$.id"
[Asserts]
jsonpath "$.object" == "batch"
jsonpath "$.status" == "validating"
jsonpath "$.request_counts.total" == 3
jsonpath "$.output_file_id" == null
# ANCHOR_END: create

# ANCHOR: poll
GET {{base_url}}/ai/openai/v1/batches/{{batch_id}}
X-Rustybin-Session: docs-batch
[Options]
retry: 20
retry-interval: 200ms
HTTP 200
[Captures]
output_file: jsonpath "$.output_file_id"
[Asserts]
jsonpath "$.status" == "completed"
jsonpath "$.request_counts.completed" == 3
jsonpath "$.request_counts.failed" == 0
jsonpath "$.completed_at" isInteger
# ANCHOR_END: poll

# ANCHOR: output
GET {{base_url}}/ai/openai/v1/files/{{output_file}}/content
X-Rustybin-Session: docs-batch
HTTP 200
[Asserts]
body contains "\"custom_id\":\"greeting\""
body contains "\"status_code\":200"
body contains "chat.completion"
# ANCHOR_END: output

# ANCHOR: failing
POST {{base_url}}/ai/openai/v1/batches
X-Rustybin-Session: docs-batch
X-Rustybin-Batch-Seconds: 0
X-Rustybin-Batch-Fail-Rate: 1
X-Rustybin-Batch-Fail: 429
{"input_file_id": "{{file_id}}", "endpoint": "/v1/chat/completions", "completion_window": "24h"}
HTTP 200
[Captures]
error_file: jsonpath "$.error_file_id"
[Asserts]
jsonpath "$.status" == "completed"
jsonpath "$.request_counts.failed" == 3
jsonpath "$.output_file_id" == null

GET {{base_url}}/ai/openai/v1/files/{{error_file}}/content
X-Rustybin-Session: docs-batch
HTTP 200
[Asserts]
body contains "\"status_code\":429"
body contains "Rate limit reached"
# ANCHOR_END: failing

# ANCHOR: cancel
POST {{base_url}}/ai/openai/v1/batches
X-Rustybin-Session: docs-batch
X-Rustybin-Batch-Seconds: 2
{"input_file_id": "{{file_id}}", "endpoint": "/v1/chat/completions", "completion_window": "24h"}
HTTP 200
[Captures]
slow_batch: jsonpath "$.id"

POST {{base_url}}/ai/openai/v1/batches/{{slow_batch}}/cancel
X-Rustybin-Session: docs-batch
HTTP 200
[Asserts]
jsonpath "$.status" == "cancelling"

GET {{base_url}}/ai/openai/v1/batches/{{slow_batch}}
X-Rustybin-Session: docs-batch
[Options]
retry: 20
retry-interval: 100ms
HTTP 200
[Asserts]
jsonpath "$.status" == "cancelled"
jsonpath "$.cancelled_at" isInteger
# ANCHOR_END: cancel

# ANCHOR: log
GET {{base_url}}/ai/requests/docs-batch-1?session=docs-batch
HTTP 200
[Asserts]
jsonpath "$.kind" == "batch"
jsonpath "$.detail.batch_id" == "{{batch_id}}"
jsonpath "$.detail.lines" count == 3
jsonpath "$.detail.lines[0].custom_id" == "greeting"
# ANCHOR_END: log

# ANCHOR: anthropic
POST {{base_url}}/ai/anthropic/v1/messages/batches
X-Rustybin-Session: docs-batch
X-Rustybin-Batch-Seconds: 0
{
  "requests": [
    {"custom_id": "first", "params": {"model": "claude-sonnet-4-5", "max_tokens": 64, "messages": [{"role": "user", "content": "hello"}]}},
    {"custom_id": "second", "params": {"model": "claude-sonnet-4-5", "messages": [{"role": "user", "content": "no max_tokens"}]}}
  ]
}
HTTP 200
[Captures]
message_batch: jsonpath "$.id"
[Asserts]
jsonpath "$.type" == "message_batch"
jsonpath "$.processing_status" == "ended"
jsonpath "$.request_counts.succeeded" == 1
jsonpath "$.request_counts.errored" == 1

GET {{base_url}}/ai/anthropic/v1/messages/batches/{{message_batch}}/results
X-Rustybin-Session: docs-batch
HTTP 200
[Asserts]
body contains "\"type\":\"succeeded\""
body contains "\"type\":\"errored\""
body contains "invalid_request_error"
# ANCHOR_END: anthropic

# ANCHOR: cleanup
DELETE {{base_url}}/ai/openai/v1/files/{{file_id}}
X-Rustybin-Session: docs-batch
HTTP 200
[Asserts]
jsonpath "$.deleted" == true
# ANCHOR_END: cleanup
