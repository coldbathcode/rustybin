# Mock LLM: other providers (docs/src/ai/providers.md)

# ANCHOR: anthropic
POST {{base_url}}/ai/anthropic/v1/messages
anthropic-version: 2023-06-01
Content-Type: application/json
{
  "model": "claude-sonnet-4-5",
  "max_tokens": 256,
  "system": "You are a helpful assistant.",
  "messages": [{"role": "user", "content": "hello"}]
}
HTTP 200
[Asserts]
header "X-Rustybin-Provider" == "anthropic"
header "request-id" startsWith "req_"
header "anthropic-ratelimit-requests-limit" == "4000"
jsonpath "$.type" == "message"
jsonpath "$.content[0].type" == "text"
jsonpath "$.stop_reason" == "end_turn"
jsonpath "$.usage.input_tokens" > 0
# ANCHOR_END: anthropic

# ANCHOR: anthropic_stream
POST {{base_url}}/ai/anthropic/v1/messages
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 3, "stream": true, "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
body contains "event: message_start"
body contains "event: content_block_delta"
body contains "\"type\":\"text_delta\""
body contains "event: message_stop"
# ANCHOR_END: anthropic_stream

# ANCHOR: anthropic_cache
# Prompt caching: the first request creates the cache entry, a repeat reads it.
POST {{base_url}}/ai/anthropic/v1/messages
X-Rustybin-Session: docs-cache
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 20, "system": [{"type": "text", "text": "Long shared instructions for the docs cache demo.", "cache_control": {"type": "ephemeral"}}], "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.usage.cache_creation_input_tokens" > 0
jsonpath "$.usage.cache_read_input_tokens" == 0

POST {{base_url}}/ai/anthropic/v1/messages
X-Rustybin-Session: docs-cache
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 20, "system": [{"type": "text", "text": "Long shared instructions for the docs cache demo.", "cache_control": {"type": "ephemeral"}}], "messages": [{"role": "user", "content": "hello again"}]}
HTTP 200
[Asserts]
jsonpath "$.usage.cache_creation_input_tokens" == 0
jsonpath "$.usage.cache_read_input_tokens" > 0
# ANCHOR_END: anthropic_cache

# ANCHOR: anthropic_errors
# max_tokens is required, like the real API.
POST {{base_url}}/ai/anthropic/v1/messages
Content-Type: application/json
{"model": "claude-sonnet-4-5", "messages": [{"role": "user", "content": "hello"}]}
HTTP 400
[Asserts]
jsonpath "$.type" == "error"
jsonpath "$.error.type" == "invalid_request_error"

