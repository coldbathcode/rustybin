# Step 13: Harness follow-ups

Three open questions of `docs/plans/harness.md` section 17 (5, 3 and 6), built in that order: the
smallest first, the one that widens what an instance sends last. The choices are those of section 5
of the master plan: each part keeps the rule it touches, and nothing new is on until a scenario asks
for it.

## Part A: scenario import from other tools

### What and why

People arrive with a Postman collection or an Artillery script, not with a HAR file. Step 08 made a
recording into `source.requests`; this part reads two more formats into the same thing.

### Design

- **Still an import** (`harness/import.rs`): what comes out is `source.requests`, so the engine and
  `safety.rs` are untouched and an imported request meets validation, the allowlist and pinning like
  a hand-written one. Hosts are dropped and listed: a scenario names its own target.
- **One route, one answer**: `POST /_rustybin/harness/import` reads the format from the document
  (`?format=har|captures|postman|artillery` says it outright). The body is JSON, or YAML for an
  Artillery script. The answer keeps the fields of step 08 (`source`, `entries`, `skipped`,
  `removed`, `hosts`) and gains `format`, `left_out` (what does not map, by its place in the
  document), `variables` (the ones the document does not define) and, for an Artillery script whose
  phases map, `pattern` and `timeout`.
- **Postman collections v2.1** (`info.schema` names v2.1; v1 and v2.0 are refused with the reason):
  folders walked to 16 levels, each request's method, the URL's path and enabled query parameters,
  enabled headers, and the body modes `raw`, `urlencoded` and `graphql`. Left out and named:
  `event` (pre-request and test scripts), `auth` at any level (a credential), `formdata` and `file`
  bodies, `protocolProfileBehavior`.
- **Artillery scripts**: `config.target` (its host is listed, its path prefixes the requests),
  `config.defaults.headers`, `config.variables` with one value, `config.http.timeout`, each
  scenario's `weight`, and the request steps of a flow (`get`, `post`, `put`, `patch`, `delete`,
  `head`, `options` with `url`, `qs`, `headers`, `json`, `body`, `form`, and the status of an
  `expect` list). Left out and named: `config.processor` and every `function`, `beforeRequest`,
  `afterResponse`, `beforeScenario`, `afterScenario` (code: never run), `config.plugins`,
  `config.payload`, `config.environments`, `config.ensure`, `config.tls`, `capture`, `match`,
  `think`, `loop`, `parallel`, `log`, `ifTrue`, `auth`, `cookie`, `formData`, and a scenario of
  another engine (`ws`, `socketio`).
- **Phases** become a pattern only when the model says the same thing: `arrivalRate` is a `constant`
  stage, `arrivalRate` with `rampTo` a linear `ramp`, `arrivalCount` a `constant` at count over
  duration, `pause` a `constant` at 0. An Artillery phase counts arrivals, each of which runs one
  flow, so a stage's rate is the arrivals times the weighted requests per flow, and the answer says
  so in `notes`. Phases are all or nothing: a `maxVusers`, a value that is not a number, more than
  16 phases, or a flow whose request count per arrival is not known (`loop`, `think`, `parallel`,
  `function`, `ifTrue`) leaves `pattern` out and names why.
- **Variables**: `{{name}}` in both formats. One the document defines with a single plain value
  (a collection `variable`, `config.variables` with one value) is put in. Any other (an environment
  variable, a captured value, `{{$guid}}`, `{{ $randomNumber() }}`) is never guessed: the request is
  left out (`skipped`: an unresolved variable) and the name is listed in `variables`. It is not left
  in place either, because `{{id}}`, `{{seq}}` and the other harness placeholders would then mean
  something the author never wrote. A variable whose name looks secret is never put in.
- **Credentials** as the recording import does it: a secret-looking header or query parameter is
  removed and named in `removed`, an entry whose body has a secret-looking field is left out.
