# Mock A2A agents (docs/src/a2a/index.md)

# ANCHOR: directory
GET {{base_url}}/a2a
HTTP 200
[Asserts]
jsonpath "$.agents[*].id" contains "travel-planner"
jsonpath "$.agents[?(@.id == 'secure')].requiresAuth" nth 0 == true
# ANCHOR_END: directory

# ANCHOR: cards
GET {{base_url}}/.well-known/agent-card.json
HTTP 200
[Asserts]
jsonpath "$.name" == "Echo Agent"
jsonpath "$.supportedInterfaces[*].protocolBinding" contains "JSONRPC"
jsonpath "$.supportedInterfaces[*].protocolBinding" contains "HTTP+JSON"

GET {{base_url}}/a2a/weather/.well-known/agent-card.json
HTTP 200
[Asserts]
jsonpath "$.name" == "Weather Agent"
jsonpath "$.skills[0].id" == "forecast"

# Legacy v0.3 card: url + preferredTransport.
GET {{base_url}}/a2a/weather/.well-known/agent.json
HTTP 200
[Asserts]
jsonpath "$.url" == "{{base_url}}/a2a/weather"
jsonpath "$.preferredTransport" == "JSONRPC"
# ANCHOR_END: cards

# ANCHOR: jsonrpc_v1
POST {{base_url}}/a2a/echo
A2A-Version: 1.0
Content-Type: application/json
{
  "jsonrpc": "2.0", "id": 1, "method": "SendMessage",
  "params": {"message": {"messageId": "m-1", "role": "ROLE_USER", "parts": [{"text": "hello agent"}]}}
}
HTTP 200
[Captures]
echo_task: jsonpath "$.result.task.id"
[Asserts]
jsonpath "$.result.task.status.state" == "TASK_STATE_COMPLETED"
jsonpath "$.result.task.artifacts[0].parts[0].text" == "hello agent"

POST {{base_url}}/a2a/echo
A2A-Version: 1.0
Content-Type: application/json
{"jsonrpc": "2.0", "id": 2, "method": "GetTask", "params": {"id": "{{echo_task}}", "historyLength": 1}}
HTTP 200
[Asserts]
jsonpath "$.result.id" == "{{echo_task}}"
jsonpath "$.result.history" count == 1
# ANCHOR_END: jsonrpc_v1

# ANCHOR: jsonrpc_v03
# v0.3 clients send no A2A-Version header and use the old method names and shapes.
POST {{base_url}}/a2a/echo
Content-Type: application/json
{
  "jsonrpc": "2.0", "id": 1, "method": "message/send",
  "params": {"message": {"kind": "message", "messageId": "m-2", "role": "user", "parts": [{"kind": "text", "text": "hello agent"}]}}
}
HTTP 200
[Asserts]
jsonpath "$.result.kind" == "task"
jsonpath "$.result.status.state" == "completed"
# ANCHOR_END: jsonrpc_v03

# ANCHOR: version_mismatch
POST {{base_url}}/a2a/echo
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {"message": {"messageId": "m-3", "role": "ROLE_USER", "parts": [{"text": "x"}]}}}
HTTP 200
[Asserts]
jsonpath "$.error.code" == -32009
jsonpath "$.error.data[0].reason" == "VERSION_NOT_SUPPORTED"
# ANCHOR_END: version_mismatch

# ANCHOR: direct_message
POST {{base_url}}/a2a/echo
A2A-Version: 1.0
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {"message": {"messageId": "m-4", "role": "ROLE_USER", "parts": [{"text": "msg: no task please"}]}}}
HTTP 200
[Asserts]
jsonpath "$.result.message.role" == "ROLE_AGENT"
jsonpath "$.result.task" not exists
# ANCHOR_END: direct_message

# ANCHOR: rest
POST {{base_url}}/a2a/weather/v1/message:send
A2A-Version: 1.0
Content-Type: application/json
{"message": {"messageId": "w-1", "role": "ROLE_USER", "parts": [{"text": "What is the weather in Paris?"}]}}
HTTP 200
[Captures]
weather_task: jsonpath "$.task.id"
[Asserts]
jsonpath "$.task.status.state" == "TASK_STATE_COMPLETED"
jsonpath "$.task.artifacts[0].parts[0].data.city" == "Paris"
jsonpath "$.task.artifacts[0].parts[0].mediaType" == "application/json"

GET {{base_url}}/a2a/weather/v1/tasks/{{weather_task}}
A2A-Version: 1.0
HTTP 200
[Asserts]
jsonpath "$.id" == "{{weather_task}}"

GET {{base_url}}/a2a/weather/v1/tasks/no-such-task
A2A-Version: 1.0
HTTP 404
[Asserts]
jsonpath "$.error.details[0].reason" == "TASK_NOT_FOUND"
# ANCHOR_END: rest

# ANCHOR: multi_turn
POST {{base_url}}/a2a/approval/v1/message:send
A2A-Version: 1.0
Content-Type: application/json
{"message": {"messageId": "a-1", "role": "ROLE_USER", "parts": [{"text": "Approve my 120 dollar taxi expense"}]}}
HTTP 200
[Captures]
approval_task: jsonpath "$.task.id"
approval_ctx: jsonpath "$.task.contextId"
[Asserts]
jsonpath "$.task.status.state" == "TASK_STATE_INPUT_REQUIRED"

