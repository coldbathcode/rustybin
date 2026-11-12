# Demo storyboards (docs/src/storyboards.md): a demo written as steps,
# rehearsed step by step, reset, replaced, cleaned up.

# ANCHOR: checkpoint
POST {{base_url}}/_rustybin/checkpoints
Authorization: Bearer {{admin_token}}
{"name": "docs-storyboard-start"}
HTTP 201
# ANCHOR_END: checkpoint

# ANCHOR: save
POST {{base_url}}/_rustybin/storyboards
Authorization: Bearer {{admin_token}}
{
  "name": "docs-upstream-down",
  "title": "What the gateway does when the upstream goes down",
  "steps": [
    {
      "title": "A known starting state",
      "talk_track": "Every run of this demo starts from the **same** mocks and a healthy upstream.",
      "action": {"type": "restore_checkpoint", "name": "docs-storyboard-start"},
      "reset": {"type": "restore_checkpoint", "name": "docs-storyboard-start"}
    },
    {
      "title": "The upstream answers",
      "talk_track": "This is the health check the gateway calls.",
      "action": {"type": "send", "request": {"path": "/health"}, "assert": [{"status": 200}]}
    },
    {
      "title": "Break the upstream",
      "talk_track": "Now the upstream fails. Watch the gateway's health checks.",
      "action": {"type": "set_health", "healthy": false},
      "reset": {"type": "set_health", "healthy": true}
    },
    {
      "title": "The health check fails",
      "action": {"type": "send", "request": {"path": "/health"}, "assert": [{"status": 503}, {"json": "$.status", "equals": "unhealthy"}]}
    },
    {
      "title": "What arrived",
      "talk_track": "Every call the gateway made is in the inspector.",
      "action": {"type": "open_view", "route": "#/requests"}
    },
    {
      "title": "Questions"
    }
  ]
}
HTTP 201
[Asserts]
jsonpath "$.name" == "docs-upstream-down"
jsonpath "$.steps" count == 6
jsonpath "$.saved_at" exists
# ANCHOR_END: save

# ANCHOR: list
GET {{base_url}}/_rustybin/storyboards
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.storyboards[*].name" contains "docs-upstream-down"
jsonpath "$.max_storyboards" == 50
jsonpath "$.max_steps" == 40
# ANCHOR_END: list

# ANCHOR: step
POST {{base_url}}/_rustybin/storyboards/docs-upstream-down/steps/1/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.action" == "restore_checkpoint"
jsonpath "$.next" == 2
jsonpath "$.steps" == 6
# ANCHOR_END: step

# ANCHOR: verdicts
POST {{base_url}}/_rustybin/storyboards/docs-upstream-down/steps/2/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.summary" contains "passed"
jsonpath "$.detail.results[0].status" == 200

POST {{base_url}}/_rustybin/storyboards/docs-upstream-down/steps/3/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.detail.healthy" == false

POST {{base_url}}/_rustybin/storyboards/docs-upstream-down/steps/4/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.detail.results[0].status" == 503

POST {{base_url}}/_rustybin/storyboards/docs-upstream-down/steps/5/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.detail.open" == "#/requests"
# ANCHOR_END: verdicts

# ANCHOR: progress
GET {{base_url}}/_rustybin/storyboards/docs-upstream-down
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.progress.current" == 6
jsonpath "$.progress.done" == false
jsonpath "$.progress.results" count == 5
jsonpath "$.progress.results[2].action" == "set_health"
# ANCHOR_END: progress

# ANCHOR: failed
PUT {{base_url}}/_rustybin/storyboards/docs-wrong-guess
Authorization: Bearer {{admin_token}}
{
  "steps": [
    {"title": "This demo expects a healthy upstream", "action": {"type": "send", "request": {"path": "/health"}, "assert": [{"status": 200}]}},
    {"title": "A checkpoint nobody saved", "action": {"type": "restore_checkpoint", "name": "docs-never-saved"}}
  ]
}
HTTP 200

POST {{base_url}}/_rustybin/storyboards/docs-wrong-guess/steps/1/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == false
jsonpath "$.summary" contains "expected 200, saw 503"

POST {{base_url}}/_rustybin/storyboards/docs-wrong-guess/steps/2/run
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == false
jsonpath "$.summary" contains "no checkpoint named"
# ANCHOR_END: failed

# ANCHOR: reset
POST {{base_url}}/_rustybin/storyboards/docs-upstream-down/reset
Authorization: Bearer {{admin_token}}
X-Rustybin-Session: docs-presenter
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.reset" count == 2
jsonpath "$.reset[0].action" == "restore_checkpoint"
jsonpath "$.reset[1].action" == "set_health"

GET {{base_url}}/health
HTTP 200
# ANCHOR_END: reset

# ANCHOR: replace
PUT {{base_url}}/_rustybin/storyboards/docs-upstream-down
Authorization: Bearer {{admin_token}}
{
  "title": "A shorter demo",
  "steps": [{"title": "Only the questions"}]
}
HTTP 200
[Asserts]
jsonpath "$.steps" count == 1
# ANCHOR_END: replace

# ANCHOR: cleanup
DELETE {{base_url}}/_rustybin/storyboards/docs-upstream-down
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/storyboards/docs-wrong-guess
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/checkpoints/docs-storyboard-start
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: cleanup
