# Changelog

## Unreleased: the AI and agent gateway upgrade

A broad rework of Rustybin, from an httpbin-style echo service into an upstream for
API, AI and agent gateway demos.

### Foundation

- axum 0.8, a library plus a thin binary, a shared `AppState`, and a middleware stack
  with request ids, CORS, a time-to-headers timeout, a request body limit and the
  request inspector.
- The route catalogue as the single source of truth for the landing page, the eight
  collection exports, the README tables and the console's API explorer, with tests
  that keep routes, catalogue and OpenAPI in sync.
- Fault injection on every route (`X-Rustybin-Delay`, `X-Rustybin-Fail`), sessions
  (`X-Rustybin-Session`), public mode, an admin token for global mutations, and a
  control plane under `/_rustybin/`.
- Graceful shutdown; the HTTPS and gRPC listeners are optional; invalid settings
  warn instead of failing; bounded state everywhere.

### New

- **Mock LLM** (`/ai/*`): OpenAI (Chat Completions, Responses, completions,
  embeddings, moderations, images, audio), Azure OpenAI, Anthropic, Gemini, AWS
  Bedrock (with binary event streams), Ollama and Cohere, with modes, scripted
  rules, tool calls, structured output, deterministic tokens, native errors and
  rate-limit headers, latency and pace controls, credential checks and request
  inspection.
- **Guardrail mocks**: Azure AI Content Safety, Bedrock ApplyGuardrail, generic
  check and PII redaction.
- **MCP server**: Streamable HTTP for 2026-07-28 and the 2025 handshake versions,
  legacy HTTP+SSE, OAuth 2.1 protected and API key variants, named servers.
- **A2A agents**: seven agents over A2A v1.0 and v0.3, JSON-RPC and HTTP+JSON, with
  streaming, multi-turn, auth-required and SSRF-safe push notifications.
- **Identity provider**: authorization code + PKCE with a login page, refresh token
  rotation, password, client credentials, token exchange, introspection,
  revocation, dynamic client registration.
- **Real auth checks**: JWT validation (HS256 and RS256), HMAC signatures (hmac-auth
  and draft-cavage, with digests), mTLS on the HTTPS listener or forwarded in a
  header, with a persisted demo CA.
- Request bins, generic SSE streams, webhook signing and verification (Standard
  Webhooks, GitHub and Stripe styles), compression (gzip, deflate, Brotli, zstd),
  ranges, data transfer helpers, generic JSON-RPC, GraphQL subscriptions and
  automatic persisted queries, gRPC reflection, health and gRPC-Web.
- The **web console** at `/ui`.
- **OpenAPI mocks** (`/mock/{name}`): upload an OpenAPI 3.0 / 3.1 spec and get a
  named mock API answering with spec examples or deterministic data generated from
  the schemas (path parameters echoed), with rules, per-operation overrides, the
  `Prefer` header, request validation (warn or enforce), deploy checks with a
  report, drafts, rollback, an on/off switch, captured traffic, and mocked security
  from the spec (API key, basic, bearer, OAuth 2.0 / OpenID Connect scopes against
  the built-in identity provider, mTLS) or a fixed scheme, its secrets shown only to
  callers who may change the mock. Managed in the console,
  through `/_rustybin/mocks*`, from `rustybin.yaml` (`RUSTYBIN_CONFIG_FILE`,
  `--config`, reload on `SIGHUP`, JSON Schema for editors) or a directory of specs
  (`RUSTYBIN_MOCKS_DIR`), and with the `rustybin mock` CLI. A `petstore` sample is
  built in (`RUSTYBIN_MOCKS_SAMPLE`). A reload never deletes mocks because of a
  broken file or entry (they keep their last good revision and show a
  `load_error`), enforced validation answers `406` for an unacceptable `Accept`,
  missing scopes get an RFC 6750 `insufficient_scope` challenge, and requests are
  counted in `rustybin_mock_requests_total`.
