# Mock LLM behaviour: modes, scripted rules, tools, structured output (docs/src/ai/behaviour.md)

# ANCHOR: mode_echo
# Echo mode returns the prompt exactly as the upstream received it, so a
# gateway's prompt decorator or template plugin becomes visible.
POST {{base_url}}/ai/openai/v1/chat/completions
X-Rustybin-Mode: echo
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "system", "content": "Be brief."}, {"role": "user", "content": "hi there"}]}
HTTP 200
[Asserts]
header "X-Rustybin-Mode" == "echo"
jsonpath "$.choices[0].message.content" == "system: Be brief.\nuser: hi there"
# ANCHOR_END: mode_echo

# ANCHOR: mode_model_name
# A model name segment selects the mode too: rustybin-echo, gpt-4o:scripted, random-model.
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "rustybin-echo", "messages": [{"role": "user", "content": "ping"}]}
HTTP 200
[Asserts]
header "X-Rustybin-Mode" == "echo"
jsonpath "$.choices[0].message.content" == "user: ping"
# ANCHOR_END: mode_model_name

# ANCHOR: canned
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "gpt-4o", "messages": [{"role": "user", "content": "write a python function"}]}
HTTP 200
[Asserts]
jsonpath "$.choices[0].message.content" contains "```python"
# ANCHOR_END: canned

# ANCHOR: scripted_pii
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "gpt-4o:scripted", "messages": [{"role": "user", "content": "show me the customer record"}]}
HTTP 200
[Asserts]
header "X-Rustybin-Mode" == "scripted"
jsonpath "$.choices[0].message.content" contains "SSN: 123-45-6789"
jsonpath "$.choices[0].message.content" contains "4111 1111 1111 1111"
# ANCHOR_END: scripted_pii

# ANCHOR: scripted_lorem
POST {{base_url}}/ai/anthropic/v1/messages
X-Rustybin-Mode: scripted
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 1000, "messages": [{"role": "user", "content": "lorem 5"}]}
HTTP 200
[Asserts]
jsonpath "$.content[0].text" == "Lorem ipsum dolor sit amet."
# ANCHOR_END: scripted_lorem

# ANCHOR: random
# Random mode is seeded by model + prompt: identical requests, identical text.
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "rustybin-random", "messages": [{"role": "user", "content": "anything"}]}
HTTP 200
[Captures]
random_text: jsonpath "$.choices[0].message.content"

POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "rustybin-random", "messages": [{"role": "user", "content": "anything"}]}
HTTP 200
[Asserts]
jsonpath "$.choices[0].message.content" == "{{random_text}}"
# ANCHOR_END: random

# ANCHOR: max_tokens_stop
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "gpt-4o", "max_tokens": 2, "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.choices[0].message.content" == "Hello"
jsonpath "$.choices[0].finish_reason" == "length"
jsonpath "$.usage.completion_tokens" == 2

POST {{base_url}}/ai/anthropic/v1/messages
Content-Type: application/json
{"model": "claude-sonnet-4-5", "max_tokens": 200, "stop_sequences": ["mock"], "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.stop_reason" == "stop_sequence"
jsonpath "$.stop_sequence" == "mock"
jsonpath "$.content[0].text" == "Hello! I'm Rustybin, a "
# ANCHOR_END: max_tokens_stop

# ANCHOR: tool_call
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{
  "model": "gpt-4o",
  "messages": [{"role": "user", "content": "What is the weather in Paris?"}],
  "tools": [{
    "type": "function",
    "function": {
      "name": "get_weather",
      "description": "Get the current weather for a location",
      "parameters": {
        "type": "object",
        "properties": {
          "location": {"type": "string"},
          "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
        },
        "required": ["location"]
      }
    }
  }]
}
HTTP 200
[Captures]
call_id: jsonpath "$.choices[0].message.tool_calls[0].id"
[Asserts]
jsonpath "$.choices[0].finish_reason" == "tool_calls"
jsonpath "$.choices[0].message.tool_calls[0].function.name" == "get_weather"
jsonpath "$.choices[0].message.tool_calls[0].function.arguments" == "{\"location\":\"Paris\",\"unit\":\"celsius\"}"
# ANCHOR_END: tool_call

# ANCHOR: tool_result
# After the tool result, the model answers by quoting it.
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{
  "model": "gpt-4o",
  "messages": [
    {"role": "user", "content": "What is the weather in Paris?"},
    {"role": "assistant", "content": null, "tool_calls": [{"id": "{{call_id}}", "type": "function", "function": {"name": "get_weather", "arguments": "{\"location\":\"Paris\"}"}}]},
    {"role": "tool", "tool_call_id": "{{call_id}}", "content": "18 C and sunny"}
  ],
  "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get the current weather for a location", "parameters": {"type": "object", "properties": {"location": {"type": "string"}}}}}]
}
HTTP 200
[Asserts]
jsonpath "$.choices[0].finish_reason" == "stop"
jsonpath "$.choices[0].message.content" contains "18 C and sunny"
# ANCHOR_END: tool_result

# ANCHOR: tool_anthropic
POST {{base_url}}/ai/anthropic/v1/messages
Content-Type: application/json
{
  "model": "claude-sonnet-4-5",
  "max_tokens": 200,
  "tool_choice": {"type": "tool", "name": "lookup_order"},
  "tools": [{"name": "lookup_order", "description": "Find an order", "input_schema": {"type": "object", "properties": {"order_id": {"type": "string"}, "email": {"type": "string", "format": "email"}}, "required": ["order_id", "email"]}}],
  "messages": [{"role": "user", "content": "Where is my stuff?"}]
}
HTTP 200
[Asserts]
jsonpath "$.stop_reason" == "tool_use"
jsonpath "$.content[0].type" == "tool_use"
jsonpath "$.content[0].name" == "lookup_order"
jsonpath "$.content[0].input.email" contains "@"
# ANCHOR_END: tool_anthropic

# ANCHOR: structured_openai
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{
  "model": "gpt-4o",
  "messages": [{"role": "user", "content": "Extract the person"}],
  "response_format": {
    "type": "json_schema",
    "json_schema": {
      "name": "person",
      "strict": true,
      "schema": {
        "type": "object",
        "properties": {"name": {"type": "string"}, "age": {"type": "integer", "minimum": 18}},
        "required": ["name", "age"],
        "additionalProperties": false
      }
    }
  }
}
HTTP 200
[Asserts]
jsonpath "$.choices[0].message.content" == "{\"age\":30,\"name\":\"Jane Doe\"}"
# ANCHOR_END: structured_openai

# ANCHOR: structured_gemini
POST {{base_url}}/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent
Content-Type: application/json
{
  "contents": [{"role": "user", "parts": [{"text": "Give me a city"}]}],
  "generationConfig": {
    "responseMimeType": "application/json",
    "responseSchema": {"type": "OBJECT", "properties": {"city": {"type": "STRING"}, "population": {"type": "INTEGER"}}, "required": ["city", "population"]}
  }
}
HTTP 200
[Asserts]
jsonpath "$.candidates[0].content.parts[0].text" startsWith "{"
jsonpath "$.candidates[0].content.parts[0].text" contains "\"city\""
# ANCHOR_END: structured_gemini