POST {{base_url}}/ai/anthropic/v1/messages/count_tokens
Content-Type: application/json
{"model": "claude-sonnet-4-5", "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.input_tokens" > 0
# ANCHOR_END: anthropic_errors

# ANCHOR: gemini
POST {{base_url}}/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent
x-goog-api-key: gemini-demo-key
Content-Type: application/json
{
  "systemInstruction": {"parts": [{"text": "Be brief."}]},
  "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
  "generationConfig": {"maxOutputTokens": 5}
}
HTTP 200
[Asserts]
header "X-Rustybin-Provider" == "gemini"
header "X-Rustybin-Credential" == "x-goog-api-key ****-key"
jsonpath "$.candidates[0].content.role" == "model"
jsonpath "$.candidates[0].finishReason" == "MAX_TOKENS"
jsonpath "$.usageMetadata.candidatesTokenCount" == 5
# ANCHOR_END: gemini

# ANCHOR: gemini_stream
POST {{base_url}}/ai/gemini/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse
Content-Type: application/json
{"contents": [{"role": "user", "parts": [{"text": "hello"}]}], "generationConfig": {"maxOutputTokens": 3}}
HTTP 200
[Asserts]
header "Content-Type" contains "text/event-stream"
body startsWith "data: {\"candidates\""
# ANCHOR_END: gemini_stream

# ANCHOR: gemini_models
GET {{base_url}}/ai/gemini/v1beta/models
HTTP 200
[Asserts]
jsonpath "$.models[*].name" contains "models/gemini-2.5-flash"

POST {{base_url}}/ai/gemini/v1beta/models/gemini-2.5-flash:countTokens
Content-Type: application/json
{"contents": [{"role": "user", "parts": [{"text": "hello"}]}]}
HTTP 200
[Asserts]
jsonpath "$.totalTokens" > 0
# ANCHOR_END: gemini_models

# ANCHOR: bedrock
POST {{base_url}}/ai/bedrock/model/anthropic.claude-3-5-sonnet-20240620-v1:0/converse
Content-Type: application/json
{
  "system": [{"text": "Be brief."}],
  "messages": [{"role": "user", "content": [{"text": "hello"}]}],
  "inferenceConfig": {"maxTokens": 5}
}
HTTP 200
[Asserts]
header "X-Rustybin-Provider" == "bedrock"
header "x-amzn-requestid" exists
jsonpath "$.output.message.role" == "assistant"
jsonpath "$.stopReason" == "max_tokens"
jsonpath "$.usage.outputTokens" == 5
# ANCHOR_END: bedrock

# ANCHOR: bedrock_stream
POST {{base_url}}/ai/bedrock/model/amazon.nova-pro-v1:0/converse-stream
Content-Type: application/json
{"messages": [{"role": "user", "content": [{"text": "hello"}]}], "inferenceConfig": {"maxTokens": 3}}
HTTP 200
[Asserts]
header "Content-Type" == "application/vnd.amazon.eventstream"
# Binary AWS event stream frames: ":event-type" headers, a messageStop event.
bytes contains hex,3a6576656e742d74797065;
bytes contains hex,6d65737361676553746f70;
# ANCHOR_END: bedrock_stream

# ANCHOR: bedrock_invoke
POST {{base_url}}/ai/bedrock/model/anthropic.claude-3-haiku-20240307-v1:0/invoke
Content-Type: application/json
{"anthropic_version": "bedrock-2023-05-31", "max_tokens": 20, "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.type" == "message"

POST {{base_url}}/ai/bedrock/model/amazon.titan-embed-text-v2:0/invoke
Content-Type: application/json
{"inputText": "Hello world", "dimensions": 256}
HTTP 200
[Asserts]
jsonpath "$.embedding" count == 256
# ANCHOR_END: bedrock_invoke

# ANCHOR: ollama
POST {{base_url}}/ai/ollama/api/chat
Content-Type: application/json
{"model": "llama3.2", "messages": [{"role": "user", "content": "hello"}], "stream": false}
HTTP 200
[Asserts]
jsonpath "$.done" == true
jsonpath "$.message.role" == "assistant"

# Streaming is on by default: newline-delimited JSON.
POST {{base_url}}/ai/ollama/api/generate
Content-Type: application/json
{"model": "llama3.2", "prompt": "hi", "options": {"num_predict": 2}}
HTTP 200
[Asserts]
header "Content-Type" contains "application/x-ndjson"
body contains "\"done\":false"
body contains "\"done_reason\":\"length\""
# ANCHOR_END: ollama

# ANCHOR: azure
POST {{base_url}}/ai/azure/openai/deployments/gpt-4o/chat/completions?api-version=2024-10-21
api-key: azure-demo-key
Content-Type: application/json
{"messages": [{"role": "user", "content": "hello"}], "max_tokens": 3}
HTTP 200
[Asserts]
header "X-Rustybin-Provider" == "azure"
header "X-Rustybin-Credential" == "api-key ****-key"
jsonpath "$.model" == "gpt-4o"
jsonpath "$.prompt_filter_results[0].content_filter_results.hate.filtered" == false
jsonpath "$.choices[0].content_filter_results.violence.severity" == "safe"

POST {{base_url}}/ai/azure/openai/deployments/gpt-4o/chat/completions
Content-Type: application/json
{"messages": [{"role": "user", "content": "hello"}]}
HTTP 400
[Asserts]
jsonpath "$.error.message" contains "api-version"
# ANCHOR_END: azure
