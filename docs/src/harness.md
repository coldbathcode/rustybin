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

What verification proves, and what it does not. The token is public: the node serves it
to anyone, without a credential, and a cluster's server nodes serve the cluster's. A
target passes when a `GET` of its URL plus `/.well-known/rustybin-harness` answers `2xx`
with the token, so passing proves that the target routes that path to a node that serves
the token (or to anything that copied it), and that the route works. It does **not** prove
that whoever owns the target agreed to the load: a server that fetched the token and
serves it passes as well, and so does one that relays the path to a node. Consent and
scope come from the allowlist (`RUSTYBIN_HARNESS_TARGETS`, set by the operator) and the
caps; verification catches a typo in a target, a route that does not reach the cluster,
and a gateway in front of the wrong instance. A random challenge would not change that (a
relay passes it too), so there is none.

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
`RUSTYBIN_HARNESS_STATE_DIR` when set). Wherever a scenario is shown (the API, the
management tools, a report), credential-looking header values, listed API keys, OAuth
client secrets and secret-looking query values in its paths read `[redacted]`:

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
`RUSTYBIN_HARNESS_REPORT_DIR` when set, named after the run id; an id is 1 to 64 letters,
digits, `.`, `_` or `-`, and a cluster part with any other id is refused). The kill switch
stops all load on the node at once, whatever started it. It clears once 200 ms pass
without another press, so the next run may start; a run that was still being prepared
when it was pressed (resolving, fetching tokens or verifying its targets) never starts,
however long the preparation takes:

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
(`harness_status`, `validate_scenario`, `preview_scenario`, `import_traffic`, `import_scenario`, `create_run`, `get_run`,
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

## HTTP/3 targets

A target with `http: h3` is sent to over HTTP/3 (QUIC), for a gateway that terminates it:

```yaml
targets:
  - url: https://gateway.internal:8443
    http: h3
```

It needs a build with the cargo feature `harness-http3`: the published image whose tag ends
in `-http3` (`ghcr.io/coldbathcode/rustybin:latest-http3`, or `X.Y.Z-http3` for a release),
or your own build (`cargo build --release --features harness-http3`, or `docker build
--build-arg CARGO_FEATURES="quota-postgres harness-http3"`). The default binary and the
default image carry no QUIC stack: they know the parameter (it is in the schema, the console
form and the tools' schemas in every build) and refuse the scenario at validation with a
message that names the feature. `GET /_rustybin/harness` answers `http3: true` on a build
that has it, `false` on one that has not. In a cluster every node that sends runs the
variant.

The variant is signed like the default image and verified the same way, by its own tag:

```bash
cosign verify ghcr.io/coldbathcode/rustybin:latest-http3 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/coldbathcode/rustybin/'
```

Nothing about where load may go changes with the protocol. The allowlist entry's port is the
UDP port; the name is resolved once and QUIC connects to those pinned addresses only, with no
`Alt-Svc`, no redirect, and no move to another address a server asks for; forbidden addresses
are refused; with `RUSTYBIN_HARNESS_VERIFY_TARGETS` the target is verified over HTTP/3 (the
TCP port of the same number may be another service); the kill switch stops it; TLS is
verified unless the target says `insecure`. Every request carries the marker, and the report
classes the answers (`upstream`, `gateway`, `transport` with its kinds) and measures the
overhead as over HTTP/1 and 2.

HTTP/3 carries the plain HTTP sources (`request`, `requests`, a `mock`). The protocol
sources, the faults `connection_churn` and `http2`, and `identities.source_addresses` are
refused with an `h3` target, by name.

```hurl
{{#include ../examples/harness/api.hurl:harness_http3}}
```

A hop of a [server-to-server chain](#a-hop-over-http3) takes `http: h3` too, in the same
builds.

## Server-to-server chains

A request through a gateway often goes on: the service it reaches calls another service, through
the gateway again. `servers.chain` makes the harness's server side do the same, so a run measures
every leg: the server that answers a client's request calls the first hop, the server that answers
that call the second, and so on.

```yaml
servers:
  chain:
    hops:
      - url: https://gateway.internal
        path: /orders          # the route that reaches the next server
      - url: https://gateway.internal
        path: /stock
    timeout: 5s                # per onward call (default 5s, at most 30s)
    max_concurrent: 64         # onward calls in flight per run on a server node
```

A hop is a gateway URL plus the one path the call asks for (default `/`), with `host`, `http`
(`auto`, `h1`, `h2`, `h2c`, or `h3` in a [build with HTTP/3](#a-hop-over-http3)) and `insecure`
as a target has them. The onward call is a `GET` with the harness's own headers and no body.

This is the only traffic the server role sends, so it has rules of its own, and nothing turns it on
except a scenario that declares a chain:

- **Only the run's own requests.** A server calls on only for a request that carries its run's
  signed marker. A request without one, with a marker of another run, or with a signature that does
  not verify is answered as an ordinary request and makes no call.
- **Only while the run is live.** When the run ends, or the kill switch is pressed, the calls stop
  at once, those under way included. In a cluster a server calls on only on its leader's word that
  the run is sending, renewed like a client's lease, so a lost leader leaves no server calling. A
  marker replayed after the run calls nothing.
- **Only to the hops the scenario names.** Nothing of the incoming request (address, host, path,
  query, headers, body) chooses where the next call goes or travels with it.
- **Checked like a target.** Every hop must be allowed by `RUSTYBIN_HARNESS_TARGETS` **on the server
  node**, is resolved once and pinned, is never a link-local or metadata address or a control plane,
  and, with `RUSTYBIN_HARNESS_VERIFY_TARGETS`, must serve the verification token. A chain is
  prepared whole or not at all: a single node refuses the run (`403`, the problems by hop), a
  cluster's server node that cannot prepare it makes no call and the report says why.
- **A fixed depth, no loops.** At most 3 hops, never the same one twice. The hop count travels
  inside the marker's signature, so it cannot be changed on the way, and a server calls on at most
  once for a request at a hop: a gateway's retry, a mirrored or a replayed request calls nothing a
  second time. A run's onward calls never exceed the requests its clients sent times the hops.
- **Bounded.** `max_concurrent` calls in flight per run on a node; one over it is refused, not
  queued. Each call has its `timeout`. A run follows the first 16,777,216 requests of each hop.

The preview (`POST /_rustybin/harness/preview`) and the `summary` of a validation list the hops
before anything is sent:

```hurl
{{#include ../examples/harness/api.hurl:harness_chain_preview}}
```

A hop outside the allowlist refuses the run:

```hurl
{{#include ../examples/harness/api.hurl:harness_chain_refused}}
```

The report's `server.chain` has one entry per hop: the `calls` made, how they were answered
(`upstream`: by a server of the run, its tag verified; `gateway`: answered without it;
`transport` by kind), the `latency` of the leg, the next server's processing time
(`upstream_latency`) and the difference (`overhead`, the gateway's part of the leg), the
`arrivals` of that hop, and `refused`: the requests that made no call, by reason (`not_live`,
`kill_switch`, `seen_before`, `untracked`, `busy`, `not_prepared`). A server's processing time
includes its onward call, as a real service's would, so the client's `upstream_latency` holds the
legs behind it. An onward call is counted with its hop and not with `server.arrivals`, so `lost`
and `upstream_reach` keep describing what the clients sent.

```hurl
{{#include ../examples/harness/api.hurl:harness_chain_run}}
```

A dashboard sees the onward calls while the run is on:
[`/_rustybin/metrics`](concepts/control-plane-security.md#metrics) has the counter
`rustybin_harness_chain_calls_total{run, hop, result}` on every server node, for the calls that
node made (a cluster's total is the sum over its server nodes, as in the merged report).

| Label | Values |
|---|---|
| `run` | the run's id; like every `rustybin_harness_*` series, the active run plus the last 4 |
| `hop` | `1` to `3`, the report's `hop` |
| `result` | `ok`: the call was answered, whatever the status (the report's `upstream` plus `gateway`); `failed`: no answer, such as a refused connection or the `timeout` (the report's `transport` without `stopped`); or why a request made no call, or a call under way was ended: `not_prepared`, `not_live`, `kill_switch`, `seen_before`, `untracked`, `busy` (the report's `refused`) |

The series is fed by the counts the report sums, so at the end of a run the two agree. A value
appears once it is counted: a run without a chain has no such series, and neither has a result that
never happened, so write `sum(rate(...))` rather than expect a zero. With the kill switch pressed
`kill_switch` rises and `ok` stops. An onward call's arrival is in no series:
`rustybin_harness_server_arrivals_total` keeps counting the clients' requests only. There is no
latency histogram per hop; the report has the legs.

```hurl
{{#include ../examples/harness/api.hurl:harness_chain_metrics}}
```

### A hop over HTTP/3

A mesh whose inner hops speak HTTP/3 is tested end to end with `http: h3` on a hop:

```yaml
servers:
  chain:
    hops:
      - url: https://mesh.internal:8443
        http: h3
        path: /orders
```

It needs the [build with HTTP/3](#http3-targets) on the server nodes, which make the call; a
build without the cargo feature `harness-http3` refuses the scenario at validation with a
message that names the feature, as for a target, and a server node of a cluster that lacks it
makes no call and says so in `server.chain.problems`. Chains may be mixed: the clients' target
over HTTP/1 or 2 with a hop over HTTP/3, or the reverse.

Every rule of a chain holds as it is: the signed marker, the run being live, one bodiless `GET`
per request and hop, the fixed depth, `max_concurrent` and the `timeout`, the legs in the report
and the `rustybin_harness_chain_calls_total` series. And every fence of an HTTP/3 target holds
for the hop: the allowlist entry's port is the UDP port, the hop's name is resolved once on the
server node and QUIC connects to those pinned addresses only, with no `Alt-Svc`, no redirect and
no move to another address the hop's server asks for; with `RUSTYBIN_HARNESS_VERIFY_TARGETS` the
hop is verified over HTTP/3; TLS is verified unless the hop says `insecure`. A hop keeps at most 8
QUIC connections, as a target does. They are closed when the kill switch is set and when the run
is no longer live, calls under way included (the report counts those as `stopped`, under
`kill_switch` or `not_live`), and for good when the run ends.

```hurl
{{#include ../examples/harness/api.hurl:harness_chain_http3}}
```

## OpenTelemetry

Set `harness.otlp_endpoint` (Management, or `rustybin settings set`) to a collector's HTTP base URL
and each finished run is sent to its `/v1/metrics`, as OTLP over HTTP with a JSON body: sent, completed,
ok, transport errors, lost, the p50 and p99 latency, the achieved rate and whether the run passed,
labelled with the run, the scenario and its state. The secret `harness.otlp_token` goes along as a
bearer token when your collector wants one. A cluster's run is sent once, by the leader, merged.

Only those numbers and names go out, never a target, a header or a body. A task sends them
after a run has ended, so a slow collector cannot touch a run, and one that is down costs a
warning in the log, since the report still holds everything.

## Replaying a recording

A HAR file from a browser or a proxy, or the captures of `GET /_rustybin/requests`, can become a
scenario's source: `POST /_rustybin/harness/import`, `rustybin harness import <file>`, the
`import_traffic` tool, or the file field of the console's builder. Identical requests turn into one
template with their count as its weight (scaled when one passes 1000, the proportions kept), so the
replay keeps the recording's mix; the 64 most frequent are kept, as many as a layer takes. Nothing is sent:
put the answer's `source` in a scenario, which then meets the same validation and allowlist as any
other, and name your own target, since the recording's hosts are dropped.

A recording holds credentials and a scenario gets shared, so a secret-looking header or query
parameter is removed and named in `removed`, and an entry with a secret-looking field in its body
is left out and counted in `skipped`. Add the credentials your target needs through an
[identity](#traffic-from-mocks-identities-protocols-and-faults), never by hand into the file.

```hurl
{{#include ../examples/harness/api.hurl:harness_import}}
```

## Importing a Postman collection or an Artillery script

The same import reads two formats other tools write: a Postman collection (v2.1, as the
app exports it) and an Artillery script (YAML or JSON). The format is read from the
document; `?format=har|captures|postman|artillery` (`--format` on the command line, `format`
of the `import_scenario` tool) names it instead. It is still an import: the answer's `source`
goes into a scenario that names its own target and meets the same validation and allowlist
as a hand-written one.

Other tools say more than a request list, and only what has an exact counterpart is taken:

| From | Taken | Named in `left_out`, never run |
|---|---|---|
| A Postman collection | Folders (16 deep), each request's method, path, enabled query parameters and headers, the body modes `raw`, `urlencoded` and `graphql`, collection variables | `event` (pre-request and test scripts), `auth` at every level, `protocolProfileBehavior`; a `formdata` or `file` body leaves its request out |
| An Artillery script | `config.target` (its path leads the requests), `config.defaults.headers`, `config.variables` with one value, `config.http.timeout` (as `timeout`), each scenario's `weight`, the request steps with `url`, `qs`, `headers`, `json`, `body`, `form` and the `statusCode` of an `expect` | `config.processor` and every `function`, `beforeRequest`, `afterResponse`, `beforeScenario`, `afterScenario` (code), `config.plugins`, `payload`, `environments`, `ensure`, `tls`, `capture`, `match`, `think`, `loop`, `parallel`, `log`, scenarios of another engine; `ifTrue` and `formData` leave their request out |

An Artillery script's **phases** become `pattern` when the pattern model says the same
thing: `arrivalRate` is a `constant` stage, with `rampTo` a linear `ramp`, `arrivalCount` a
`constant` at the count over the duration, `pause` a `constant` at 0. An Artillery phase
counts arrivals and each arrival runs one flow, so a stage's rate is the arrivals times the
weighted requests per flow (`notes` says the number). The phases are all or nothing: a
`maxVusers`, a value that is not a number, more than 16 phases, a flow with `think`, `loop`,
`parallel`, `function` or `ifTrue`, or a request that was left out, and `pattern` is absent
with the reason in `left_out`.

**Variables** (`{{name}}` in both formats): one the document defines with a single plain
value is put in. Any other (an environment variable, a captured value, `{{$guid}}`) is
never guessed: its request is left out, counted in `skipped`, and the name is listed in
`variables`. Define it in the collection or the script, or
[give its value with the import](#giving-the-variables-their-values), one by one or as
[the collection's environment file](#a-postman-environment-file-as-the-variables). A variable whose
name looks secret is never put in, and credentials are removed as for a recording.

A program in a scripting language (a k6 or Locust script, an Artillery processor) is refused
with `400`: nothing here runs or translates code. A collection or a script is at most 4 MiB,
and at most 64 different requests are kept, the most frequent ones, since that is what a
layer takes.

```hurl
{{#include ../examples/harness/api.hurl:harness_import_postman}}
```

```hurl
{{#include ../examples/harness/api.hurl:harness_import_artillery}}
```

### Giving the variables their values

A collection usually keeps its base URL and ids in an environment the exported file does
not carry, so it can import as nothing: every request waits for a variable. Give the values
with the import. On the API the body becomes an envelope, `{"document": ..., "format": ...,
"variables": {...}}`, where `document` is the document or its text (a YAML script travels
as text) and `format` is optional:

```json
{{#include ../examples/harness/import-variables.json}}
```

```hurl
{{#include ../examples/harness/api.hurl:harness_import_variables}}
```

- A name is written as the document writes it, without the braces: `baseUrl` for
  `{{baseUrl}}`, `:order` for a Postman path variable. **A given value wins** over the
  document's own (the collection above says `version` is `v1`).
- A variable in the place of a Postman URL's host (`{{baseUrl}}/orders`) is a base URL: its
  host is listed in `hosts` and dropped as every host is, and its path leads the requests.
  Without a value it is dropped with the host, and `notes` says so.
- **The answer keeps naming what is still undefined** in `variables`, every variable a
  left-out request needs, so one more round finishes the job. When nothing imports at all,
  the `400` carries the same `variables` list. A given variable the document never uses is
  named in `notes`.
- **Never a credential.** A name that looks like one (`token`, `apiKey`, `password`) is
  refused, and a value that lands in a secret-looking header or query parameter is removed
  with it, as the document's own would be: the `Authorization` header above is gone
  whatever `session` holds. Give the scenario an
  [identity](#traffic-from-mocks-identities-protocols-and-faults) instead. A value is never
  logged, never in the audit log and never in an error, which names the variable only.
- **A name the harness has a placeholder for.** `seq`, `vu`, `run`, `node`, `layer`, `rand`,
  `id` and `ip` are the harness's own `{{placeholders}}`, and `{{id}}` is also the commonest
  variable a collection has. In an imported document it is always the document's variable,
  never the harness's: without a value its request is left out and the name listed in
  `variables`, like any other. Give it a value and it is put in like any other too, and
  `notes` names it (`variables: a harness placeholder name was given a fixed value: id`),
  because every imported request then carries that one constant where a hand-written
  `{{id}}` would get a new value for each request. To have the harness generate the value,
  write the placeholder into the scenario after the import. A value may still not hold
  `{{`, and a request whose text reads as a placeholder once the values are in is left
  out: an import never brings a placeholder in.
- At most 64 variables, 1 KiB each, text, numbers or `true` and `false`. A recording has no
  variables: giving one with a HAR file or captures is refused.
- Nothing about safety changes with a value: what comes out is still a scenario's `source`,
  validated and allowlisted like a hand-written one, and it still names its own target.

| Surface | How |
|---|---|
| API | `POST /_rustybin/harness/import` with the envelope `{document, format, variables, environment}` |
| Console | Load test, the builder's file field: when the file leaves requests out for variables (or drops a host's variable), a form under it lists them, takes more rows (such as a base URL) and takes the collection's environment file, which fills the fields; **Import again** sends the same file with the values |
| Management MCP | `import_scenario` with `variables`, a map of name to value, and `environment`, the environment file or its text |
| CLI | `rustybin harness import shop.postman_collection.json --var baseUrl=https://api.example.com/shop --var :order=o-42` (repeatable), `--environment staging.postman_environment.json` |
| `rustybin.yaml` | Nothing: an import is a conversion, its result is a scenario |

### A Postman environment file as the variables

The values usually exist already, in the environment the collection is run with. Export it
(the environment's menu, **Export**; a globals export has the same shape) and hand the file
over instead of typing each variable. This collection keeps its base URL, an id and a region
in its environment:

```json
{{#include ../examples/harness/items.postman_collection.json}}
```

```json
{{#include ../examples/harness/items.postman_environment.json}}
```

On the API the envelope takes it as `environment`, the document or its text, next to
`variables`:

```hurl
{{#include ../examples/harness/api.hurl:harness_import_environment}}
```

```bash
rustybin harness import items.postman_collection.json --environment items.postman_environment.json
```

- **Every entry meets the checks a given variable meets**, and the 64 count what is taken
  from the file together with what `variables` gives. A file is somebody's whole setup, so
  an entry that fails a check is not taken, and `notes` names it with the reason, where a
  variable you typed would be refused: the import goes on with the rest.
- **An entry marked `secret` is never taken**: it is a credential. Nor is one whose name
  looks like a credential (`token`, `apiKey`), whatever its type. Both are named in `notes`,
  never their values. Give the scenario an
  [identity](#traffic-from-mocks-identities-protocols-and-faults) instead, and export the
  environment without its secrets where you can: the file still travels to the instance.
- **A disabled entry** (`"enabled": false`) is passed over, as the app passes over it. An
  entry without a value, or with one that is no variable's (over 1 KiB, holding `{{` as a
  value built from another variable does, or not text, a number, `true` or `false`), is
  not taken and is named. When a name is listed twice, the last entry stands.
- **`variables` win** over the file's entries, so one value can be changed without editing
  the file. An entry the document never uses is not reported: only a variable you typed is
  (it may be a typo).
- An environment is at most 256 KiB and 1000 entries. One that is not an environment (not
  JSON, no `values` list, an entry without a `key`) is a `400` that quotes nothing of it,
  and more than 64 usable variables is a `400` too: disable the entries the collection does
  not use. A value is never logged, never in the audit log and never in an answer beyond
  the requests it was put in. A recording has no variables, from a file either.
- In the console the form reads the file in the browser to fill the fields it lists (never
  from a secret or disabled entry) and sends the file with the import for the rest, such as
  a base URL.

## Importing an Arazzo document

The import also reads an [Arazzo](saved-tests.md#arazzo-workflows) 1.0 document
(`?format=arazzo`, or read from the document): the steps of its workflows become
request templates, each against the mock its source description is on this instance.
A step's `$statusCode` criterion is the template's `expect_status`.

A scenario's requests are sent by weight, not as a journey, so nothing can be passed
from one to the next: a step that needs a workflow input or an earlier step's output
is left out, and the value is named in `variables`. A workflow input can be
[given with the import](#giving-the-variables-their-values) under the name `variables`
shows (`PETNAME` for the input `petName`); an earlier step's output cannot, since only a
run makes it. Load test the steps that stand
alone here, and run the whole journey as a multi-step saved test. Both steps of the
example document need a value, so the import says so:

```hurl
{{#include ../examples/tests/arazzo.hurl:harness}}
```

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
sending), and one that finds the leader of a newer term steps down. A crashed leader that
restarts is a new node: start it as a standby pointing at any eligible member to rejoin.

To take the leader down for maintenance, ask it to step aside first, with
`POST /_rustybin/harness/clusters/{id}/leader/step-aside` on the leader,
`rustybin harness step-aside <id>`, the `step_aside` tool, or **Step aside** on the cluster
card. It needs three or more eligible nodes. The leader stops renewing leases and the
first in the succession takes over by the ordinary election, within the election
timeout, so the run stays on and nothing is lost; the old leader follows the new one. If
nobody takes over within four election timeouts it keeps leading.

```hurl
{{#include ../examples/harness/api.hurl:cluster_step_aside}}
```

A leader steps down only for a newer leader it can check: an announcement from a node it
knows as eligible, at a term at most 16 ahead of its own, naming the control URL that node
joined with; or, when a member's heartbeat claims a newer term, an eligible node that answers
its probe as that term's leader at its own control URL (or names one that passes the same
checks). The claim alone only starts the probe (at most every half election timeout), so a
member cannot depose the leader by saying so. A member follows a leader on the same terms,
whether an `elected` announcement names it, a node's `409` does or a probe of the succession
finds it; anything else is refused, so a member's node credential cannot crown its holder.
Every call between nodes refuses link-local, cloud metadata, unspecified, multicast and
broadcast addresses at the connection itself (names are resolved and checked for each
connection), so a name that changes its answer after a URL was checked gets nowhere. **The
join token is a cluster credential**: a
holder can join as an eligible node (with a control URL) and so receive the snapshot,
secrets included, and stand in elections. Treat it like the admin token: give it only
to the nodes you run, and rotate it (a new cluster) if it leaks.

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
`/.well-known/rustybin-harness` (proof of the route, not of the owner's consent, see
above); caps per node on rate, requests in flight and duration;
one run at a time (a start being prepared holds the slot, so parallel starts get `409`, and
a kill pressed while a run is still resolving or verifying its targets keeps it from
starting); a node never sends above its rate cap, whatever the live multipliers and its
share say (a re-split share is checked with the multipliers in force, and the scheduler
clamps too); answers with absurd `Server-Timing` or `Retry-After` values cannot stall a
run; a lease that stops a cluster's clients when the leader is gone; a kill switch; the `harness` control scope; and a fatal refusal to start in public mode or with
self-service mocks. In a cluster, each node applies its own caps and allowlist to every
assignment it receives, and node-to-node calls (join, heartbeat, vote, announcement) refuse
a URL with credentials and every link-local, cloud metadata or multicast address, checked
at each connection, so a DNS change cannot turn a node into a way to reach one.

Two things send in other ways and go through the same fences. An
[HTTP/3 target](#http3-targets) is pinned like any other and QUIC connects to the pinned
addresses only, so UDP is no way around the allowlist. A
[chain](#server-to-server-chains) is the only traffic the server role sends: only for a
request carrying its run's signed marker, only while the run is live, only to the hops the
scenario names (each allowed on the server node, pinned and verified like a target), once
per request and hop to a fixed depth, and the kill switch stops it at once.

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
| API | `GET /_rustybin/harness`, `/schema`, `/scenarios`, `POST /_rustybin/harness/validate \| preview \| import \| runs`, `GET`, `PATCH /_rustybin/harness/runs/{run}`, `/stop`, `/events`, `/report`, the cluster routes under `/_rustybin/harness/clusters` |
| Console | The **Harness** view (shown when harness mode is on): the builder generated from the schema with its file field for a recording, a collection or a script, runs, live tuning, reports |
| CLI | `rustybin harness validate \| run \| report` offline, the same with `--remote` and `status \| scenarios \| import \| runs \| show \| tune \| stop \| stop-all \| events \| clusters ...` against an instance ([reference](cli.md#harness)) |
| Management MCP | `harness_status`, `list_scenarios`, `get_scenario`, `save_scenario`, `list_patterns`, `validate_scenario`, `preview_scenario`, `import_traffic`, `import_scenario`, `create_run`, `start_run`, `update_run`, `list_runs`, `get_run`, `stop_run`, `get_run_report`, `harness_stop` (the kill switch), `create_cluster` and the cluster tools; the prompt `load_test_gateway`; the resource `rustybin://harness.schema.json` ([agent page](agent-mcp.md#harness)) |
| `rustybin.yaml` | `harness:` (the saved scenarios) ([the whole instance as code](configuration-as-code.md)) |
| Environment | `RUSTYBIN_HARNESS`, `_ROLES`, `_TARGETS`, `_MAX_RATE`, `_MAX_INFLIGHT`, `_MAX_DURATION`, `_VERIFY_TARGETS`, `_SCENARIO`, `_START`, `_REPORT`, `_EXIT` and the cluster variables ([configuration](configuration.md#harness-mode)) |