- **Self-service mocks** for hosted demos (`RUSTYBIN_MOCKS_SELF_SERVICE`, off by
  default): visitors of a public instance create and manage their own mocks
  through a manage token, on a time allowance (an hour, extendable to three, then
  an email address for up to six hours a day, delivered to a webhook), on a
  separate mocks domain, with responses that can never set cookies, redirect
  elsewhere or ship active content. Operators can list, end and pause visits.
- Mock collections also export as a Bruno collection and an Insomnia export
  (`GET /_rustybin/mocks/{name}/collections/bruno|insomnia`, `rustybin mock
  collection`, the console and the management MCP's `export_collection`), in the
  same shapes as `/export/bruno.json` and `/export/insomnia.json`.
- **An MCP server for every OpenAPI mock** at `/mock/{name}/_rustybin/mcp` (and
  `<name>.<domain>/_rustybin/mcp` on the mocks host): one tool per operation, with
  flat arguments from the parameters and body, annotations from the method, an
  output schema from the default response, and the served spec as a resource. Each
  call runs through the mock (rules, examples, stateful data, mocked security with
  the MCP request's credentials, delays, captures marked as MCP), so tools return
  what the REST calls return; a status outside 2xx is a tool error. Settings
  `mcp.enabled` and `mcp.exclude`; an "MCP server" tab in the console.
- **Management MCP server** at `/_rustybin/mcp`: configure a Rustybin from an AI
  agent such as Claude Code. Tools for the instance, OpenAPI mocks (create, edit,
  check, deploy, roll back, delete, collections, `rustybin.yaml` export and apply),
  testing (`try_request` in process on this instance only, `issue_token`,
  `validate_payload`, `wait_for_requests`), captured traffic, usage and
  self-service visits, plus three prompts and two resources. Protected by
  control-plane authentication; `tools/list` shows only what the caller may use;
  destructive tools ask for confirmation (elicitation, else `confirm: true`);
  captured traffic is returned only under `untrusted_*` fields, truncated; changes
  are logged without arguments and counted in `rustybin_manage_mcp_calls_total`.
  The console shows the `claude mcp add` command for the instance, and
  `rustybin mcp-stdio` bridges stdio-only clients.
- **OAuth sign-in to the management MCP server**: where the control plane asks for
  credentials, agents sign in through the browser instead of pasting a token. A
  built-in authorization server (RFC 9728 and RFC 8414 metadata, dynamic client
  registration, PKCE, refresh tokens) sends the browser to a consent page in the
  console, which grants at most what the approver's own credential allows. Tokens
  open `/_rustybin/mcp` only. `RUSTYBIN_CONTROL_OAUTH`, `RUSTYBIN_CONTROL_OAUTH_KEY`,
  and `RUSTYBIN_CONTROL_OAUTH_ISSUER` for an external authorization server.
- **Plan limits** for hosted offerings (`RUSTYBIN_PLAN`, `RUSTYBIN_LIMIT_*`,
  `RUSTYBIN_USAGE_FILE`): rate, concurrency, streams and daily or monthly request and
  egress quotas, per instance or per session, with IETF RateLimit headers on 429s and
  `GET /_rustybin/usage`. Off by default.
- This documentation site, with every example tested in CI, and SDK conformance
  suites for the mock LLM, MCP, A2A and OAuth.

### Operations and hosting

- The management MCP server no longer passes the caller's control credentials
  into the requests its tools send to the data plane, and captures (inspector,
  bins, mock traffic) store `X-Rustybin-Admin-Token` and `X-Rustybin-Mock-Token` as
  `[redacted]`. Mock traffic now redacts API keys like the inspector does.
- A workflow publishes the image to `ghcr.io/coldbathcode/rustybin` on pushes to
  `main` and on `v*` tags.
- Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`): `token` (admin token) or
  `jwt` (admin token or Ed25519 JWTs with `aud`, `exp` and the scopes `inspector`,
  `console`, `admin`) for every `/_rustybin/*` route except the new readiness probe.
  The web console signs in with a token from the URL fragment or a sign-in screen
  and reads its live feed with `fetch` so the bearer header is sent.
  `RUSTYBIN_HOSTED_MODE=true` presets `jwt` and JSON logs.
- `GET /_rustybin/ready`: readiness, always 200 while serving, independent of the
  `/health` demo toggle, exempt from auth, plan limits and capture.
- `GET /_rustybin/metrics`: Prometheus metrics with bounded labels (route templates,
  status classes, protocols, mock LLM tokens by model family, injected faults,
  open streams, egress, build info).
- More metrics: `rustybin_limit_rejections_total{dimension}` (plan limiter 429s and
  gRPC `RESOURCE_EXHAUSTED`), `rustybin_llm_requests_total{provider,model_family,streaming}`
  and `rustybin_llm_faults_total{provider,kind}` (mock LLM faults by kind), and
  latency buckets from 1 ms to 60 s on `rustybin_request_duration_seconds` so p50
  and p99 can be computed with `histogram_quantile` (PromQL examples in the docs).
- `RUSTYBIN_LOG_FORMAT=json`: one JSON object per line with the request id.
- `RUSTYBIN_GRPC_ON_HTTP=true`: the gRPC services on the HTTP and HTTPS listeners
  (h2c and ALPN `h2`) for a TLS-terminating proxy.
- `RUSTYBIN_CONSOLE_TITLE` and `RUSTYBIN_CONSOLE_BACKLINK` in the console header.
- `/_rustybin/version`, `/_rustybin/status` and `/_rustybin/usage` report the build
  commit (`git_sha`) and the control auth mode.
- Shared mode (`RUSTYBIN_SHARED_MODE=true`): one instance for many API keys
  (`X-Rustybin-Key` or `?rb_key=`) from a registry file or URL, per-key limits and
  daily quotas (429 `rustybin_quota_exceeded` until 00:00 UTC), inspector and usage
  scoped by key, keys redacted in captures, readiness waiting for the registry, and
  `GET /_rustybin/keys/usage` for collectors. `RUSTYBIN_REQUIRE_KEY=true` requires a
  key on a normal instance.
- Optional cargo feature `quota-postgres`: replicas share daily quotas through
  PostgreSQL (`RUSTYBIN_QUOTA_STORE`), with `rustybin_quota_store_up` in the metrics.
- **Harness mode, single node** (`RUSTYBIN_HARNESS=on`, phase 1 of the harness plan):
  Rustybin as a load client with its own engine runtime, a dedicated scheduler
  thread, an in-flight cap from the descriptor limit and saturation detection, so a
  slow client is never read as a slow gateway. Scenarios (targets with weights,
  request templates with placeholders, open and closed load models, `constant` and
  `ramp` patterns chained with `then`, thresholds) are validated against the node's
  caps and one JSON Schema (`GET /_rustybin/harness/schema`) drives the API, the
  management MCP tools (`validate_scenario`, `create_run` with confirmation,
  `get_run_report`, `harness_stop`) and the console's scenario builder. Targets must
  be in `RUSTYBIN_HARNESS_TARGETS` and are pinned to their resolved addresses;
  link-local, metadata and control-plane targets are always refused; optional target
  verification through `/.well-known/rustybin-harness`; a kill switch at
  `POST /_rustybin/harness/stop`; the `harness` control scope. Reports as JSON, text
  and JUnit; headless runs at startup with exit codes; `rustybin harness run |
  validate | report`.
- **Harness correlation** (phase 2): every harness request carries a signed marker
  (`X-Rustybin-Harness`), an `X-Request-Id` and a `traceparent`; a server layer
  records arrivals, processing time and what the gateway changed (headers added,
  removed, rewritten, bodies, paths), samples captures into the inspector and tags
  every answer (`Server-Timing: rb`, `X-Rustybin-Served-By`). The client classifies
  answers as `upstream`, `gateway` (by status) or `transport`, counts `lost`
  requests after the run, and reports the gateway overhead and, in loop-through,
  one-way legs. Thresholds `gateway_overhead_p99_ms`, `lost`, `upstream_reach`; the
  scenario's `servers` block drives the shaped upstream `/harness/{*path}` (log-normal
  latency, status mix, body size, chunked streaming) and inspector sampling;
  `rustybin_harness_*` metrics with a bounded run label.
- **Harness clusters** (phase 3): a coordinator creates a cluster
  (`POST /_rustybin/harness/clusters`, or `RUSTYBIN_HARNESS_CLUSTER` with
  `RUSTYBIN_HARNESS_JOIN_TOKEN`); client and server nodes join with the token
  (`RUSTYBIN_HARNESS_JOIN`, `POST /_rustybin/harness/join`) and heartbeat with a long
  poll, carrying cumulative stats and NTP-style clock stamps. Runs are submitted, then
  started once the quorum is there (`scale.client_nodes`, `scale.server_nodes`, the
  cluster's `min_nodes`; headless `RUSTYBIN_HARNESS_START=on-quorum`): the rate is split
  by declared capacity, virtual clients by disjoint ranges, the start is synchronised,
  and one report merges every node's exact histograms, with `lost` checked against
  every server node. Leases stop a client that loses its leader. The server nodes are
  exported for the gateway's upstream pool (`GET .../clusters/{id}/upstreams`); members
  redirect controllers to the leader. MCP tools `create_cluster`, `list_cluster_nodes`,
  `export_upstreams`, `start_run` (and `cluster_id` on the run tools); a cluster card in
  the console.
- **Harness failover** (phase 3b): nodes with `RUSTYBIN_HARNESS_CONTROL_URL` are
  eligible to lead (`RUSTYBIN_HARNESS_LEADER=prefer` makes a standby). The leader
  replicates a versioned snapshot (members, runs with a checkpoint of their stats,
  secrets to eligible nodes only) and orders the live eligible nodes by work; when it
  goes silent for the election timeout (`RUSTYBIN_HARNESS_ELECTION_TIMEOUT`, 3 s), the
  first in line is elected by a majority of the eligible nodes and takes over within
  the lease, so clients never stop. Every message carries the term; an old leader
  steps down, a leader without a majority stops renewing leases (a minority partition
  stops sending). The new leader hands its own client share back and re-splits the
  share of silent clients once their lease expired, never sending more than planned;
  what an old leader's own part sent after its last checkpoint is reported as
  `unreported`. `GET .../clusters/{id}/leader` on any member, vote and announcement
  node routes, the MCP tool `get_leader`, and the leader, term, history and succession
  in the console.
- **Harness patterns and live tuning** (phase 4): layers run every pattern
  (`constant`, `ramp`, `step`, `target`, `breakpoint`, `spike`, `pulse`, `burst`,
  `wave`), chained with `then:`, with `jitter` and `sync: cluster | node`; the scheduler
  follows the exact curve second by second and lands a burst within its window. Closed
  loops: `target` on `overhead_p99_ms` moves the rate toward the gateway overhead goal,
  `breakpoint` stops at the first plateau that fails `until` and reports the last good
  rate. `PATCH .../runs/{run}` changes a running run (rate multiplier, pause and resume,
  hold, skip stage, `set` on any pattern field, add and remove layers), checked against
  the caps, logged, and carried in the cluster snapshot through a failover;
  `GET .../runs/{run}/events` is the run's feed (server-sent events). Reports gain the
  changes, the findings and every stage as it ran. `POST /_rustybin/harness/preview`
  returns the expected curve; saved scenarios (`PUT` and `DELETE
  /_rustybin/harness/scenarios/{name}`, `RUSTYBIN_HARNESS_STATE_DIR`) and six new
  built-ins. `scale.client_nodes: auto` waits for nodes and emits `scale_request`, also
  POSTed to a signed controller hook (`RUSTYBIN_HARNESS_SCALE_HOOK`), with
  `scale.scale_timeout`; `scale.connections_per_client`. MCP tools `list_patterns`,
  `preview_scenario`, `save_scenario`, `update_run`; in the console, a preview chart,
  saved scenarios, live run controls and the feed.
- **Harness rate-limit tests** (phase 5): a layer's `expect` (`limit` per `window`,
  `per` key, ip, consumer, route or global, a `burst` allowance, the refusal `status`, a
  `node_header`) and the `limit_test` pattern with ten tests: `over_limit`,
  `window_edge` (bursts around window boundaries on the wall clock), `just_under`,
  `distributed` (every client node at the same window start, over every target),
  `many_keys` and `many_ips` (identity pools: `{{id}}`, `{{ip}}`, rotating forwarded
  addresses from 198.18.0.0/15), `retry_wave` (refusals retried at once or after
  `Retry-After`), `concurrency`, `slow_rise` and `limit_probe` (a closed loop: a binary
  search for the effective limit), each with an optional `recover` stage. The report's
  `rate_limits`: admission per window and in the worst window-long span, over- and
  under-admission, the first refusal, recovery, the measured window, identities and
  gateway nodes, the rate-limit headers (IETF `RateLimit` drafts, `X-RateLimit-*`,
  `Retry-After`), upstream protection from the server side's arrivals by layer,
  collateral damage to the other layers, and findings; per-layer `thresholds`
  (`rate_limited`, `over_admission`, `under_admission`). Counted on every client node
  and merged by the leader. Built-ins `rate-limit`, `limit-probe` and `many-ips`; the
  analysis in the console's report and the MCP report summary.
- **Harness mock-driven traffic and identities** (phase 6): a layer's source can be an
  OpenAPI mock (`source: { mock: petstore }`): every operation of its live revision, with
  generated parameters and bodies, `operations` (include, exclude, weights),
  `invalid_fraction` (requests that expect a 4xx), `stateful` (a create followed by a
  read of what it made) and `base_path`. A layer's `identities`: API keys (listed or
  minted), JWTs minted by the node's built-in IdP (RS256 or the shared HS256 demo
  secret), OAuth client credentials against an allowlisted token endpoint (fetched
  before the run, cached, renewed), forwarded-address pools and source address binding.
  The report breaks the answers down by operation and by credential (labels without
  secrets) and says when invalid requests got through; listed keys and client secrets
  are redacted. The `petstore-customers` built-in.
- **Harness faults and protocols** (phase 7): a `faults` block with presets (`calm`,
  `jittery`, `flaky-upstream`, `retries`, `synchronised`, `churn`, `all`) and the live
  change `faults`. Client faults by fraction: connection churn, abandoned requests, slow
  bodies, malformed JSON, wrong content types, oversized headers, immediate (or
  synchronised) retries on 5xx, expired tokens, an HTTP/2 mix. Server faults on the
  run's marked requests: drops after the headers, stalls, flapping `/health`, and node
  churn (the server nodes down in turn, 503 with the connection closed). Protocol
  sources: `llm` (streamed chat completions timed token by token), `sse`, `websocket`
  (upgrade and masked frames on the pinned connections), `grpc` (unary echo over HTTP/2,
  judged by `grpc-status`) and `mcp` (stateless `tools/call`). The report's `streams`
  (time to first token, inter-token latency, tokens per second, stalls), the injected
  faults by name, `abandoned` and `unattributed` counts. Built-ins `llm-streaming`,
  `flaky-upstream` and `jittery-clients`.
- **Harness docs and conformance** (phase 8, the last): the guide
  [Load testing through a gateway](harness.md) with tested examples for the control API
  (status, schema, validate, preview, a run with a live change and its report in three
  formats, saved scenarios, the cluster API on one node, the kill switch) and the CLI;
  a compose cluster (`docs/examples/harness/cluster`: a leader, a standby, two clients,
  two servers and a stand-in gateway, headless on quorum); `conformance/harness/run.sh`,
  which runs that cluster, kills the leader mid-run and asserts the standby's report; the
  harness tools, prompt and resource on the agent page. The harness plan is complete.
  Found by that run and fixed: a member that followed a new leader's announcement stood
  for election itself at once, and candidates that split the votes retried in lockstep;
  following a leader now defers the node's next stand, retries add a random wait, and
  refused votes are logged with their reason.

### Changed

- Open redirects and other abuse-prone endpoints were removed or made safe
  (`/redirect-to` only redirects to the same host).
- Content negotiation serves JSON to browsers by default.
- The Docker image no longer generates certificates with OpenSSL at startup:
  Rustybin generates and persists its own demo PKI (the old script produced a CA
  that the server replaced, which broke mTLS and HTTPS verification in containers).
- `tests/smoke_test.sh` was replaced by the documentation examples
  (`docs/examples/run.sh`), which need Hurl 8 (CI pins 8.0.1).
- The Docker image runs as an unprivileged user (uid 10001) and supports a
  read-only root filesystem; `docker-compose.yml` keeps the demo PKI in a named
  volume, and `fly.toml` uses ports 8080 / 8443 and the readiness probe.
- MCP `tools/list` (and the other list methods) render their result once per server
  and protocol version instead of on every request: about 7x the throughput, same
  bytes on the wire.
- Minimum supported Rust version is now 1.88 (was 1.86), to take security fixes in
  `ruzstd` (zstd request bodies, RUSTSEC-2026-0344) and `time` (used when parsing
  client certificates, RUSTSEC-2026-0009).
- Security updates: rustls 0.23.46 (RUSTSEC-2026-0285, -0346, -0347, -0348, -0349),
  rustls-webpki 0.103.15 (RUSTSEC-2026-0104), h2 0.4.20 (RUSTSEC-2026-0258) and
  quick-xml 0.42 (RUSTSEC-2026-0194, -0195: memory exhaustion in the namespace reader
  used by the SOAP endpoint). SOAP field values still decode `&amp;`, character
  references and the other predefined entities; unknown entities are a Sender fault.
- CI runs `cargo audit` on every push.

### Fixed

From a full code review (2026-10-25):

- **GraphQL**: a query nested thousands of levels deep overflowed the parser's stack
  and aborted the whole process. Queries nested more than 64 levels are refused before
  parsing (`NESTING_LIMIT_EXCEEDED`, WebSocket close 1008).
- **Credentials in captures**: `Authorization: Bearer` with the admin token or a
  control JWT is captured as `[redacted]`; the `rb_key` query is redacted in OpenAPI
  mock captures; AI request records redact `X-Rustybin-Key` and `rb_key` and, in
  shared mode, are scoped by key.
- **Console**: the API explorer shows the console's own credentials masked; a valid
  token with a narrow scope signs in; the admin token is kept for the tab only.
- **Shared mode console**: a token bound to a key (`inspector` scope, `rb_key`) was
  sent back to the sign-in screen when an admin token was configured: the console's
  `GET /_rustybin/status` and every other route outside the key's reach answered `401`.
  The status now answers a key with the key's view, and a valid key gets `403` on the
  routes it may not use (`401` stays for a missing or invalid credential). The
  console's own data-plane requests send the token as `X-Rustybin-Key-Token` (it
  stands in for the key, redacted in captures), its traffic view no longer narrows a
  key's requests to the console session, the overview shows the key's view, and the
  API explorer may read the route catalogue.
- **MCP**: tool scopes are checked inside JSON-RPC batches on `/mcp/protected`; the
  legacy HTTP+SSE transport caps requests in flight per session.
- **Self-service**: redirects that browsers read as off-domain (`/\host`) are refused;
  a trailing-dot `Host` no longer reaches the console on the mocks domain; rotating a
  manage token can no longer drop the visit's mocks; every saved visitor mock loads
  after a restart; a visitor mock's spec answers only on the mocks domain.
- **OpenAPI mocks**: odd path templates, multibyte response keys and huge integer
  bounds no longer panic; validation, `minLength` and stateful collections are bounded.
- `/status` with weights whose sum overflows answers `400`; the Azure-shaped
  `text:analyze` caps blocklist names and repeated rows; `/response-headers` keeps
  reflected values inert under any `Content-Type`.
- File-defined mocks load before the HTTP listener serves; JWT control auth warns when
  the token audience would change on every restart; `/openapi.json` is built once.
