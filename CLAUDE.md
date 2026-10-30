# Rustybin: notes for coding agents

HTTP stub service for API and AI gateway demos (Rust, axum 0.8, tokio). Crate is lib + bin.

## Next: Rustybin 0.2, the AI-assisted mocking service (in progress: steps 01 to 09 done)

[docs/plans/next/master-plan.md](docs/plans/next/master-plan.md) (planned 2026-10-29) is the plan for the next
build: twelve steps, one file each (`docs/plans/next/step-NN-*.md`): foundations (step 01, done 2026-10-29: the
settings store `src/settings.rs`, the egress policy `src/egress.rs`, the console's Management view
`ui/views/manage.js`; see the step file's build notes), the console locked with a username and password (step 02,
done 2026-10-29: `src/login.rs`, the one change of default; test and example instances that read the control plane
without a token set `RUSTYBIN_CONTROL_AUTH=open`), AI assist with the user's own key (step 03, done 2026-10-29:
`src/assist/`, see below; Anthropic, OpenAI, Gemini, OpenAI-compatible, or the built-in mock provider for tests;
the console's assist bar, drawer and `ui/lib/assist.js`), AI-built mocks (step 04, done 2026-10-29: the tasks in
`src/assist/tasks/`, the routes in `src/mocks/ai.rs`, stored examples and coverage in the mocks model and checks,
`ui/views/mocks/ai.js`; specs from a
prompt to the chosen OpenAPI version, example payloads with an alert when a spec has none, AI edits through a
diff), an MCP inspector for any server with every auth mode and a relay (step 05, done 2026-10-30:
`src/mcp/inspector/`, `ui/lib/mcpclient.js`, `ui/lib/mcpauth.js`, `ui/views/mcp.js`, see below), a mock MCP registry and registry
browsing (step 06, done 2026-10-30: `src/mcp/registry/`, `ui/views/registry.js`, see below), a dev portal (step 07, done
2026-10-30: `src/portal/`, `ui-portal/`, see below), AI agents management (step 08, done 2026-10-30:
`src/mcp/manage/agents/`, `ui/views/agents.js`, see below), the whole instance as `rustybin.yaml` with git source
control (step 09, done 2026-10-30: `src/configdoc/`, `src/sourcecontrol/`, see below), a `rustybin` CLI for remote
instances, management MCP parity, and the 0.2.0 release. The master plan's section 4 is
the build marker and section 5 the session protocol: build a step whole, update its status and build notes, update
the master table, commit as oliwynn with the step number in the message, push to main. Steps go only when the
user says go. Rules the foundations set: a feature that reaches out resolves through `egress::Egress` under its
`Purpose` (never its own `reqwest::Client`); a feature's configurable value is a `settings::Spec` (seeded by its
variable, stored override, `secret()` for credentials, never returned as a value); the console's instance
settings are forms from the settings schema (`ui/lib/settingsform.js`), one card per group on a Management tab.

## The whole instance as code, and source control (built)

Step 09 of the 0.2 plan (user docs `docs/src/configuration-as-code.md`, `docs/src/source-control.md`). `rustybin.yaml`
version 2 is `MocksFile` (`src/mocks/config_file.rs`) grown by `instance`, `settings`, `secrets` (references only),
`harness` and `health`; `mocks` is an `Option` (an absent section is left alone, a present one is the truth when
pruning). `src/configdoc/` owns `export` (secrets as `${VAR}` or `secret:stored`, never values; the `git` settings
group is never exported nor pruned), `diff` (a section-aware list of changes, answered by every apply), `apply`
(each section through its own service: the settings store, `MockStore::apply` with `Origin::Api`, the registry's
file copy on the `MockStore` plus `AppState.registry` for published entries, `Portal::publish`, `Harness.saved`,
the agents policy, the health toggle; the registry and agents sections persist in `<state>/config.json`),
`apply_extra` (what a `RUSTYBIN_CONFIG_FILE` load applies beyond the mocks sections, from `mocks::reload_files`),
the bundle (`write_dir`, `tar_gz`), and the routes `/_rustybin/config/{export,apply,schema}`. A new feature with
instance state gets a section (or a setting) here, so a document stays the whole instance. `src/sourcecontrol/`
(`AppState.git`): the `git` binary in `<state>/repo`, the `git.*` settings seeded by `RUSTYBIN_CONFIG_GIT*`, a
token through `GIT_ASKPASS` or a deploy key through `GIT_SSH_COMMAND`, the remote's host under `Purpose::Git`,
`/_rustybin/git*`, startup from the repository (`SourceControl::startup` in `server.rs`) and the automatic mode
task; never a merge. Console: Management, Export (`ui/views/manage/export.js`) and Source control
(`ui/views/manage/git.js`).

