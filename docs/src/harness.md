# Load testing through a gateway

Load test mode, the *harness*, makes Rustybin the load client as well as the upstream:
one node sends a scenario's traffic through the gateway under test and the same node (or
other nodes) answers behind it, so the report can tell what the gateway did from what the
upstream did, request by request. It is off by default and never on in public mode. This
page is the guide; the reference tables (every variable, pattern, test, pool and fault)
are in [configuration](configuration.md#harness-mode), and the design in
`docs/plans/harness.md`.

What you get:

- **Scenarios**: targets, traffic layers with run patterns (constant, ramp, step, spike,
  pulse, burst, wave, the closed loops `target` and `breakpoint`, chained with `then:`),
  open (requests per second) or closed (users with think time) load, request templates,
  traffic from an [OpenAPI mock](mocks/index.md), LLM streams, SSE, WebSocket, gRPC and
  MCP calls, identities (API keys, minted JWTs, OAuth clients, forwarded addresses), fault
  presets and thresholds. One JSON Schema drives the API, the
  [management MCP](agent-mcp.md) and the [console](console.md) builder.
- **Correlation**: every request is signed and tagged; the report classes every answer as
  `upstream`, `gateway` (made by the gateway), `transport` or `lost`, measures the
  gateway's overhead, and lists what it changed on the way.
- **Rate-limit tests** against the limiter in front: window edges, distributed counters,
  many keys and IPs, retries, a limit probe, with an analysis in words and numbers.
- **A cluster**: client nodes split the rate, server nodes answer, one report; the leader
  can fail over without a gap in the load.
- **Live tuning** of a running run, a feed, reports as JSON, text and JUnit, headless runs
  with exit codes for CI.

## Turn it on

```bash
RUSTYBIN_HARNESS=on RUSTYBIN_HARNESS_ROLES=both \
RUSTYBIN_HARNESS_TARGETS=gateway.internal,10.0.0.0/8 rustybin
```

`RUSTYBIN_HARNESS_ROLES` is `client` (sends load), `server` (answers behind the gateway),
`both` (loop-through: the node sends to a gateway that routes back to it, the one-box
demo) or `coordinator` (leads a cluster), comma separated. A client needs
`RUSTYBIN_HARNESS_TARGETS`: the hosts, domains, addresses and CIDRs it may send load to,
and nothing else; names are resolved once per run and the connections pinned. Starting
with `RUSTYBIN_HARNESS=on` in public mode, with self-service mocks, or as a client with no
targets is a fatal error.

The control routes live under `/_rustybin/harness`. Reads are open like the rest of the
control plane; starting, changing and stopping runs follows the admin token on an open
control plane and needs the `harness` (or `admin`) scope on a protected one
([control-plane security](concepts/control-plane-security.md)).

```hurl
{{#include ../examples/harness/api.hurl:status}}
```

With `RUSTYBIN_HARNESS_VERIFY_TARGETS=true` (always on in hosted mode) every target must
serve the node's token at this path before a run starts; a gateway that routes to the node
passes, which also proves the route before any load is sent:

```hurl
{{#include ../examples/harness/api.hurl:verification}}
```

## A scenario

A scenario is YAML or JSON. The smallest one names a target, a request and a pattern:

```yaml
{{#include ../../src/harness/scenarios/smoke.yaml}}
```

The full shape has `targets` with weights, `scale` (virtual clients, connections, the
cluster's node counts), `layers` (each with a `source`, a `pattern`, a `model`,
`identities`, `expect` and `thresholds`), a `timeout`, run-level `thresholds`, a `servers`
profile for the shaped upstream at `/harness/*`, and `faults`. A request template takes
`method`, `path`, `headers`, `query` and `body` with `{{seq}}` (the request number),
`{{vu}}` (the virtual client), `{{id}}` (the identity) and `{{ip}}` (a forwarded address),
and `requests` is a weighted list of them. `GET /_rustybin/harness/schema` is the JSON
Schema with the node's caps as bounds, and `GET /_rustybin/harness/scenarios` lists the
built-ins (`smoke`, `ramp-to-target`, `soak`, `closed-users`, `step-plateaus`,
`breakpoint`, `spike-and-recover`, `pulse`, `burst`, `daily-wave`, `rate-limit`,
`limit-probe`, `many-ips`, `petstore-customers`, `llm-streaming`, `flaky-upstream`,
`jittery-clients`) to copy:

```hurl
{{#include ../examples/harness/api.hurl:schema}}
```

```hurl
{{#include ../examples/harness/api.hurl:scenarios}}
```

Validate a scenario before running it. The answer normalises it, hashes it (the hash is in
every report) and summarises the layers, stages, peak rate and expected requests:

```hurl
{{#include ../examples/harness/api.hurl:validate}}
```

```hurl
{{#include ../examples/harness/api.hurl:validate_refused}}
```

A preview is the pattern math without sending anything: the expected requests per second
of every layer and in total, one point per second (coarser past an hour), which the console
draws as you edit:

```hurl
{{#include ../examples/harness/api.hurl:preview}}
```

Save a scenario by name next to the built-ins (validated; kept in
`RUSTYBIN_HARNESS_STATE_DIR` when set):

```hurl
{{#include ../examples/harness/api.hurl:saved_scenario}}
```

## Run it

`POST /_rustybin/harness/runs` validates the scenario, pins (and when configured verifies)
its targets and starts sending. One run at a time per node.

```hurl
{{#include ../examples/harness/api.hurl:run}}
```

```hurl
{{#include ../examples/harness/api.hurl:guarded}}
```

```hurl
{{#include ../examples/harness/api.hurl:run_refused}}
```

While it runs, `GET .../runs/<run>` is the live view (counts, classes, latency so far, the
last second, every layer's stage and target rate, saturation), and
`GET .../runs/<run>/events` is the same as a feed of server-sent events: a `snapshot`
every second, `change`, `finding` and `finished`.

```hurl
{{#include ../examples/harness/api.hurl:run_live}}
```

Change it without stopping ([live tuning](#live-tuning) below):

```hurl
{{#include ../examples/harness/api.hurl:run_change}}
```

The report exists once the run has drained:

```hurl
{{#include ../examples/harness/api.hurl:report_pending}}
```

```hurl
{{#include ../examples/harness/api.hurl:run_wait}}
```

```hurl
{{#include ../examples/harness/api.hurl:report}}
```

The same report as text (`?format=text`) and as JUnit (`?format=junit`, one test case per
threshold, for CI):

```hurl
{{#include ../examples/harness/api.hurl:report_text}}
```

```hurl
{{#include ../examples/harness/api.hurl:report_junit}}
```

```hurl
{{#include ../examples/harness/api.hurl:runs}}
```

A node keeps its last runs in memory (and every report in
`RUSTYBIN_HARNESS_REPORT_DIR` when set). The kill switch stops all load on the node at
once, whatever started it:

```hurl
{{#include ../examples/harness/api.hurl:kill_switch}}
```

### From the command line

The same engine runs as a command. `--local-server` starts an in-process Rustybin, loops
the scenario through it and allows it as a target, which is the quickest way to try a
scenario or to smoke test a build:

```bash
{{#include ../examples/harness/cli.sh:validate}}
```

```bash
{{#include ../examples/harness/cli.sh:run_local}}
```

```bash
{{#include ../examples/harness/cli.sh:report}}
```

`rustybin harness run scenario.yaml --target https://gateway.example.com --targets
gateway.example.com` sends to a real gateway (the allowlist comes from `--targets` or
`RUSTYBIN_HARNESS_TARGETS`); `--max-rate`, `--max-inflight`, `--max-duration` and
`--threads` override the caps, `--verify` requires the verification token, `--format`
picks text, JSON or JUnit. The exit code is the report's: 0 when the thresholds pass, 1
when they fail, 2 on an error.

### Headless, from the environment

A node can run one scenario at startup and exit with its code, which fits a job in a
pipeline or a scheduler:

```bash
RUSTYBIN_HARNESS=on RUSTYBIN_HARNESS_ROLES=client RUSTYBIN_HARNESS_TARGETS=gateway.internal \
RUSTYBIN_HARNESS_SCENARIO=gateway-load.yaml RUSTYBIN_HARNESS_START=immediately \
RUSTYBIN_HARNESS_REPORT=report.json RUSTYBIN_HARNESS_EXIT=true rustybin
```

`RUSTYBIN_HARNESS_REPORT=-` prints the JSON report on stdout. On a coordinator,
`RUSTYBIN_HARNESS_START=on-quorum` waits until the nodes the scenario asks for have
joined ([a cluster](#a-cluster) below).

### From an agent or the console

The [management MCP server](agent-mcp.md#harness) has a tool per step
(`harness_status`, `validate_scenario`, `preview_scenario`, `create_run`, `get_run`,
`update_run`, `get_run_report`, the cluster tools) and a prompt, `load_test_gateway`,
that walks an agent from the allowlist to a dry run and the real one; starting load always
asks for confirmation. The [console](console.md)'s Harness view has the scenario builder
generated from the schema with a preview chart, the live run with its charts and
controls, the cluster and leader cards, and the report.

## What the report says

Every answer is in one class, and the classes add up to what was sent:

| Class | Meaning |
|---|---|
| `upstream` | The answer came from a Rustybin server node (it carries the served-by tag and `Server-Timing`); its status is whatever the upstream sent, so a 4xx here is the upstream's |
| `gateway` | The answer did not come from the upstream: the gateway made it (a 429, a 503, an auth refusal, a cached answer); `gateway_statuses` counts them by status and `limited` is the 429s |
| `transport` | A connection error or a timeout, by kind; `transport_after_upstream` says how many of them the upstream had seen anyway |
| `lost` | Sent, never answered, and the server side never saw it: the request died inside the gateway |
| `not_sent` | The client could not send on time (saturation), never counted against the gateway |
| `unattributed` | Answers that cannot carry the tag (gRPC, a WebSocket upgrade outside a Rustybin server's layer) |
| `abandoned` | Requests the scenario's faults cancelled on purpose |

Latency comes in several measures: from the intended send time (coordinated omission
corrected: a late send counts the delay), from the actual send, the upstream's own
processing time (`Server-Timing`), and `overhead`, the difference, which is the gateway's
cost per request; in loop-through mode the request and response legs are measured on one
clock. The server side counts arrivals by node, route and layer, what the gateway changed
(headers added, removed and rewritten, bodies, paths) and how many captures were sampled
into the [inspector](concepts/inspector.md). `saturation` says whether the client itself
was the bottleneck (in-flight cap, scheduler lag, CPU) so a slow number is never blamed on
the gateway by mistake. Then the timeline per second, every stage as it ran, the live
changes, the findings of the closed loops, the thresholds with their measured values,
breakdowns by layer, target, operation and credential, the rate-limit analysis, stream
statistics, and samples (the slowest, one of every class) with their `X-Request-Id` and
`traceparent` to look up in the gateway's logs.

## Patterns

A layer's `pattern` says how hard it pushes over time. Every pattern of the
[table](configuration.md#harness-mode) takes `jitter` and `sync`, and chains with `then:`.
The built-in `ramp-to-target` ramps, holds and keeps a probe alongside:

```yaml
{{#include ../../src/harness/scenarios/ramp-to-target.yaml}}
```

`step` climbs plateaus and reports each one; `spike` rises, holds and falls then measures
the recovery; `pulse` and `burst` make bursts land on the gateway at once from every
client node (`sync: cluster`, the default) or spread (`sync: node`); `wave` follows a
daily curve. Two patterns are closed loops: `target` with `overhead_p99_ms` moves the rate
every second toward a gateway overhead goal, and `breakpoint` grows the rate plateau by
plateau until an `until` condition (error rate, latency, overhead) fails and reports the
last good rate:

```yaml
{{#include ../../src/harness/scenarios/breakpoint.yaml}}
```

A closed `model: closed` layer runs `users` with a `think_time` instead of a rate; the
report then shows users and the rate they produced.

## Rate-limit tests

A layer's `expect` describes what the gateway's limiter should do with its traffic, and
the `limit_test` pattern probes it for a known weak spot. The built-in `rate-limit`
scenario sends bursts around the window boundaries of a limit of 100 per minute next to
legitimate customers that must never be refused:

```yaml
{{#include ../../src/harness/scenarios/rate-limit.yaml}}
```

The tests are `over_limit`, `window_edge`, `just_under`, `distributed` (from every client
node at once, for counters local to each gateway node), `many_keys`, `many_ips`,
`retry_wave`, `concurrency`, `slow_rise` and `limit_probe` (a binary search for the
effective limit when nobody wrote it down; the built-in `limit-probe` scenario). The
report's `rate_limits` section has, per layer: admitted, refused and failed requests;
admission per window and in the worst window-long span, as over- or under-admission
against `expect`; the time to the first refusal and the requests admitted before it; the
recovery; the window as measured; breakdowns per identity and per gateway node; the
rate-limit headers (`RateLimit`, `RateLimit-Policy`, `X-RateLimit-*`, `Retry-After`) and
whether they say what `expect` says; upstream protection (what reached the server side
against what was admitted); collateral damage on the other layers; and findings in words,
such as "the fixed window admitted 196 of 100 across the boundary at 0:59". A layer's
`thresholds` (`rate_limited`, `over_admission`, `under_admission`) fail the run like the
run's own.

## Traffic from mocks, identities, protocols and faults

A layer's `source` can be an [OpenAPI mock](mocks/index.md) deployed on the client node
(`source: {mock: petstore}`): every operation with generated parameters and bodies,
weights per operation, an `invalid_fraction` of requests that expect a 4xx (the report
says how many got through anyway), and `stateful: true` for create-then-read pairs. The
built-in `petstore-customers` scenario is the example. `identities` give each virtual
client a credential: `api_keys` (listed or minted by name), `jwt` (minted by the node's
built-in [identity provider](reference/oidc.md), RS256 with its JWKS or the HS256 demo
secret), `oauth` (client credentials against the gateway's token endpoint, fetched before
the run), `forwarded` (addresses in `X-Forwarded-For`) and `source_addresses`; the report
breaks the answers down by operation and by credential label, never showing a secret.

`llm` sources stream chat completions from the [mock LLM](ai/index.md) through the gateway
and measure the time to the first token, the gaps between tokens and tokens per second
(the built-in `llm-streaming`); `sse`, `websocket`, `grpc` and `mcp` do the same for
event streams, sessions, unary calls and tool calls, in the report's `streams` section.

The `faults` block (and the live change `faults`) injects what real clients and upstreams
do: a `preset` (`calm`, `jittery`, `flaky-upstream`, `retries`, `synchronised`, `churn`,
`all`) plus overrides. Client faults are drawn per request by fraction: connection churn,
abandoned requests, slow bodies, malformed JSON, wrong content types, oversized headers,
retries on 5xx (immediate, or synchronised across the nodes to the next whole second),
expired tokens and an HTTP/2 mix. Server faults travel to the server nodes with the run
and act on the run's own requests at `/harness/*` and on `/health`: drops after the
headers, stalls, a flapping health check and node churn, where the server nodes go down in
turn, which exercises the gateway's health checks and circuit breakers. The built-ins
`flaky-upstream` and `jittery-clients` show them:

```yaml
{{#include ../../src/harness/scenarios/flaky-upstream.yaml}}
```

The tables of every parameter are in [configuration](configuration.md#harness-mode).

## Live tuning

`PATCH .../runs/<run>` changes a running run: `rate_multiplier` (0 to 10, the run or one
`layer`), `pause` and `resume`, `hold` (freeze the pattern where it is) and `false` to
release it, `skip_stage`, `set` (`{"<layer>.pattern.<field>": value}`, `then.` for the
next stage), `add_layer`, `remove_layer` and `faults`. A change is checked like a new
scenario (caps included), applied on the scheduler's next pass, on every client node of a
cluster at once, logged with the run time it was made at, and carried in the cluster's
snapshot, so it survives a failover. The report lists the changes and every stage as it
ran, so a hold or a skip is visible next to its numbers.

## A cluster

One node sends what one machine can. A cluster spreads a run over client nodes, puts
server nodes behind the gateway, and merges one report. Nodes pull: they join a leader and
heartbeat it (a long poll carrying their stats and clock stamps, returning assignments),
so nodes behind NAT or in other networks work, and the leader never has to reach them.

- The **leader** is the node a cluster was created on (`RUSTYBIN_HARNESS_ROLES=coordinator`
  with `RUSTYBIN_HARNESS_CLUSTER` and `RUSTYBIN_HARNESS_JOIN_TOKEN`, or
  `POST /_rustybin/harness/clusters`). It validates and splits a run by the clients'
  declared capacity, starts every client at the same instant on its clock, merges their
  histograms exactly (no averaged percentiles), and asks the server nodes after the run
  which failed requests they saw, so `lost` is exact across the cluster.
- **Client** and **server** nodes join with the token (`RUSTYBIN_HARNESS_JOIN=<leader>`,
  or `POST /_rustybin/harness/join`). A client checks every assignment against its own
  caps and its own allowlist, so a leader cannot aim it anywhere its operator did not
  allow, and stops sending when no leader has renewed its lease (5 s by default).
- `GET .../clusters/<id>/upstreams` lists the server nodes' advertised URLs
  (`RUSTYBIN_HARNESS_ADVERTISE`) for the gateway's upstream pool.
- A run asks for its nodes with `scale.client_nodes` and `scale.server_nodes`; starting
  waits for that quorum (409 without it). `client_nodes: auto` lets the leader size the
  run from the clients' capacity and emit a signed `scale_request` to
  `RUSTYBIN_HARNESS_SCALE_HOOK` when more are needed; Rustybin never starts machines.

A node with the client or server role and the coordinator role joins its own cluster, so
the cluster API works on one node too:

```hurl
{{#include ../examples/harness/api.hurl:cluster_create}}
```

```hurl
{{#include ../examples/harness/api.hurl:cluster_leader}}
```

```hurl
{{#include ../examples/harness/api.hurl:cluster_upstreams}}
```

```hurl
{{#include ../examples/harness/api.hurl:cluster_run}}
```

```hurl
{{#include ../examples/harness/api.hurl:cluster_report}}
```

The report's `cluster` section has every node with its share, its range of virtual
clients, what it sent and saw, its clock offset and error bound and its start lag, the
`lost_check`, any `unreported` bound and the history of leaders. Controller routes asked
on a member that does not lead answer `307` to the leader with
`X-Rustybin-Harness-Leader`, and `GET .../leader` is answered by every member, so a
controller can talk to any node it knows.

### The compose example

`docs/examples/harness/cluster/` runs a whole cluster on one machine: a leader, a standby,
two client nodes, two server nodes and a stand-in gateway (a plain reverse proxy over the
two servers, `gateway.conf`; put the gateway under test there and point it at the
upstream export).

```bash
cd docs/examples/harness/cluster
docker compose up --build --abort-on-container-exit --exit-code-from leader
```

The leader creates `perf-1` at startup, waits for the two clients and two servers
(`RUSTYBIN_HARNESS_START=on-quorum`), runs `gateway-load.yaml` through the gateway, prints
the report and exits with its code. The scenario:

```yaml
{{#include ../examples/harness/cluster/gateway-load.yaml}}
```

The compose file:

```yaml
{{#include ../examples/harness/cluster/compose.yaml}}
```

## Failover

Nodes with `RUSTYBIN_HARNESS_CONTROL_URL` are eligible to lead; `RUSTYBIN_HARNESS_LEADER=prefer`
makes a standby that is first in the succession whatever its work, and a coordinator-only
node with a control URL and `RUSTYBIN_HARNESS_JOIN` joins as one. The leader replicates a
versioned snapshot (who leads, the term, the succession; to eligible nodes also the members,
the runs with a checkpoint of their stats every 5 s, and the secrets). When the leader has
not answered for the election timeout (`RUSTYBIN_HARNESS_ELECTION_TIMEOUT`, 3 s, below
the lease), the first node in the succession stands for election and needs the votes of a
majority of the eligible nodes, so failover needs three of them; `GET .../leader` says
whether it is `available`. The new leader takes over from the snapshot within the lease, so
the clients never stop; they report their cumulative stats to it again, which fills the
gap exactly. Whatever the old leader's own client part may have sent after its last
checkpoint is reported as `unreported`, an upper bound, never dropped silently. A leader
that cannot hear a majority stops renewing leases (the minority side of a partition stops
sending), and one that hears a newer term steps down. A crashed leader that restarts is a
new node: start it as a standby pointing at any eligible member to rejoin.

The compose cluster has three eligible nodes (the leader, the standby and `client-1`).
`conformance/harness/run.sh` starts it, kills the leader ten seconds into the run and
asserts the report the standby produces: the standby leads the new term, both clients finished
with their half of the rate, no second of the timeline is empty, `lost` and `unreported`
are 0, the thresholds pass. Run it, or kill the leader yourself:

```bash
docker compose kill leader
curl -s localhost:18901/_rustybin/harness/clusters/perf-1/leader | jq '.leader, .history'
```

## Safety

Load only goes where the operator said: the allowlist (`RUSTYBIN_HARNESS_TARGETS`),
checked by name and by every resolved address, with link-local, cloud metadata, multicast
and the node's own control plane always refused; pinned connections, so a DNS change
during a run cannot redirect it; optional target verification through
`/.well-known/rustybin-harness`; caps per node on rate, requests in flight and duration;
one run at a time; a lease that stops a cluster's clients when the leader is gone; a kill
switch; the `harness` control scope; and a fatal refusal to start in public mode or with
self-service mocks. In a cluster, each node applies its own caps and allowlist to every
assignment it receives.

## Testing it

`cargo test` covers the engine, correlation, patterns, rate-limit tests, mock traffic,
identities, faults and protocols against an in-repo test proxy that stands in for a
gateway (it adds headers, answers its own 429s, drops requests and has reference rate
limiters with known flaws), and the cluster and failover with several nodes in one
process. `docs/examples/run.sh` runs this page's examples against a loop-through node;
`conformance/harness/run.sh` runs the compose cluster and the failover; `cargo run
--release --example harness_spike -- --sweep` is the engine benchmark.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/harness`, `/schema`, `/scenarios`, `POST /_rustybin/harness/validate | preview | runs`, `GET`, `PATCH /_rustybin/harness/runs/{run}`, `/stop`, `/events`, `/report`, the cluster routes under `/_rustybin/harness/clusters` |
| Console | The **Harness** view (shown when harness mode is on): the builder generated from the schema, runs, live tuning, reports |
| CLI | `rustybin harness validate | run | report` offline, the same with `--remote` and `status | scenarios | runs | show | tune | stop | events | clusters ...` against an instance ([reference](cli.md#harness)) |
| Management MCP | `harness_status`, `list_scenarios`, `get_scenario`, `save_scenario`, `list_patterns`, `validate_scenario`, `preview_scenario`, `create_run`, `start_run`, `update_run`, `list_runs`, `get_run`, `stop_run`, `get_run_report`, `create_cluster` and the cluster tools; the prompt `load_test_gateway`; the resource `rustybin://harness.schema.json` ([agent page](agent-mcp.md#harness)) |
| `rustybin.yaml` | `harness:` (the saved scenarios) ([the whole instance as code](configuration-as-code.md)) |
| Environment | `RUSTYBIN_HARNESS`, `_ROLES`, `_TARGETS`, `_MAX_RATE`, `_MAX_INFLIGHT`, `_MAX_DURATION`, `_VERIFY_TARGETS`, `_SCENARIO`, `_START`, `_REPORT`, `_EXIT` and the cluster variables ([configuration](configuration.md#harness-mode)) |
