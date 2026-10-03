# Mock LLM: OpenAI-compatible API (docs/src/ai/providers.md)

# ANCHOR: chat
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{
  "model": "gpt-4o",
  "messages": [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "hello"}
  ]
}
HTTP 200
[Asserts]
header "X-Rustybin-Provider" == "openai"
header "X-Rustybin-Model" == "gpt-4o"
header "X-Rustybin-Mode" == "canned"
header "X-Rustybin-Credential" == "none"
header "x-ratelimit-limit-tokens" == "2000000"
jsonpath "$.object" == "chat.completion"
jsonpath "$.choices[0].message.role" == "assistant"
jsonpath "$.choices[0].message.content" startsWith "Hello! I'm Rustybin"
jsonpath "$.choices[0].finish_reason" == "stop"
jsonpath "$.usage.total_tokens" > 0
# ANCHOR_END: chat

# ANCHOR: chat_stream
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{
  "model": "gpt-4o",
  "stream": true,
  "stream_options": {"include_usage": true},
  "max_tokens": 3,
  "messages": [{"role": "user", "content": "hello"}]
}
HTTP 200
[Asserts]
header "Content-Type" contains "text/event-stream"
body contains "\"object\":\"chat.completion.chunk\""
body contains "{\"content\":\"Hell\"}"
body contains "\"finish_reason\":\"length\""
body contains "\"completion_tokens\":3"
body endsWith "data: [DONE]\n\n"
# ANCHOR_END: chat_stream

# ANCHOR: legacy_alias
# The legacy /ai/v1 prefix serves the same API.
POST {{base_url}}/ai/v1/chat/completions
Content-Type: application/json
{"model": "gpt-4o-mini", "n": 2, "messages": [{"role": "user", "content": "hi"}]}
HTTP 200
[Asserts]
jsonpath "$.choices" count == 2
# ANCHOR_END: legacy_alias

# ANCHOR: completions
POST {{base_url}}/ai/openai/v1/completions
Content-Type: application/json
{"model": "gpt-3.5-turbo-instruct", "prompt": "Say hello", "max_tokens": 4, "echo": true}
HTTP 200
[Asserts]
jsonpath "$.object" == "text_completion"
jsonpath "$.choices[0].text" startsWith "Say hello"
jsonpath "$.choices[0].finish_reason" == "length"
# ANCHOR_END: completions

# ANCHOR: responses
POST {{base_url}}/ai/openai/v1/responses
Content-Type: application/json
{"model": "gpt-4o", "instructions": "Be brief.", "input": "hello"}
HTTP 200
[Asserts]
jsonpath "$.object" == "response"
jsonpath "$.status" == "completed"
jsonpath "$.output[0].type" == "message"
jsonpath "$.output[0].content[0].type" == "output_text"
# ANCHOR_END: responses

# ANCHOR: responses_stream
POST {{base_url}}/ai/openai/v1/responses
Content-Type: application/json
{"model": "gpt-4o", "input": "hello", "stream": true, "max_output_tokens": 3}
HTTP 200
[Asserts]
body contains "event: response.created"
body contains "event: response.output_text.delta"
body contains "event: response.incomplete"
# ANCHOR_END: responses_stream

# ANCHOR: models
GET {{base_url}}/ai/openai/v1/models
HTTP 200
[Asserts]
jsonpath "$.object" == "list"
jsonpath "$.data[*].id" contains "rustybin-echo"
jsonpath "$.data[*].id" contains "text-embedding-3-small"

GET {{base_url}}/ai/openai/v1/models/no-such-model
HTTP 404
[Asserts]
jsonpath "$.error.code" == "model_not_found"
# ANCHOR_END: models

# ANCHOR: moderations
POST {{base_url}}/ai/openai/v1/moderations
Content-Type: application/json
{"model": "omni-moderation-latest", "input": "I will kill you"}
HTTP 200
[Asserts]
jsonpath "$.results[0].flagged" == true
jsonpath "$.results[0].categories.violence" == true
# ANCHOR_END: moderations

# ANCHOR: images
POST {{base_url}}/ai/openai/v1/images/generations
Content-Type: application/json
{"model": "gpt-image-1", "prompt": "a rusty bin", "response_format": "b64_json"}
HTTP 200
[Asserts]
jsonpath "$.data[0].b64_json" startsWith "iVBORw0KGgo"
# ANCHOR_END: images

# ANCHOR: transcription
POST {{base_url}}/ai/openai/v1/audio/transcriptions
[Multipart]
model: whisper-1
response_format: verbose_json
file: file,sample.wav; audio/wav
HTTP 200
[Asserts]
jsonpath "$.text" exists
jsonpath "$.segments" count >= 1
# ANCHOR_END: transcription
