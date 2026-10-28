# Load test mode, the harness (docs/src/harness.md): the control API of one
# node in loop-through mode (roles both and coordinator, 127.0.0.1 allowed),
# started by docs/examples/run.sh as {{harness_url}}.

# ANCHOR: status
GET {{harness_url}}/_rustybin/harness
HTTP 200
[Asserts]
jsonpath "$.enabled" == true
jsonpath "$.roles" contains "client"
jsonpath "$.roles" contains "server"
jsonpath "$.allowlist" contains "127.0.0.1"
jsonpath "$.caps.max_rate" == 50000
jsonpath "$.active_run" == null
jsonpath "$.kill_switch" == false
# ANCHOR_END: status

# ANCHOR: verification
GET {{harness_url}}/.well-known/rustybin-harness
HTTP 200
[Asserts]
header "Content-Type" contains "text/plain"
body matches "^[0-9a-f]{32}$"
# ANCHOR_END: verification

# ANCHOR: schema
GET {{harness_url}}/_rustybin/harness/schema
HTTP 200
[Asserts]
jsonpath "$.properties.layers" exists
jsonpath "$.properties.faults" exists
jsonpath "$['$defs'].pattern.oneOf" count >= 10
jsonpath "$['$defs'].change" exists
jsonpath "$['x-caps'].max_rate" == 50000
# ANCHOR_END: schema

# ANCHOR: scenarios
GET {{harness_url}}/_rustybin/harness/scenarios
HTTP 200
[Asserts]
jsonpath "$.scenarios[*].name" contains "smoke"
jsonpath "$.scenarios[*].name" contains "rate-limit"
jsonpath "$.scenarios[*].name" contains "llm-streaming"
jsonpath "$.scenarios[0].summary.expected_requests" == 500

GET {{harness_url}}/_rustybin/harness/scenarios/smoke?format=yaml
HTTP 200
[Asserts]
header "Content-Type" contains "yaml"
body contains "type: constant"
# ANCHOR_END: scenarios

# ANCHOR: validate
POST {{harness_url}}/_rustybin/harness/validate
Content-Type: application/yaml
```
name: ramp-check
target: {{harness_url}}
source:
  request:
    path: /status/200
pattern:
  type: ramp
  from: 10
  to: 100
  over: 30s
thresholds:
  error_rate: 0.01
```
HTTP 200
[Asserts]
jsonpath "$.valid" == true
jsonpath "$.problems" count == 0
jsonpath "$.summary.duration_secs" == 30
jsonpath "$.summary.peak_rate" == 100
jsonpath "$.summary.expected_requests" == 1650
jsonpath "$.summary.layers[0].stages[0].type" == "ramp"
jsonpath "$.hash" matches "^[0-9a-f]{64}$"
# ANCHOR_END: validate

# ANCHOR: validate_refused
# A scenario over the node's caps is not valid; the problems say why.
POST {{harness_url}}/_rustybin/harness/validate
Content-Type: application/json
{
  "name": "too-much",
  "target": "{{harness_url}}",
  "source": {"request": {"path": "/status/200"}},
  "pattern": {"type": "constant", "rate": 100000, "for": "5s"}
}
HTTP 200
[Asserts]
jsonpath "$.valid" == false
jsonpath "$.problems[0]" contains "over the cap of 50000"

# An unknown field is a parse error.
POST {{harness_url}}/_rustybin/harness/validate
Content-Type: application/json
{"name": "typo", "target": "{{harness_url}}", "patern": {"type": "constant", "rate": 1, "for": "1s"}}
HTTP 400
[Asserts]
jsonpath "$.error" contains "unknown field `patern`"
# ANCHOR_END: validate_refused

# ANCHOR: preview
POST {{harness_url}}/_rustybin/harness/preview
Content-Type: application/json
{
  "name": "ramp-then-hold",
  "target": "{{harness_url}}",
  "source": {"request": {"path": "/status/200"}},
  "pattern": {
    "type": "ramp", "from": 10, "to": 100, "over": "30s",
    "then": {"type": "constant", "rate": 100, "for": "30s"}
  }
}
HTTP 200
[Asserts]
jsonpath "$.valid" == true
jsonpath "$.duration_secs" == 60
jsonpath "$.step_secs" == 1
jsonpath "$.points" == 60
jsonpath "$.peak_rate" == 100
jsonpath "$.total[0]" == 11.5
jsonpath "$.total[59]" == 100
jsonpath "$.layers[0].expected_requests" == 4650
jsonpath "$.layers[0].stages" count == 2
# ANCHOR_END: preview

# ANCHOR: guarded
# On an open control plane with an admin token, starting load needs it.
POST {{harness_url}}/_rustybin/harness/runs
Content-Type: application/json
{"name": "x", "target": "{{harness_url}}", "source": {"request": {"path": "/"}}, "pattern": {"type": "constant", "rate": 1, "for": "1s"}}
HTTP 401
# ANCHOR_END: guarded

