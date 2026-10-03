# Mock LLM: simulated faults, latency, credential checks, request inspection (docs/src/ai/gateway-features.md)

# ANCHOR: rate_limit
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Fail: 429
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 429
[Asserts]
header "X-Rustybin-Fault" == "ai"
header "Retry-After" == "2"
header "x-ratelimit-remaining-requests" == "0"
jsonpath "$.error.code" == "rate_limit_exceeded"
# ANCHOR_END: rate_limit

# ANCHOR: overloaded
POST {{base_url}}/ai/anthropic/v1/messages
X-Rustybin-Fail: overloaded
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]}
HTTP 529
[Asserts]
jsonpath "$.type" == "error"
jsonpath "$.error.type" == "overloaded_error"
# ANCHOR_END: overloaded

# ANCHOR: gemini_quota
POST {{base_url}}/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent?fail=rate_limit
Content-Type: application/json
{"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}
HTTP 429
[Asserts]
jsonpath "$.error.status" == "RESOURCE_EXHAUSTED"
# ANCHOR_END: gemini_quota

# ANCHOR: context_length
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Fail: context_length
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 400
[Asserts]
jsonpath "$.error.code" == "context_length_exceeded"
# ANCHOR_END: context_length

# ANCHOR: content_filter
# content_filter answers 200 with the provider's filtered finish reason.
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Fail: content_filter
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
[Asserts]
jsonpath "$.choices[0].finish_reason" == "content_filter"
jsonpath "$.choices[0].message.content" == null
# ANCHOR_END: content_filter

# ANCHOR: fail_rate
# Percent suffix or ?fail_rate=: 0 never fails.
POST {{base_url}}/ai/openai/v1/chat/completions?fail=503&fail_rate=0
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
# ANCHOR_END: fail_rate

# ANCHOR: latency
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Latency-Ms: 200
X-Rustybin-TTFT-Ms: 100
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
[Asserts]
duration >= 300
# ANCHOR_END: latency

# ANCHOR: pace
# 20 tokens per second: a 10 token stream takes about half a second.
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Tokens-Per-Second: 20
Content-Type: application/json
{"model": "gpt-4o", "stream": true, "max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
[Asserts]
duration >= 400
# ANCHOR_END: pace

# ANCHOR: require_auth
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Require-Auth: true
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 401
[Asserts]
header "X-Rustybin-Credential" == "none"
jsonpath "$.error.type" == "invalid_request_error"

# A gateway injecting the key makes the same call succeed; the redacted
# credential proves which key arrived.
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Require-Auth: true
Authorization: Bearer sk-gateway-injected-1234
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
[Asserts]
header "X-Rustybin-Credential" == "bearer ****1234"
# ANCHOR_END: require_auth

# ANCHOR: native_credentials
POST {{base_url}}/ai/anthropic/v1/messages
X-Rustybin-Require-Auth: true
x-api-key: sk-ant-demo-5678
anthropic-version: 2023-06-01
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 5, "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
[Asserts]
header "X-Rustybin-Credential" == "x-api-key ****5678"

POST {{base_url}}/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent?key=AIzaDemo9999
X-Rustybin-Require-Auth: true
Content-Type: application/json
{"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}
HTTP 200
[Asserts]
header "X-Rustybin-Credential" == "query-key ****9999"

POST {{base_url}}/ai/bedrock/model/amazon.nova-pro-v1:0/converse
X-Rustybin-Require-Auth: true
X-Amz-Date: 20260101T000000Z
Authorization: AWS4-HMAC-SHA256 Credential=AKIADEMOKEY0001/20260101/us-east-1/bedrock/aws4_request, SignedHeaders=host;x-amz-date, Signature=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
Content-Type: application/json
{"messages": [{"role": "user", "content": [{"text": "hi"}]}]}
HTTP 200
[Asserts]
header "X-Rustybin-Credential" == "sigv4 ****0001"
# ANCHOR_END: native_credentials

# ANCHOR: inspect
POST {{base_url}}/ai/openai/v1/chat/completions
X-Request-Id: docs-ai-0001
Authorization: Bearer sk-secret-abcd
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "system", "content": "Injected by the gateway"}, {"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
header "X-Rustybin-Request-Id" == "docs-ai-0001"

GET {{base_url}}/ai/requests/docs-ai-0001
HTTP 200
[Asserts]
jsonpath "$.provider" == "openai"
jsonpath "$.model" == "gpt-4o"
jsonpath "$.headers.authorization" == "Bearer ****abcd"
jsonpath "$.normalized_prompt" == "system: Injected by the gateway\nuser: hello"
jsonpath "$.usage.total_tokens" > 0
jsonpath "$.captured.inspector_id" exists

GET {{base_url}}/ai/requests?limit=5
HTTP 200
[Asserts]
jsonpath "$.requests[0].request_id" == "docs-ai-0001"
# ANCHOR_END: inspect