## Load test mode (harness): built

[docs/plans/harness.md](docs/plans/harness.md) is built (planned 2026-10-22; phase 0, the engine spike in
`examples/harness_spike.rs`, phase 1, the single-node engine in `src/harness/`, and phase 2, correlation, done
2026-10-25; phase 3, the cluster, phase 3b, failover, and phase 4, patterns and live tuning, done 2026-10-26; phases
5, the rate-limit tests, 6, mock-driven traffic and identities, 7, faults and protocols, and 8, the docs
(`docs/src/harness.md`, the compose cluster `docs/examples/harness/cluster/`) and the conformance run
(`conformance/harness/run.sh`), done 2026-10-28; the plan's "Phase 1/2/3/3b/4/5/6/7/8 build notes" list where the
build differs; follow-ups are under consideration in `docs/src/roadmap.md`):
Rustybin as a load client and correlating upstream, single node (loop-through) or a cluster (an elected leader,
clients, servers), traffic layers with run patterns and rate-limit tests, every parameter from one schema in the
API, MCP and console (tunable live), or headless. It is the first feature that sends load to targets the caller
chooses (A2A push notifications,
the leads webhook and the key registry URL already send outbound requests, with their own allowlists), so
`src/harness/safety.rs` (allowlist, pinning, forbidden addresses, verification, kill switch) comes first and every
new way to point load goes through it. One model, one schema: a scenario parameter lives in `scenario.rs` (model,
validation, `schema()`), and the parity tests fail when the schema, the MCP tool schemas or the console builder
(`ui/lib/scenarioform.js`, renders every non-shorthand schema property) miss it. A new parameter also belongs in
the tables of `docs/src/configuration.md` and, when it changes how the harness is used, in `docs/src/harness.md`
with a tested example in `docs/examples/harness/`.

## MCP server for every OpenAPI mock (built)

[docs/plans/mock-mcp.md](docs/plans/mock-mcp.md) is built (phases 1 to 4, 2026-10-24; its section 13 lists where the
build differs; user docs in `docs/src/mocks/mcp.md`). `/mock/<name>/_rustybin/mcp` serves one tool per operation:
`src/mocks/mcp.rs` builds the tools from a `Compiled` revision (cached on it), `src/mcp/openapi.rs` is the
`Family::OpenApi` profile whose `tools/call` runs in process through `mocks::handlers::serve` (so REST and MCP never
drift apart; captures carry `channel: "mcp"` and the tool). A new mock feature that changes responses needs nothing
here; one that changes how requests are built (new parameter styles, body media types) belongs in both.

## Management MCP server (built)

[docs/plans/management-mcp.md](docs/plans/management-mcp.md) is built (phases 0 to 5, 2026-10-20; its section 13
lists where the build differs; user docs in `docs/src/agent-mcp.md`). `/_rustybin/mcp` lets an agent configure the
instance (`claude mcp add --transport http rustybin https://<host>/_rustybin/mcp --header "Authorization: Bearer
<token>"`). It is a second profile family on the mock MCP server's protocol and transport code (`src/mcp/manage/`),
separate from the mock server at `/mcp`. Its tools call `src/mocks/service.rs`, the layer the REST handlers use, so
a new mock feature belongs in the service layer and then in both. Keep the rules when adding tools: a `Right` per
tool and the role-filtered `tools/list`, annotations plus confirmation (elicitation, else `confirm: true`) on
destructive tools, captured or mocked content only under `untrusted_*` fields (truncated, never in the text
summary), audit lines without arguments. In-process requests from tools (`try_request` and friends) carry only
the caller's API key, never their control credentials (captures would expose them).

AI agents (step 08 of the 0.2 plan, `src/mcp/manage/agents/`): every `tools/list` and `tools/call` is attributed to
an agent (the caller's `Credential` from `Access` plus the client's `clientInfo` name) in `AppState.agents`, whose
rights (a ceiling, tool allow and deny lists: the agent's own, a rule of `rustybin.yaml`'s `agents:` section, the
`agents.default_ceiling` setting) apply after the credential's; revocation refuses calls, ends sessions and puts the
agent's token ids and OAuth subject in `login::Tokens`. `/_rustybin/agents*` (admin), console view `ui/views/agents.js`.
A new tool needs nothing here: ceilings follow its `Right` and `read_only`.