POST {{base_url}}/a2a/approval/v1/message:send
A2A-Version: 1.0
Content-Type: application/json
{"message": {"messageId": "a-2", "role": "ROLE_USER", "taskId": "{{approval_task}}", "contextId": "{{approval_ctx}}", "parts": [{"text": "approve"}]}}
HTTP 200
[Asserts]
jsonpath "$.task.status.state" == "TASK_STATE_COMPLETED"
jsonpath "$.task.artifacts[0].parts[0].data.approved" == true
# ANCHOR_END: multi_turn

# ANCHOR: failure_states
POST {{base_url}}/a2a/reject
A2A-Version: 1.0
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {"message": {"messageId": "r-1", "role": "ROLE_USER", "parts": [{"text": "book a flight"}]}}}
HTTP 200
[Asserts]
jsonpath "$.result.task.status.state" == "TASK_STATE_REJECTED"

POST {{base_url}}/a2a/flaky
A2A-Version: 1.0
Content-Type: application/json
{"jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {"message": {"messageId": "f-1", "role": "ROLE_USER", "parts": [{"text": "do something"}]}}}
HTTP 200
[Asserts]
jsonpath "$.result.task.status.state" == "TASK_STATE_FAILED"
# ANCHOR_END: failure_states

# ANCHOR: secure
POST {{base_url}}/a2a/secure/v1/message:send
A2A-Version: 1.0
Content-Type: application/json
{"message": {"messageId": "s-1", "role": "ROLE_USER", "parts": [{"text": "who am I?"}]}}
HTTP 200
[Captures]
secure_task: jsonpath "$.task.id"
[Asserts]
jsonpath "$.task.status.state" == "TASK_STATE_AUTH_REQUIRED"

POST {{base_url}}/oauth/token
[BasicAuth]
rustybin: secret
[Form]
grant_type: client_credentials
HTTP 200
[Captures]
agent_token: jsonpath "$.access_token"

POST {{base_url}}/a2a/secure/v1/message:send
A2A-Version: 1.0
Authorization: Bearer {{agent_token}}
Content-Type: application/json
{"message": {"messageId": "s-2", "role": "ROLE_USER", "taskId": "{{secure_task}}", "parts": [{"text": "who am I?"}]}}
HTTP 200
[Asserts]
jsonpath "$.task.status.state" == "TASK_STATE_COMPLETED"
# ANCHOR_END: secure

# ANCHOR: streaming
# The stream ends when the task reaches a terminal state.
POST {{base_url}}/a2a/travel-planner/v1/message:stream
A2A-Version: 1.0
Content-Type: application/json
{"message": {"messageId": "t-1", "role": "ROLE_USER", "parts": [{"text": "Plan a 3 day trip to Lisbon"}], "metadata": {"stepDelayMs": 0}}}
HTTP 200
[Asserts]
header "Content-Type" contains "text/event-stream"
body contains "TASK_STATE_SUBMITTED"
body contains "\"artifactUpdate\""
body contains "TASK_STATE_COMPLETED"
# ANCHOR_END: streaming

# ANCHOR: push
POST {{base_url}}/a2a/travel-planner/v1/message:send
A2A-Version: 1.0
Content-Type: application/json
{
  "message": {"messageId": "p-1", "role": "ROLE_USER", "parts": [{"text": "Weekend to Kyoto"}], "metadata": {"stepDelayMs": 0}},
  "configuration": {"taskPushNotificationConfig": {"url": "/a2a/webhook-sink/docs-sink", "token": "docs-token"}}
}
HTTP 200
[Asserts]
jsonpath "$.task.status.state" == "TASK_STATE_COMPLETED"

# Notifications are delivered asynchronously: poll the built-in sink.
GET {{base_url}}/a2a/webhook-sink/docs-sink
[Options]
retry: 20
retry-interval: 100
HTTP 200
[Asserts]
jsonpath "$.notifications[0].headers['x-a2a-notification-token']" == "docs-token"
jsonpath "$.notifications[*].body.statusUpdate.status.state" contains "TASK_STATE_COMPLETED"

DELETE {{base_url}}/a2a/webhook-sink/docs-sink
HTTP 200
# ANCHOR_END: push

# ANCHOR: push_ssrf
POST {{base_url}}/a2a/travel-planner/v1/message:send
A2A-Version: 1.0
Content-Type: application/json
{"message": {"messageId": "p-2", "role": "ROLE_USER", "parts": [{"text": "trip"}]}, "configuration": {"taskPushNotificationConfig": {"url": "http://169.254.169.254/latest/meta-data"}}}
HTTP 400
[Asserts]
jsonpath "$.error.status" == "INVALID_ARGUMENT"
jsonpath "$.error.message" contains "RUSTYBIN_A2A_PUSH_ALLOWLIST"
# ANCHOR_END: push_ssrf