- **A program is refused**: a document that is neither JSON nor a YAML mapping (a k6 or Locust
  script, a JavaScript processor) answers `400` with the reason and the formats that are read.
- **Bounds**: 4 MiB of text for a collection or a script (a HAR file keeps the instance's body
  limit), 5,000 requests read, folders 16 deep, 100 names in `left_out` and in `variables`, and the
  templates kept are at most a layer's (`MAX_REQUESTS_PER_LAYER`, 64: step 08 kept 200, more than a
  layer validates). Parsing runs on the blocking pool, off the request workers.

### Surfaces

| Surface | What |
|---|---|
| API | `POST /_rustybin/harness/import` (the route of step 08; `format` query, YAML body), catalogue and OpenAPI |
| Console | the builder's file field takes `.har`, `.json`, `.yaml`, `.yml`; a mapped `pattern` and `timeout` go into the form with the source; `left_out` and `variables` are shown |
| Management MCP | `import_scenario` (a new tool at the end of `TOOLS`: `document` as text, `format`); `import_traffic` stays |
| CLI | `rustybin harness import <file> [--format]` |
| `rustybin.yaml` | none: an import is a conversion, its result is a scenario (the `harness` section) |

### Tests that fail first

`import_tests.rs`: `a_postman_collection_becomes_weighted_requests`,
`postman_scripts_and_auth_are_named_never_run`, `postman_variables_are_resolved_or_the_request_is_left_out`,
`an_artillery_script_maps_requests_and_phases`, `artillery_phases_are_all_or_nothing`,
`artillery_code_and_plugins_are_named_never_run`, `no_credential_survives_a_collection_or_a_script`,
`a_program_is_refused_with_the_reason`, `collections_and_scripts_are_bounded`,
`an_imported_source_validates_as_a_scenario`; the route test `the_import_route_reads_yaml_and_the_format`;
the tool test `import_scenario_takes_text`; the Hurl examples `harness_import_postman` and
`harness_import_artillery`.

## Part B: HTTP/3 targets

### What and why

A gateway that terminates QUIC cannot be measured over TCP. The load client learns HTTP/3 for the
plain HTTP sources, behind a cargo feature, so the default binary gains no QUIC stack.

### Design

- **The parameter**: `targets[].http: h3` (`HttpVersion::H3`), in the model, the schema (so the MCP
  tool schemas and the console form follow) and the docs in every build.
- **The feature**: `harness-http3`, not in `default`. Without it `Scenario::validate` refuses
  `http: h3` with a message that names the feature; the node's status says `http3: false`.
- **Crates**: `quinn` (QUIC, tokio runtime, rustls with the aws-lc-rs provider the repository
  already uses, no ring, no platform verifier), `h3` and `h3-quinn`. The licence policy the
  repository enforces is `cargo audit` in CI (there is no `deny.toml`); MIT and Apache-2.0 crates
  under an AGPL-3.0 crate are fine.
- **The same safety path**: nothing in `safety.rs` changes and nothing is resolved twice. A target
  is parsed, checked against `RUSTYBIN_HARNESS_TARGETS` and pinned by `safety::pin` exactly as
  today; the QUIC endpoint connects to those pinned socket addresses and nowhere else (no DNS, no
  `Alt-Svc`, no connection migration to another address, no redirect). The allowlist entry's port
  is the UDP port. Forbidden addresses are refused by the same pin. Target verification runs over
  HTTP/3 too (the TCP port of the same name may be another service). The kill switch stops the
  scheduler as for any run; the endpoint is closed with the run.
- **TLS**: `tls_config_with` gains the `h3` ALPN; verification is the same code (web PKI roots, or
  `insecure: true` for a demo gateway). HTTP/3 needs `https`.
- **Requests**: `build_request` is unchanged, so the marker, the request id and `traceparent` are
  on every request. The answer's headers go through the same tag check, `Server-Timing` and
  rate-limit reading, and the outcome through the same classification (`upstream`, `gateway`,
  `transport` with its kinds) and recording.