# ANCHOR: run_refused
# A target outside RUSTYBIN_HARNESS_TARGETS never gets a single request.
POST {{harness_url}}/_rustybin/harness/runs
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "elsewhere",
  "target": "https://api.example.com",
  "source": {"request": {"path": "/"}},
  "pattern": {"type": "constant", "rate": 1, "for": "1s"}
}
HTTP 403
[Asserts]
jsonpath "$.error" == "a target is not allowed on this node"
jsonpath "$.problems[0]" contains "api.example.com:443 is not in RUSTYBIN_HARNESS_TARGETS"
# ANCHOR_END: run_refused

# ANCHOR: run
# Start a run: 20 requests per second to this node itself for 3 seconds.
POST {{harness_url}}/_rustybin/harness/runs
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-run",
  "target": "{{harness_url}}",
  "source": {"request": {"path": "/status/200"}},
  "pattern": {"type": "constant", "rate": 20, "for": "3s"},
  "timeout": "2s",
  "thresholds": {"error_rate": 0, "latency_p99_ms": 500, "saturated": false}
}
HTTP 202
[Captures]
run_id: jsonpath "$.run_id"
[Asserts]
header "Location" matches "^/_rustybin/harness/runs/[0-9a-f]+$"
jsonpath "$.state" matches "^(starting|running)$"
jsonpath "$.planned_secs" == 3
jsonpath "$.share" == 1
jsonpath "$.links.report" exists
# ANCHOR_END: run

# ANCHOR: run_live
GET {{harness_url}}/_rustybin/harness/runs/{{run_id}}
[Options]
delay: 1000
HTTP 200
[Asserts]
jsonpath "$.state" == "running"
jsonpath "$.sent" > 0
jsonpath "$.in_flight" >= 0
jsonpath "$.layers[0].target_now" == 20
jsonpath "$.layers[0].stage.type" == "constant"
jsonpath "$.report_ready" == false
# ANCHOR_END: run_live

# ANCHOR: run_change
# Halve the rate while it runs; the change is logged with the run time.
PATCH {{harness_url}}/_rustybin/harness/runs/{{run_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"rate_multiplier": 0.5}
HTTP 200
[Asserts]
jsonpath "$.applied" == "rate x0.5"
jsonpath "$.controls.version" == 1
jsonpath "$.controls.multiplier" == 0.5
jsonpath "$.controls.log[0].what" == "rate x0.5"
# ANCHOR_END: run_change

# ANCHOR: report_pending
GET {{harness_url}}/_rustybin/harness/runs/{{run_id}}/report
HTTP 409
[Asserts]
jsonpath "$.error" == "the run has not finished"
jsonpath "$.state" == "running"
# ANCHOR_END: report_pending

# ANCHOR: run_wait
# Poll the run until report_ready (or read the feed at .../events).
GET {{harness_url}}/_rustybin/harness/runs/{{run_id}}
[Options]
retry: 30
retry-interval: 500
HTTP 200
[Asserts]
jsonpath "$.state" == "finished"
jsonpath "$.report_ready" == true
jsonpath "$.passed" == true
# ANCHOR_END: run_wait

# ANCHOR: report
GET {{harness_url}}/_rustybin/harness/runs/{{run_id}}/report
HTTP 200
[Asserts]
jsonpath "$.state" == "finished"
jsonpath "$.passed" == true
jsonpath "$.exit_code" == 0
jsonpath "$.totals.sent" >= 40
jsonpath "$.totals.not_sent" == 0
jsonpath "$.totals.gateway" == 0
jsonpath "$.totals.lost" == 0
jsonpath "$.totals.statuses['200']" >= 40
jsonpath "$.latency.p99_ms" < 500
jsonpath "$.overhead.count" >= 40
jsonpath "$.server.arrivals" >= 40
jsonpath "$.server.changes.headers_changed" == 0
jsonpath "$.saturation.saturated" == false
jsonpath "$.thresholds" count == 3
jsonpath "$.thresholds[*].pass" not contains false
jsonpath "$.changes[0].what" == "rate x0.5"
jsonpath "$.stages[0].type" == "constant"
jsonpath "$.samples[0].request_id" startsWith "{{run_id}}-"
jsonpath "$.timeline" count >= 3
# ANCHOR_END: report

# ANCHOR: report_text
GET {{harness_url}}/_rustybin/harness/runs/{{run_id}}/report?format=text
HTTP 200
[Asserts]
header "Content-Type" contains "text/plain"
body contains "Classes:"
body contains "Latency (ms)"
body contains "Result: PASS (exit code 0)"
# ANCHOR_END: report_text

# ANCHOR: report_junit
GET {{harness_url}}/_rustybin/harness/runs/{{run_id}}/report?format=junit
HTTP 200
[Asserts]
header "Content-Type" contains "xml"
xpath "string(//testsuite/@failures)" == "0"
xpath "count(//testcase)" == 4
# ANCHOR_END: report_junit

# ANCHOR: runs
GET {{harness_url}}/_rustybin/harness/runs
HTTP 200
[Asserts]
jsonpath "$.runs[0].run_id" == "{{run_id}}"
jsonpath "$.runs[0].state" == "finished"
jsonpath "$.active" == null
# ANCHOR_END: runs

# ANCHOR: saved_scenario
PUT {{harness_url}}/_rustybin/harness/scenarios/my-smoke
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
```
name: my-smoke
target: {{harness_url}}
source:
  request:
    path: /status/200
pattern:
  type: constant
  rate: 5
  for: 2s
```
HTTP 201
[Asserts]
jsonpath "$.saved" == true
jsonpath "$.summary.expected_requests" == 10

GET {{harness_url}}/_rustybin/harness/scenarios
HTTP 200
[Asserts]
jsonpath "$.saved[0].name" == "my-smoke"

GET {{harness_url}}/_rustybin/harness/scenarios/my-smoke
HTTP 200
[Asserts]
jsonpath "$.name" == "my-smoke"
jsonpath "$.pattern.rate" == 5

DELETE {{harness_url}}/_rustybin/harness/scenarios/my-smoke
Authorization: Bearer {{admin_token}}
HTTP 204
# ANCHOR_END: saved_scenario

# ANCHOR: cluster_create
# A cluster led by this node. A node with the client or server role joins
# its own cluster, so one node is a cluster of one.
POST {{harness_url}}/_rustybin/harness/clusters
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"cluster_id": "docs-cluster", "join_token": "docs-cluster-join-token-0001"}
HTTP 201
[Asserts]
jsonpath "$.cluster_id" == "docs-cluster"
jsonpath "$.term" == 1
jsonpath "$.leader.node_id" == "docs-harness"
jsonpath "$.join_token" == "docs-cluster-join-token-0001"
jsonpath "$.members" count == 1
jsonpath "$.members[0].eligible" == true
jsonpath "$.alive.clients" == 1
jsonpath "$.alive.servers" == 1
jsonpath "$.failover" startsWith "unavailable"
jsonpath "$.links.join" == "/_rustybin/harness/clusters/docs-cluster/nodes"
# ANCHOR_END: cluster_create