OAuth sign-in to `/_rustybin/mcp` ([docs/plans/control-oauth.md](docs/plans/control-oauth.md), 2026-10-21):
`src/control_oauth.rs` is a small authorization server for the control plane (metadata, stateless client
registration, PKCE, a consent API the console's `#/authorize/<id>` view calls with its own credential, HS256
tokens bound to the `/_rustybin/mcp` URL). Its tokens must stay accepted on `/_rustybin/mcp` only
(`control_auth::enforce` checks them there; `admin::admin_check` must never accept them).

## Hosted self-service mocks (built)

[docs/plans/hosted-mocks.md](docs/plans/hosted-mocks.md) is built (phases 0 to 7, 2026-10-18; its section 16 lists
where the build differs). Visitors of a public instance create their own mocks with `RUSTYBIN_MOCKS_SELF_SERVICE`
(off by default): a manage token per visit, 1 hour extendable to 3, then an email lead for up to 6 hours per UTC
day, visitor mocks only on `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` with restricted responses. What is left for the hosted
demo is deployment, not code: DNS and TLS for `rustybin-mock.dev`, the env vars, a lead webhook and a privacy page
(see `docs/src/mocks/self-service.md`). Candidates for later are in `docs/src/roadmap.md`. The OpenAPI mocks and
hosted mocks plans in `docs/plans/` are complete; keep `docs/src/roadmap.md` current when new work is planned.

## Commands

```bash
cargo build
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test                                   # unit + tests/integration.rs, a few seconds
cargo test --features quota-postgres         # + the PostgreSQL quota store (set RUSTYBIN_TEST_QUOTA_STORE to run against a database)
RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints   # regenerate README endpoint tables
cargo run -- --print-endpoints-markdown      # print them instead
cargo run -- mock validate src/mocks/petstore.yaml   # OpenAPI mocks CLI (offline: validate, init, sample, check-config)
cargo run --release --example harness_spike -- --sweep   # harness phase 0 spike: load sweep against an in-process Rustybin
cargo run -- harness run src/harness/scenarios/smoke.yaml --local-server   # a harness run looped through an in-process Rustybin
RUSTYBIN_HTTP_PORT=8080 cargo run
docs/examples/run.sh                         # documentation examples (Hurl) against fresh servers (open, plan, secured, shared, keyed, self-service, harness)
conformance/harness/run.sh                   # the compose harness cluster (docs/examples/harness/cluster): the leader killed mid-run, the report asserted
mdbook build docs                            # documentation site
python3 docs/tools/check_includes.py         # every docs include and example anchor resolves
```

MSRV is Rust 1.88 (`rust-version` in Cargo.toml, Dockerfile builder image, CI `msrv` job).

## Hard rules

- Never write the em dash (U+2014) or en dash (U+2013) anywhere: code, comments, docs, commit messages.
- Vendor neutral: no gateway vendor branding in code or UI.
- No `unwrap()` / `expect()` / panics on request paths (startup and tests are fine).
- Every piece of in-memory state reachable from requests is bounded (capacity cap plus TTL or eviction).
- HTML-escape user input reflected into HTML (`landing::html_escape`), XML-escape into XML, validate header values.
- Keep tests fast: use `crate::test_support`, never generate RSA keys per test, never write into `certs/` or fixed `/tmp` paths (use `tempfile`).

## Layout

