# Saved tests on a schedule (docs/src/saved-tests.md#schedules): a schedule
# saved, listed, read, run now, a failing run whose notice the egress policy
# refuses, an invalid one, cleanup.

# ANCHOR: setup
PUT {{base_url}}/_rustybin/tests/sched-ok
Authorization: Bearer {{admin_token}}
{"request": {"path": "/status/200"}, "assert": [{"status": 200}]}
HTTP 200
# ANCHOR_END: setup

# ANCHOR: create
POST {{base_url}}/_rustybin/tests/schedules
Authorization: Bearer {{admin_token}}
{"name": "nightly", "every": "1h", "prefix": "sched-"}
HTTP 201
[Asserts]
jsonpath "$.name" == "nightly"
jsonpath "$.selection" == "tests named sched-*"
jsonpath "$.enabled" == true
jsonpath "$.status.next_due" exists
jsonpath "$.status.last_result" == null
# ANCHOR_END: create

# ANCHOR: invalid
POST {{base_url}}/_rustybin/tests/schedules
Authorization: Bearer {{admin_token}}
{"name": "too-often", "every": "1m"}
HTTP 400
[Asserts]
jsonpath "$.error" contains "5m to 24h"
# ANCHOR_END: invalid

# ANCHOR: run_now
POST {{base_url}}/_rustybin/tests/schedules/nightly/run
Authorization: Bearer {{admin_token}}
HTTP 200
[Captures]
run_id: jsonpath "$.run.run_id"
[Asserts]
jsonpath "$.result" == "passed"
jsonpath "$.run.trigger" == "schedule:nightly"
jsonpath "$.run.principal" == "scheduler"
jsonpath "$.schedule.status.last_run_id" == "{{run_id}}"
jsonpath "$.schedule.status.runs" == 1
# ANCHOR_END: run_now

# ANCHOR: list
GET {{base_url}}/_rustybin/tests/schedules
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.schedules[0].name" == "nightly"
jsonpath "$.schedules[0].status.last_result" == "passed"
jsonpath "$.max_schedules" == 20
# ANCHOR_END: list

# ANCHOR: webhook
PUT {{base_url}}/_rustybin/tests/schedules/daily
Authorization: Bearer {{admin_token}}
{
  "at": "02:30",
  "names": ["sched-fails"],
  "webhook": "https://hooks.example.com/rustybin",
  "notify_recovery": true
}
HTTP 200
[Asserts]
jsonpath "$.status.next_due" endsWith "T02:30:00Z"
# ANCHOR_END: webhook

# ANCHOR: failing_run
PUT {{base_url}}/_rustybin/tests/sched-fails
Authorization: Bearer {{admin_token}}
{"request": {"path": "/status/503"}, "assert": [{"status": 200}]}
HTTP 200

POST {{base_url}}/_rustybin/tests/schedules/daily/run
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.result" == "failed"
jsonpath "$.schedule.status.last_notice.event" == "failed"
# ANCHOR_END: failing_run

# ANCHOR: refused
GET {{base_url}}/_rustybin/tests/schedules/daily
Authorization: Bearer {{admin_token}}
[Options]
retry: 20
retry-interval: 250
HTTP 200
[Asserts]
jsonpath "$.status.last_result" == "failed"
jsonpath "$.status.last_notice.result" == "refused"
jsonpath "$.status.last_notice.detail" contains "notify"
# ANCHOR_END: refused

# ANCHOR: delete
DELETE {{base_url}}/_rustybin/tests/schedules/daily
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.deleted" == "daily"
# ANCHOR_END: delete

DELETE {{base_url}}/_rustybin/tests/schedules/nightly
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/tests/sched-ok
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/tests/sched-fails
Authorization: Bearer {{admin_token}}
HTTP 200