# ANCHOR: cluster_leader
GET {{harness_url}}/_rustybin/harness/clusters/docs-cluster/leader
HTTP 200
[Asserts]
jsonpath "$.leader.node_id" == "docs-harness"
jsonpath "$.leader.term" == 1
jsonpath "$.answered_by" == "docs-harness"
jsonpath "$.eligible" count == 1
jsonpath "$.history[0].reason" == "created"
# ANCHOR_END: cluster_leader

# ANCHOR: cluster_upstreams
GET {{harness_url}}/_rustybin/harness/clusters/docs-cluster/upstreams
HTTP 200
[Asserts]
jsonpath "$" count == 1
jsonpath "$[0]" == "{{harness_url}}"
# ANCHOR_END: cluster_upstreams

# ANCHOR: cluster_run
# Submit (validated, not started), then start: the rate is split over the
# client nodes and every one of them starts at the same instant.
POST {{harness_url}}/_rustybin/harness/clusters/docs-cluster/runs
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "cluster-run",
  "target": "{{harness_url}}",
  "source": {"request": {"path": "/status/200"}},
  "pattern": {"type": "constant", "rate": 20, "for": "3s"},
  "scale": {"virtual_clients": 4, "client_nodes": 1, "server_nodes": 1},
  "timeout": "2s"
}
HTTP 201
[Captures]
cluster_run: jsonpath "$.run_id"
[Asserts]
jsonpath "$.state" == "created"
jsonpath "$.quorum.met" == true
jsonpath "$.quorum.need_clients" == 1

POST {{harness_url}}/_rustybin/harness/clusters/docs-cluster/runs/{{cluster_run}}/start
Authorization: Bearer {{admin_token}}
HTTP 202
[Asserts]
jsonpath "$.state" == "starting"
jsonpath "$.start_at" exists
# ANCHOR_END: cluster_run

# ANCHOR: cluster_report
GET {{harness_url}}/_rustybin/harness/clusters/docs-cluster/runs/{{cluster_run}}/report
[Options]
retry: 40
retry-interval: 500
HTTP 200
[Asserts]
jsonpath "$.state" == "finished"
jsonpath "$.passed" == true
jsonpath "$.totals.sent" >= 40
jsonpath "$.cluster.cluster_id" == "docs-cluster"
jsonpath "$.cluster.term" == 1
jsonpath "$.cluster.lost_check" == "complete"
jsonpath "$.cluster.unreported" == 0
jsonpath "$.cluster.nodes[0].share" == 1
jsonpath "$.cluster.nodes[0].state" == "finished"
jsonpath "$.cluster.history" count == 1

DELETE {{harness_url}}/_rustybin/harness/clusters/docs-cluster
Authorization: Bearer {{admin_token}}
HTTP 204
# ANCHOR_END: cluster_report

# ANCHOR: kill_switch
POST {{harness_url}}/_rustybin/harness/stop
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.kill_switch" == true
jsonpath "$.stopped" count == 0
# ANCHOR_END: kill_switch