- **What HTTP/3 does not carry here**, refused at validation by name: the protocol sources
  (`llm`, `sse`, `websocket`, `grpc`, `mcp`), the faults `connection_churn` and `http2`, and
  `identities.source_addresses`.
- **Connections**: a small fixed set of QUIC connections per target (round robin over the pinned
  addresses), reconnected on error; streams are bounded by the in-flight cap.

### Surfaces

The scenario model is the surface: the API (`validate`, `preview`, `runs`), the schema, the console
form, the MCP tools, the CLI's `harness run` and the `harness` section of `rustybin.yaml` all take
`http: h3`. `GET /_rustybin/harness` reports `http3`. CI gains a `harness-http3` job (clippy and
tests with the feature) and the MSRV job checks the feature.

### Tests that fail first

Without the feature: `h3_is_in_the_schema_and_refused_without_the_feature`. With it
(`http3_tests.rs`, an in-process HTTP/3 server on loopback, ephemeral UDP ports, the demo PKI of
`test_support`): `a_run_over_http3_is_marked_classified_and_reported`,
`http3_needs_https_and_plain_http_sources`, `an_http3_target_outside_the_allowlist_is_refused`,
`http3_goes_to_the_pinned_address_only`, `http3_targets_are_verified_over_http3`,
`the_kill_switch_stops_an_http3_run`, `a_closed_udp_port_is_a_transport_error`.

## Part C: server-to-server chains

### What and why

A request through a gateway often goes on: service A calls service B through the gateway again. To
measure that, a server node answering a marked request calls a further hop, and the report gains the
leg of each hop. This is the first outbound traffic from the server role, so the safety model comes
first.

### The safety model (written before any code)

The rules, each with where it is enforced:

1. **Off unless declared.** A server calls on only when the run's `servers.chain` names hops. No
   setting turns chains on for requests outside a run.
2. **Only for the run's own requests.** The call is made by the correlation layer, after
   `Marker::verify` with the run key. A request without a marker, with a marker of an unknown run,
   or with a signature that does not verify makes no call (it is served as an ordinary request, as
   today).
3. **Only while the run is live.** `RunEntry::is_done` (set when the run ends on a single node, and
   when the leader stops listing it on a cluster member) refuses the call, and so does the entry's
   deadline (the run's planned duration plus a grace), so a node that never hears the end still
   stops. A marker replayed after the run makes no call.
4. **Only to a hop the scenario names.** The destination is `chain.hops[n]`, a URL of the scenario,
   where `n` is the hop count of the verified marker. No address, host, path, query, header or body
   of the incoming request is read to choose it or copied into it: the onward request is a `GET` of
   the hop's URL with the harness's own headers.
5. **Through the pinned, allowlisted path.** When a server learns the run it prepares the chain
   with `safety::prepare_chain`: each hop is parsed with `parse_target_url` (never a control plane
   path), checked against this node's `RUSTYBIN_HARNESS_TARGETS` and pinned with `safety::pin`
   (forbidden addresses refused), and its client is the engine's `PinnedTarget` (the pinned
   connector, no redirects, no second DNS answer). There is no other client. With
   `RUSTYBIN_HARNESS_VERIFY_TARGETS` each hop must also serve the verification token, as a client
   target must. A chain that fails any of this is not half-prepared: the node makes no onward call
   for the run.
6. **A hop count in the signed marker.** The marker gains `hop`, inside the signed message (only
   when above 0, so a marker of a run without a chain is byte for byte what it was). A hop cannot be
   lowered or removed without the run key.
7. **A small fixed depth.** `MAX_CHAIN_HOPS` is 3. Validation refuses a longer chain, `Marker::parse`
   refuses a larger hop count, and a marker at the end of the chain makes no call.