- `src/main.rs`: thin CLI (`--print-endpoints-markdown`, `--version`), exits non-zero on fatal errors.
- `src/lib.rs`: module list, `ROUTERS` (every module's router), `build_app(state)` with the middleware stack
  (outermost first): mock host rewrite (wraps the finished router, `mocks::host`), request id, trace, metrics,
  request id propagation, CORS, control-plane auth, API keys (`keys::enforce`), plan limits, time-to-headers timeout,
  harness correlation (load test mode only), inspector capture, body limit, fault injection. JSON 404 fallback.
- `src/server.rs`: `run(config)`, `start(config)`, `start_with_state(state)` -> `RunningServer`
  (bound addresses, `shutdown()`, `wait()`). HTTP bind failure is fatal; HTTPS and gRPC failures only warn.
  Ports may be 0. Graceful shutdown on SIGTERM/SIGINT with a 10 s grace period. With `RUSTYBIN_GRPC_ON_HTTP`
  the HTTP/HTTPS app is wrapped in `grpc::GrpcOnHttp` (content-type `application/grpc*` goes to tonic).
- `src/config.rs`: `Config` from `RUSTYBIN_*` env vars (invalid values warn and default), `Config::for_tests()`.
- `src/state.rs`: `AppState { config, jwt, certs, identity, inspector, health, limits, metrics, keys, mocks }`, `FromRef` for
  each part (handlers can keep extracting `State<Arc<Config>>`).
- `src/catalog.rs`: route catalogue, the single source of truth (landing page, `/export/*`, README tables)
  plus the consistency tests.
- `src/openapi.rs`: hand-written paths plus `MODULE_PATHS` / `MODULE_COMPONENTS` merge lists; methods are
  aligned with the catalogue (routes registered with `any()` are documented as get/post/put/patch/delete).
- `src/inspector.rs`: bounded ring buffer + broadcast feed of captured requests, `/_rustybin/requests*`.
- `src/settings.rs`: the instance settings store (`RUSTYBIN_STATE_DIR/settings.json`, `RUSTYBIN_SETTINGS_LOCK`,
  secrets sealed with ChaCha20-Poly1305 under `RUSTYBIN_SECRETS_KEY` or `<state>/secrets.key`): `Spec` tables per
  feature joined by `settings::specs()`, env seed / stored / default resolution with `Source`, `secret()` for the
  feature that uses one, `{set, hint}` in the API, audit lines without values; `/_rustybin/settings` (GET, PATCH,
  DELETE `/{key}`, admin). `AppState.settings`.
- `src/egress.rs`: the outbound policy (`RUSTYBIN_EGRESS_ALLOW`, `RUSTYBIN_EGRESS_ALLOW_LOOPBACK`, the settings
  `egress.*`): the `Allowlist` grammar (shared with the harness, which re-exports it from `safety.rs`), forbidden
  addresses, `parse_url`, `pin_with`, a `Rule` per `Purpose` (A2A push, leads, key registry, assist, and the
  later registry, MCP relay, git), `resolve` then `client` (pinned addresses, no redirects), a bounded decision ring,
  `/_rustybin/egress` (admin). `AppState.egress`; A2A push, leads and the key registry resolve through it.
- `src/control.rs`: `/_rustybin/config`, `/_rustybin/version`, `/_rustybin/ready` (readiness, never authenticated),
  `git_sha()` / `build_info()` (build.rs sets `RUSTYBIN_GIT_SHA`), `is_control_path()` (`/_rustybin/*`, `/ui/*`).
- `src/control_oauth.rs`: OAuth sign-in to the management MCP server (`RUSTYBIN_CONTROL_OAUTH*`), see above.
- `src/login.rs`: the console sign-in (`RUSTYBIN_CONTROL_AUTH=password`, the default): `RUSTYBIN_CONSOLE_USER`,
  `RUSTYBIN_CONSOLE_PASSWORD` or `_PASSWORD_HASH` (argon2id), the settings `console.user` and the secret
  `console.password_hash`, `POST /_rustybin/login` (open) minting HS256 control tokens (the OAuth key's `console`
  subkey, scope `admin`, 12 h, `rb_use: "console"`, `jti`, `rb_gen`), `logout`, `session`, `sessions*`, `password`.
  `Tokens` (generation, revoked ids, listed sessions, failed attempts per address; all bounded) lives in
  `Config.console_tokens` because every credential check takes `&Config`; `control_auth::verify_jwt_claims` calls
  `login::verify` for sign-in tokens, so every caller accepts them. Public mode with the default password refuses
  to start (`login::check_startup`); the default password warns at startup and sets `default_password` in the status.
- `src/assist/`: AI assist (`RUSTYBIN_ASSIST_*`, the `assist.*` settings). `mod.rs` (`Assist` in `AppState.assist`:
  `specs()`, `status()` resolved from the settings at every call, `sync_egress` permitting the provider's host under
  `Purpose::Assist`, the per-caller calls-per-minute cap, bounded usage, the `suggest` cache, `run(RunParams) ->
  OutStream` with the validate-and-repair loop, `test()`), `provider.rs` (`Kind`, `Request`, `Event`, the `Client`
  enum, the shared `post_sse` through the egress policy, `SseParser`), `anthropic.rs` / `openai.rs` / `gemini.rs`
  (request bodies and event mapping; structured answers through tool use, `response_format`, `responseSchema`),
  `mock.rs` (the task's deterministic answer, streamed), `models.rs` (the curated table with tiers and list prices),
  `redact.rs` (prompts never carry a stored secret or the admin token), `task.rs` (`Task` with an optional `post`
  check beyond the schema, input helpers, action blocks, `parse_json_answer`, `validate` on `mocks::schema`),
  `tasks/` (`free_answer`, `explain`, `suggest`, and the mock tasks `spec_from_description`,
  `examples_for_operations`, `edit_spec`, `request_examples`, each with a deterministic `mock` answer and a `post`
  that parses, compiles and checks through `tasks::compile_doc` / `check_spec`; one file per task, `ALL`), `api.rs`
  (the routes; `RUN_PATH` is a read-only POST for control auth). A new AI feature is a new task file (and, when it
  writes, `writes: true`), never a prompt built elsewhere.
- `src/control_auth.rs`: `RUSTYBIN_CONTROL_AUTH` (`password` / `open` / `token` / `jwt`), Ed25519 JWT verification with scopes
  (`inspector`, `console`, `admin`), the `enforce` middleware (protects `/_rustybin/*` except `/_rustybin/ready` and
  the OAuth sign-in endpoints `/_rustybin/oauth/{register,authorize,token}`; `/ui/*` static files stay open). Tests sign tokens with the keys in `tests/common/control_jwt_keys.rs`
  (`test_support::control_jwt`). `admin::admin_check` answers 403 (not 401) to a valid control JWT without the
  admin scope, so the console is not signed out by an admin-only call.
- `src/metrics.rs`: `Metrics` (in `AppState`), the `track` middleware (route label = `MatchedPath` template),
  `/_rustybin/metrics`. Every label is bounded; WebSocket handlers hold a guard from `metrics::open_ws`.
- `src/logging.rs`: `RUSTYBIN_LOG_FORMAT` (`text` / `json`), `logging::subscriber(config, writer, ansi)`.
- `src/ui.rs` + `ui/`: the web console under `/ui` (vanilla ES modules and CSS, no Node build step;
  `build.rs` embeds every file under `ui/`, and `ui-portal/` for the dev portal), plus `/_rustybin/catalog` and `/_rustybin/status`. `index.html` gets
  `<meta name="rustybin-*">` tags (control auth mode, console title and back link from the `console.*` settings);
  `ui/lib/auth.js` handles the control token (`#token=` fragment, sessionStorage, sign-in screen) and `http.js`
  sends it as a bearer on `/_rustybin/*` calls. `ui/views/manage.js` is the Management view (the gear opens it;
  `TABS` is the rail later steps extend; `ui/lib/settingsform.js` renders a settings group from the schema).
  Browser check: `conformance/ui/run.sh` (Playwright; open mode plus a jwt-mode instance).
- `src/keys.rs` + `src/keys/`: API keys. `RUSTYBIN_SHARED_MODE` (keys on every data-plane route, per-key limits
  and daily quotas, inspector and usage scoped by key id, the key layer authenticates the control plane) and
  `RUSTYBIN_REQUIRE_KEY`. `registry.rs` (JSON registry, SHA-256 lookup), `ledger.rs` (bounded per-key daily
  counters), `store.rs` (`QuotaStore`; PostgreSQL behind the cargo feature `quota-postgres`, tests in
  `pg_tests.rs` need `RUSTYBIN_TEST_QUOTA_STORE=postgres://...`), `/_rustybin/keys/usage`. The key layer sits
  outside the limiter and attaches a `KeyContext` extension. Keys are redacted in captures (`redact_*`).
- `src/mocks/`: OpenAPI mocks. `spec.rs` (OAS 3.0/3.1 parsing into a normalised model, `serde_json::Value`
  based, lenient: odd specs give warnings), `schema.rs` (deterministic generation and validation in the OpenAPI
  dialect, readOnly/writeOnly by direction, path parameter echo), `model.rs` (settings: the same struct in the
  control API, `rustybin.yaml` and exports), `compile.rs` (immutable revision: spec + settings + route table +
  deploy report), `respond.rs` (Prefer header, rules, overrides, modes), `request.rs` (request validation),
  `security.rs` (mocked security, JWTs from `jwt_state`), `checks.rs` (deploy checks: a valid request and
  response per operation), `store.rs` (`MockStore` in `AppState`: draft, live, history, on/off, capture log with
  per-operation and hourly counts, stateful data; bounded by `MockLimits`), `state.rs` (`stateful` mode: bounded
  in-memory CRUD keyed by request path), `swagger2.rs` (Swagger 2.0 to OpenAPI 3.0 on parse), `persist.rs`
  (`RUSTYBIN_MOCKS_STATE_DIR`: API-created mocks saved as JSON, loaded by `new_store`), `host.rs`
  (`RUSTYBIN_MOCKS_HOST`: `<name>.<domain>` rewritten to `/mock/<name>` by a layer wrapping the finished router in
  `build_app`), `collections.rs` (a mock as Postman / Hurl / curl / .http / k6 / Bruno / Insomnia), `config_file.rs` (`rustybin.yaml`,
  `RUSTYBIN_MOCKS_DIR`, export), `cli.rs` (`rustybin mock ...`), `mcp.rs` (a revision's MCP tools), `service.rs` (the control-plane logic shared by
  the REST handlers and the management MCP), `handlers.rs` (`/mock/{name}/...` data plane, thin
  `/_rustybin/mocks*` handlers over `service.rs`; access through `selfservice.rs`). Self-service (`RUSTYBIN_MOCKS_SELF_SERVICE`): `ai.rs` holds the AI routes
  (`/_rustybin/mocks/ai/spec`, `/{name}/ai/examples`, `/{name}/ai/edit`, `/{name}/coverage`: assist tasks plus the
  mock context, SSE progress, `RUSTYBIN_ASSIST_VISITORS`). Examples stored with a mock live in
  `OperationSettings.examples` (`StoredExample`, source user / ai / generated); `respond.rs` serves them after the
  spec's and before generation; `checks::coverage` (the `no_examples` finding) says which operations lack one.
  `visits.rs` (`Owner` of every mock: instance, visit by manage-token hash, or API key; the visit clock and daily
  ledger on the injectable `limits::Clock`), `selfservice.rs` (`Caller`, `readable` / `manageable` guards: other
  people's visitor mocks are `404`; the `/_rustybin/mocks/visit*` and operator `visits*` routes), `restrict.rs`
  (visitor responses: safety headers, header denylist, inert media types), `leads.rs` (email leads to a webhook or
  file through a bounded retry queue; addresses are never logged). Mocks are matched inside the handler (operations change at runtime), never as
  axum routes. The built-in `petstore` sample (`petstore.yaml`, `RUSTYBIN_MOCKS_SAMPLE`) is what catalogue
  examples and tests call. File-defined mocks load in `server::start_with_state` and reload on SIGHUP.
- `src/mcp/`: the mock MCP server at `/mcp` (protocol, transports, sessions, fake tools), `src/mcp/openapi.rs` (the
  MCP server of each OpenAPI mock) and `src/mcp/manage/`, the
  management MCP server at `/_rustybin/mcp` (`tools.rs` registry, `access.rs` rights, `stdio.rs` for
  `rustybin mcp-stdio`). `src/mcp/inspector/`: the MCP inspector's server side (`mod.rs`: `GET
  /_rustybin/mcp-servers`, the routes, `is_console_post`; `client.rs`: `Target` (this instance in process
  through `state.router`, else the egress client under `Purpose::McpRelay`), `Transport` (modern, session,
  legacy), `Conn`; `relay.rs`: `POST /_rustybin/mcp-relay` (SSE back, bounded sessions) and `/fetch` for
  OAuth discovery documents; `checks.rs`: `CHECKS` and `run` behind `POST /_rustybin/mcp-checks` (JSON or
  JUnit); `store.rs`: saved connections and runs in `<state>/mcp-inspector.json`, credentials sealed with
  `Settings::seal`). `src/mcp/registry/`: the mock MCP registry at `/mcp-registry` in the registry API's
  `/v0.1` shape (`/v0` nested as an alias): `mod.rs` (routes, the `Registry` state with published entries
  and tokens, `Scenario` from the `registry.scenario` setting, the control routes `/_rustybin/registry*`),
  `entries.rs` (generated entries from `inspector::server_list`, scenarios, `page` with the cursor),
  `validate.rs` (the vendored `schema/server.schema.json` 2025-12-11 plus semantic checks),
  `browse.rs` (`GET /_rustybin/registry/browse` under `Purpose::Registry`, cached). File entries are the
  `registry.servers` of `rustybin.yaml`, kept on the `MockStore`. The console side is `ui/lib/mcpclient.js` (`McpClient`, browser or relay, exports),
  `ui/lib/mcpauth.js` (auth modes, the OAuth walk) and `ui/views/mcp.js`. A new check is a `CHECKS` entry
  plus its judgement in `run`, documented in `docs/src/mcp/inspector.md`.
- `src/harness/`: load test mode (`RUSTYBIN_HARNESS`), see above. `mod.rs` (roles, `check_startup`, the `Harness`
  state in `AppState.harness` with bounded runs, target verification, router, catalogue, OpenAPI, headless mode),
  `safety.rs`, `scenario.rs` (model, `Caps`, validation, JSON Schema, preview), `pattern.rs` (every run pattern
  compiled into stages with an exact value and integral, `then`, jitter, sync, wall-clock alignment),
  `limit_test.rs` (the `limit_test` pattern: its tests, their stages, identity pools, retries),
  `mocksource.rs` (a mock source resolved on the node into request variants, invalid requests and
  create-then-read pairs), `identity.rs` (a layer's API keys, minted JWTs, OAuth tokens, forwarded and
  source addresses), `breakdown.rs` (per-operation and per-credential results), `faults.rs` (the `faults`
  block: presets, client and server faults, the per-request draws), `protocols.rs` (the `llm`, `sse`,
  `websocket`, `grpc` and `mcp` sources, SSE parsing, WebSocket frames, gRPC framing, stream statistics),
  `ratelimit.rs`
  (rate-limit headers, the per-layer `LimitAcc` merged across nodes, the analysis in the report's `rate_limits`),
  `live.rs` (live changes: `Change`, the versioned `Controls` every node applies, the `LiveView` the scheduler
  evaluates, the stage log, the closed loops `target`, `breakpoint` and `limit_probe`), `saved.rs` (saved scenarios, `RUSTYBIN_HARNESS_STATE_DIR`),
  `engine.rs` (own tokio runtime, a scheduler thread per run, `PinnedConnector` with TLS and ALPN, per-worker
  histogram shards rolled into a per-second timeline by a roller thread, closed-model users, saturation, the marker on every
  request and the classification of every answer), `correlate.rs` (the run key, the signed marker and tag,
  digests, the `Correlator` with per-run server stats and the seen-sequence bitmap, the `layer` in `build_app`
  outside inspector capture, `CaptureDecision` the inspector honours), `shaped.rs` (`/harness/{*path}`, the
  run's `servers` profile), `report.rs` (JSON, text, JUnit; classes, overhead, legs, the server side),
  `api.rs` (handlers, the `PATCH` changes and the SSE run feeds; 404 when off except status, schema, scenarios,
  validate and preview), `cli.rs` (`rustybin
  harness run | validate | report`), `scenarios/*.yaml` (built-ins), `test_proxy.rs` (tests: a misbehaving
  gateway with reference rate limiters), `live_tests.rs` (patterns, loops and live changes through the test
  proxy), `limit_tests.rs` (the rate-limit tests against the proxy's limiters), `traffic_tests.rs` (mock
  traffic and identities), `fault_tests.rs` (faults, streams and the protocol sources). MCP tools in `src/mcp/manage/harness.rs` (`Right::Harness`), console view `ui/views/harness.js`,
  `rustybin_harness_*` in `metrics.rs`. The `harness` control scope (`control_auth::Grant`). Clusters:
  `cluster.rs` (the leader: `Coordinator`, `Cluster`, `ClusterRun`, split, merge, report), `member.rs` (the
  agent each member runs: join, heartbeat, lease, clock, and the candidate and voter rules), `wire.rs` (the
  protocol and the replicated snapshot), `election.rs` (the leader's ticker, promotion, stepping down),
  `cluster_tests.rs`, `failover_tests.rs` (with `test_net.rs`: partitions that refuse or drop calls); the
  node routes (`harness::is_node_path`: join, heartbeat, vote, announcement) authenticate with the join
  token or node credential and skip the control-plane auth, key layer and plan limits.
- `src/portal/`: the dev portal (`/portal`, `RUSTYBIN_PORTAL*`, the `portal.*` settings). `mod.rs` (`ItemId`:
  `mock:<name>`, `mcp:<server>`, `llm`, `a2a:<agent>`, `tools`, `registry`; `Published` with the mock's owner,
  `Meta` and `clean_meta`, `FileItem` for `portal.items` of `rustybin.yaml` kept on the `MockStore`, `Portal` in
  `AppState.portal`: items, insights, the catalogue cache, `<state>/portal.json`), `items.rs` (candidates, published
  entries, `hidden_reason`, the catalogue each kind's page reads), `insights.rs` (bounded counters), `api.rs` (the
  SPA from `ui-portal/`, `/portal/api/catalog`, `/events`, collections, `/portal/reference/{id}`, the control routes
  `/_rustybin/portal*`; `portal.auth` checked in `gate`). The portal SPA imports the console's `dom.js`, `format.js`,
  `markdown.js` and stylesheet from `../ui/`; never its auth or http code. `/portal/*` is a control path (no
  capture, no faults) and exempt from the key layer. The console side is `ui/lib/portal.js` (publish dialog) and
  the Portal tab of `ui/views/manage.js`.
- `src/fault.rs`: `X-Rustybin-Delay` / `X-Rustybin-Fail` middleware.
- `src/admin.rs`: `require_admin(&headers, &config)` guard for instance-global mutations.
- `src/session.rs`: `session_key(&headers, client_ip)`, `client_ip(...)`, extractors `Session`, `ClientIp`, `PeerAddr`.
- `src/test_support.rs` (`cfg(test)`): `test_state()`, `test_app()`, `module_app(router)`, `body_json(...)`, ...
- `tests/integration.rs`: starts the real server on ephemeral ports.

## Adding a module

1. `src/foo.rs` with:
   - `pub fn router(state: &AppState) -> Router<AppState>`; create module-local state here and attach it
     with `Extension` (do not add AppState fields unless several modules must share the state).
   - `pub fn catalog() -> Vec<Endpoint>` describing EVERY route it registers
     (`Endpoint::new(path, &["GET"], category::X, "summary").example(Example::get(...))`; use `"ANY"` for `any()`,
     `.websocket()` / `.sse()` for non-plain HTTP, `.expect_status(..)` / `.skip_check(..)` on examples that do not
     return a normal response).
   - `pub fn openapi_paths() -> serde_json::Value` (and optionally `openapi_components()`).
2. Register it once in each list: `pub mod foo;` and `ROUTERS` in `src/lib.rs`, `SOURCES` in `src/catalog.rs`,
   `MODULE_PATHS` (and `MODULE_COMPONENTS`) in `src/openapi.rs`. New categories go in `catalog::category`
   and `CATEGORY_ORDER`.
3. Regenerate the README tables (`RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`).
4. Document it in `docs/src/` (mdBook) with every request example as a Hurl file under
   `docs/examples/<area>/`, included with anchors and run by `docs/examples/run.sh`.

The catalogue tests fail when a `.route("...")` literal is missing from the catalogue, a catalogue path is
missing from the OpenAPI spec (or lacks a method), or an example returns 404/405 against the full app.

## Conventions

- Route syntax is axum 0.8: `/status/{code}`, wildcard `/echo/{*path}`.
- `serde_json` has `preserve_order`: object keys serialise in insertion order (OpenAPI documents keep their
  path and property order). Do not write tests that rely on sorted keys.
- Per-client state is scoped with `session::session_key` (`X-Rustybin-Session`, else client IP).
- Global mutations call `admin::require_admin`: token set means it is required; no token is open in normal
  mode and forbidden in public mode (`RUSTYBIN_PUBLIC_MODE`).
- Control plane lives under `/_rustybin/*`, the web console under `/ui`; both are excluded from inspector
  capture and fault injection. Control-plane routes are protected by `control_auth::enforce` when
  `RUSTYBIN_CONTROL_AUTH` is `password` (the default), `token` or `jwt`; data-plane routes never are. The console must not use
  `EventSource` for control-plane feeds (no headers): use `send({stream: true})` + `sseParser`.
- `GET /` must always return 200 (platform liveness check), `GET /_rustybin/ready` too (readiness).
  `/health` is a demo toggle that may return 503.
