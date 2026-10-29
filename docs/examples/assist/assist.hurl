# AI assist (docs/src/ai-assist.md). Runs against the main documentation
# instance, which has RUSTYBIN_ASSIST_PROVIDER=mock: the built-in provider
# answers deterministically, no key and no network.

# ANCHOR: status
GET {{base_url}}/_rustybin/assist
HTTP 200
[Asserts]
jsonpath "$.enabled" == true
jsonpath "$.provider" == "mock"
jsonpath "$.model" == "rustybin-mock"
jsonpath "$.providers" count == 5
jsonpath "$.providers[0].id" == "anthropic"
jsonpath "$.providers[0].key.set" == false
jsonpath "$.providers[1].models[0].id" == "gpt-5"
jsonpath "$.providers[1].models[0].price_per_million.input" == 1.25
jsonpath "$.tasks[*].name" contains "free_answer"
jsonpath "$.tasks[*].name" contains "explain"
jsonpath "$.tasks[*].name" contains "suggest"
jsonpath "$.suggestions.mocks" count == 3
jsonpath "$.caps.calls_per_minute" == 20
jsonpath "$.privacy" contains "never leave"
# ANCHOR_END: status

# ANCHOR: ask
POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{
  "task": "free_answer",
  "inputs": { "question": "How do I see which headers my gateway added?", "view": "traffic", "context": { "captured": 3 } },
  "stream": false
}
HTTP 200
[Asserts]
jsonpath "$.task" == "free_answer"
jsonpath "$.text" contains "Mock assistant"
jsonpath "$.text" not contains "```action"
jsonpath "$.actions" count == 2
jsonpath "$.actions[0].action" == "open_view"
jsonpath "$.actions[0].payload.view" == "traffic"
jsonpath "$.actions[1].action" == "run_request"
jsonpath "$.usage.provider" == "mock"
jsonpath "$.usage.estimated_cost_usd" == 0
jsonpath "$.sent.model" == "rustybin-mock"
jsonpath "$.sent.user" contains "How do I see which headers"
jsonpath "$.sent.user" contains "\"captured\": 3"
# ANCHOR_END: ask

# ANCHOR: stream
POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
Accept: text/event-stream
{ "task": "free_answer", "inputs": { "question": "What is a request bin?", "view": "bins" } }
HTTP 200
[Asserts]
header "Content-Type" startsWith "text/event-stream"
body startsWith "event: sent"
body contains "event: delta"
body contains "event: action"
body contains "event: usage"
body contains "event: done"
# ANCHOR_END: stream

# ANCHOR: explain
POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{
  "task": "explain",
  "inputs": {
    "kind": "request",
    "subject": { "method": "GET", "path": "/anything/orders", "headers": { "X-Forwarded-For": "203.0.113.9", "X-Request-Id": "c1d2" } }
  },
  "stream": false
}
HTTP 200
[Asserts]
jsonpath "$.json.summary" contains "GET request to /anything/orders"
jsonpath "$.json.details" count >= 1
jsonpath "$.json.gateway_notes" count == 2
jsonpath "$.text" == null
# ANCHOR_END: explain

# ANCHOR: suggest
POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{ "task": "suggest", "inputs": { "view": "mocks" }, "stream": false }
HTTP 200
[Asserts]
jsonpath "$.json.suggestions" count == 3
jsonpath "$.json.suggestions[0].label" exists
jsonpath "$.json.suggestions[0].prompt" exists
jsonpath "$.usage.cached" == false

POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{ "task": "suggest", "inputs": { "view": "mocks" }, "stream": false }
HTTP 200
[Asserts]
jsonpath "$.usage.cached" == true
# ANCHOR_END: suggest

# ANCHOR: refusals
POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{ "task": "describe_everything", "inputs": {} }
HTTP 404
[Asserts]
jsonpath "$.reason" == "unknown_task"

POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{ "task": "suggest", "inputs": { "view": "nowhere" } }
HTTP 400
[Asserts]
jsonpath "$.reason" == "invalid_inputs"
# ANCHOR_END: refusals

# ANCHOR: secret
PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{ "secrets": { "leads.webhook_token": "hook-secret-k3Qx" } }
HTTP 200

POST {{base_url}}/_rustybin/assist/run
Content-Type: application/json
{ "task": "free_answer", "inputs": { "question": "Is hook-secret-k3Qx a strong token?" }, "stream": false }
HTTP 422
[Asserts]
jsonpath "$.reason" == "secret_in_inputs"
jsonpath "$.error" contains "leads.webhook_token"

DELETE {{base_url}}/_rustybin/settings/leads.webhook_token
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
# ANCHOR_END: secret

# ANCHOR: test
POST {{base_url}}/_rustybin/assist/test
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.provider" == "mock"
jsonpath "$.answer" == "OK"
jsonpath "$.latency_ms" isInteger
# ANCHOR_END: test

# ANCHOR: usage
GET {{base_url}}/_rustybin/assist/usage
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.total.calls" >= 5
jsonpath "$.by_model[0].provider" == "mock"
jsonpath "$.by_task[*].task" contains "explain"
jsonpath "$.caps.max_output_tokens" == 8192

GET {{base_url}}/_rustybin/metrics
HTTP 200
[Asserts]
body contains "rustybin_assist_calls_total{provider=\"mock\",model=\"rustybin-mock\",task=\"explain\",outcome=\"ok\"}"
body contains "rustybin_assist_tokens_total{provider=\"mock\",model=\"rustybin-mock\",direction=\"output\"}"
# ANCHOR_END: usage

# ANCHOR: settings
PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{ "values": { "assist.provider": "openai", "assist.model": "gpt-5-mini" }, "secrets": { "assist.openai_key": "sk-example-not-a-real-key" } }
HTTP 200
[Asserts]
jsonpath "$.settings[?(@.key == 'assist.openai_key')].value.set" == true
jsonpath "$.settings[?(@.key == 'assist.openai_key')].value.hint" == "-key"
body not contains "sk-example"

GET {{base_url}}/_rustybin/assist
HTTP 200
[Asserts]
jsonpath "$.enabled" == true
jsonpath "$.provider" == "openai"
jsonpath "$.model" == "gpt-5-mini"
jsonpath "$.fast_model" == "gpt-5-mini"
jsonpath "$.key_set" == true

# The provider's host is allowed under the assist purpose once a key is set.
GET {{base_url}}/_rustybin/egress
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.purposes[?(@.purpose == 'assist')].allow[0]" contains "api.openai.com"

# Back to the mock for the next examples.
DELETE {{base_url}}/_rustybin/settings/assist.provider
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
DELETE {{base_url}}/_rustybin/settings/assist.model
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
DELETE {{base_url}}/_rustybin/settings/assist.openai_key
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
# ANCHOR_END: settings