8. **No loops, fan-out of one.** A server calls on at most once for a client request at a hop: the
   pair (client node, sequence number) is kept in a bitmap per hop, and a second arrival (a gateway
   retry, a mirrored request, a replay inside the run) makes no call. A request the bitmap cannot
   track (past its cap) makes no call either. So the onward calls of a run never exceed the requests
   its clients sent times the depth, however a gateway routes them. Validation also refuses the same
   hop twice in a chain.
9. **Bounded on the server.** `chain.max_concurrent` onward calls in flight per run on a node
   (default 64, at most 1,024); one over it is refused, not queued. `chain.timeout` per call
   (default 5s, at most 30s). Bodies read are capped.
10. **The kill switch.** `KillSwitch::is_set` is checked before a call and the call is raced
    against `KillSwitch::notified`, so `POST /_rustybin/harness/stop` ends onward calls at once on
    the node, as it ends the client's.
11. **Clusters.** The chain is part of `servers` (the `ServerProfile`), so it travels with the run
    in the server assignment and the replicated snapshot like the rest of the profile, and every
    server node prepares it against its own allowlist: a leader cannot aim a member elsewhere.

What a refusal looks like: the incoming request is still answered (the chain is a measurement, not
the request's purpose), and the refusal is counted by reason in the report (`not_live`, `kill_switch`,
`seen_before`, `untracked`, `busy`, `not_prepared`).

### Design

- **Model**: `servers.chain: { hops: [url | {url, host, http, insecure}], timeout, max_concurrent }`.
  A hop is a `Target` without a weight. `http: h3` on a hop is refused (the server role has no QUIC
  client).
- **Validation** (`Scenario::validate`): 1 to 3 hops, each a valid target URL, no duplicates, the
  bounds above; a chain needs a node with the server role when the run starts. Starting a run on a
  single node prepares the chain before anything is sent and fails the start (`403`, the problems by
  hop) when a hop is not allowed or not verified, exactly as for a target.
- **Preview**: `chain` in the preview and in `validate`'s summary lists the hops in order.
- **The onward call** (`harness/chain.rs`): made after the marker is verified and before the handler
  runs, so the server's processing time includes its dependency, as a real service's would. The
  onward marker keeps the run, the client node and the sequence number (so the next server can
  refuse a repeat) and raises `hop`. The next server answers with the usual tag, which the caller
  verifies: a tagged answer reached a server, an untagged one came from the gateway.
- **The report**: `server.chain.hops[n]`: calls, answers by class (`upstream`, `gateway`,
  `transport` by kind) and by status, the latency of the leg, the next server's processing time and
  the difference (the gateway's part of the leg), refusals by reason, and arrivals of that hop.
  A hop's arrivals are counted there and not in `server.arrivals`, so `lost` and `upstream_reach`
  still describe the client's requests. Captures follow `CaptureDecision` as for any marked request.
- **Metrics**: `rustybin_harness_chain_calls_total{result}`.

### Surfaces

The scenario model again (API, schema, console form, MCP tool schemas, CLI, `rustybin.yaml`), plus
the report (JSON, text) and the docs.

### Tests that fail first

`chain_tests.rs`: `a_chain_reaches_every_hop_and_reports_each_leg`,
`a_request_without_a_valid_marker_makes_no_onward_call`,
`a_marker_of_an_ended_run_makes_no_onward_call`, `a_forged_hop_count_does_not_verify`,
`a_hop_not_named_by_the_scenario_is_never_called`, `the_depth_is_fixed_and_a_repeat_calls_nothing`,
`the_kill_switch_stops_onward_calls`, `a_hop_outside_the_allowlist_refuses_the_run`,
`an_unverified_hop_refuses_the_run`, `onward_calls_are_bounded_per_run`,
`a_cluster_hands_the_chain_to_its_servers`; in `scenario.rs` the schema parity and
`chains_are_validated`; in `safety.rs` `a_chain_is_prepared_whole_or_not_at_all`.
