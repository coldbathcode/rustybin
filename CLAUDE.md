# Rustybin: notes for coding agents

HTTP stub service for API and AI gateway demos (Rust, axum 0.8, tokio). Crate is lib + bin.

## Harness mode: phase 1 built, phase 2 next

[docs/plans/harness.md](docs/plans/harness.md) (planned 2026-10-22; phase 0, the engine spike in
`examples/harness_spike.rs`, and phase 1, the single-node engine in `src/harness/`, done 2026-10-25; the plan's
"Phase 1 build notes" list where the build differs; phase 2, correlation, is next): Rustybin as a load client and
correlating upstream, single node (loop-through) or a cluster (an elected leader, clients, servers), traffic layers
with run patterns and rate-limit burst attacks, every parameter from one schema in the API, MCP and console (tunable
live), or headless. It is the first feature that sends load to targets the caller chooses (A2A push notifications,
the leads webhook and the key registry URL already send outbound requests, with their own allowlists), so
`src/harness/safety.rs` (allowlist, pinning, forbidden addresses, verification, kill switch) comes first and every
new way to aim load goes through it. One model, one schema: a scenario parameter lives in `scenario.rs` (model,
validation, `schema()`), and the parity tests fail when the schema, the MCP tool schemas or the console builder
(`ui/lib/scenarioform.js`, renders every non-shorthand schema property) miss it. Update the plan's status and
`docs/src/roadmap.md` ("Next") as phases land.

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
docs/examples/run.sh                         # documentation examples (Hurl) against fresh servers (open, plan, secured, shared, keyed)
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
  inspector capture, body limit, fault injection. JSON 404 fallback.
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
- `src/control.rs`: `/_rustybin/config`, `/_rustybin/version`, `/_rustybin/ready` (readiness, never authenticated),
  `git_sha()` / `build_info()` (build.rs sets `RUSTYBIN_GIT_SHA`), `is_control_path()` (`/_rustybin/*`, `/ui/*`).
- `src/control_oauth.rs`: OAuth sign-in to the management MCP server (`RUSTYBIN_CONTROL_OAUTH*`), see above.
- `src/control_auth.rs`: `RUSTYBIN_CONTROL_AUTH` (`open` / `token` / `jwt`), Ed25519 JWT verification with scopes
  (`inspector`, `console`, `admin`), the `enforce` middleware (protects `/_rustybin/*` except `/_rustybin/ready` and
  the OAuth sign-in endpoints `/_rustybin/oauth/{register,authorize,token}`; `/ui/*` static files stay open). Tests sign tokens with the keys in `tests/common/control_jwt_keys.rs`
  (`test_support::control_jwt`).
- `src/metrics.rs`: `Metrics` (in `AppState`), the `track` middleware (route label = `MatchedPath` template),
  `/_rustybin/metrics`. Every label is bounded; WebSocket handlers hold a guard from `metrics::open_ws`.
- `src/logging.rs`: `RUSTYBIN_LOG_FORMAT` (`text` / `json`), `logging::subscriber(config, writer, ansi)`.
- `src/ui.rs` + `ui/`: the web console under `/ui` (vanilla ES modules and CSS, no Node build step;
  `build.rs` embeds every file under `ui/`), plus `/_rustybin/catalog` and `/_rustybin/status`. `index.html` gets
  `<meta name="rustybin-*">` tags (control auth mode, console title, back link); `ui/lib/auth.js` handles the
  control token (`#token=` fragment, sessionStorage, sign-in screen) and `http.js` sends it as a bearer on
  `/_rustybin/*` calls. Browser check: `conformance/ui/run.sh` (Playwright; open mode plus a jwt-mode instance).
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
  `/_rustybin/mocks*` handlers over `service.rs`; access through `selfservice.rs`). Self-service (`RUSTYBIN_MOCKS_SELF_SERVICE`):
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
  `rustybin mcp-stdio`).
- `src/harness/`: harness mode (`RUSTYBIN_HARNESS`), see above. `mod.rs` (roles, `check_startup`, the `Harness`
  state in `AppState.harness` with bounded runs, target verification, router, catalogue, OpenAPI, headless mode),
  `safety.rs`, `scenario.rs` (model, `Caps`, validation, JSON Schema), `pattern.rs` (`constant`, `ramp`, `then`),
  `engine.rs` (own tokio runtime, a scheduler thread per run, `PinnedConnector` with TLS and ALPN, per-worker
  histogram shards rolled into a per-second timeline, closed-model users, saturation), `report.rs` (JSON, text,
  JUnit), `api.rs` (handlers; 404 when off except status, schema, scenarios and validate), `cli.rs` (`rustybin
  harness run | validate | report`), `scenarios/*.yaml` (built-ins). MCP tools in `src/mcp/manage/harness.rs`
  (`Right::Harness`), console view `ui/views/harness.js`. The `harness` control scope (`control_auth::Grant`).
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
  `RUSTYBIN_CONTROL_AUTH` is `token` or `jwt`; data-plane routes never are. The console must not use
  `EventSource` for control-plane feeds (no headers): use `send({stream: true})` + `sseParser`.
- `GET /` must always return 200 (platform liveness check), `GET /_rustybin/ready` too (readiness).
  `/health` is a demo toggle that may return 503.
