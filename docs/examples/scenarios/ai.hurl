# AI gateway scenarios (docs/src/gateway-scenarios.md): a model fallback run
# prepared (two targets on this instance's AI mock, and the run's profile), a
# check refused for a kind of error the scenario does not know, the AI mock
# answering everyone else as before, and the run ended.

# ANCHOR: ai_start
POST {{base_url}}/_rustybin/scenarios/ai_model_fallback/runs
Authorization: Bearer {{admin_token}}
HTTP 201
[Captures]
ai_run: jsonpath "$.id"
[Asserts]
jsonpath "$.group" == "ai"
jsonpath "$.upstream" endsWith "/ai/openai/v1/chat/completions"
jsonpath "$.second_upstream" endsWith "/ai/v1/chat/completions"
jsonpath "$.request_path" == ""
jsonpath "$.resources[0].kind" == "ai_profile"
jsonpath "$.resources[0].fail_primary" == "rate_limit"
jsonpath "$.parameters[0].name" == "fail_kind"
jsonpath "$.parameters[0].enum" count == 5
# ANCHOR_END: ai_start

# ANCHOR: ai_kind
POST {{base_url}}/_rustybin/scenarios/ai_model_fallback/runs/{{ai_run}}/check
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"gateway_url": "http://127.0.0.1:9/llm", "fail_kind": "teapot"}
HTTP 400
[Asserts]
jsonpath "$.error" contains "fail_kind: one of rate_limit, overloaded, server_error, unavailable, timeout"
# ANCHOR_END: ai_kind

# ANCHOR: ai_untouched
POST {{base_url}}/ai/openai/v1/chat/completions
Content-Type: application/json
{"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hello"}]}
HTTP 200
[Asserts]
jsonpath "$.choices[0].message.content" exists
# ANCHOR_END: ai_untouched

# ANCHOR: ai_end
DELETE {{base_url}}/_rustybin/scenarios/ai_model_fallback/runs/{{ai_run}}
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.removed[0].kind" == "ai_profile"
# ANCHOR_END: ai_end
