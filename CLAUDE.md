# Rustybin: notes for coding agents

HTTP stub service for API and AI gateway demos (Rust, axum 0.8, tokio). Crate is lib + bin.

## Commands

```bash
cargo build
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test                                   # unit + tests/integration.rs, a few seconds
RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints   # regenerate README endpoint tables
cargo run -- --print-endpoints-markdown      # print them instead
RUSTYBIN_HTTP_PORT=8080 cargo run
docs/examples/run.sh                         # documentation examples (Hurl) against a fresh server
mdbook build docs                            # documentation site
python3 docs/tools/check_includes.py         # every docs include and example anchor resolves
```

MSRV is Rust 1.86 (`rust-version` in Cargo.toml, Dockerfile builder image, CI `msrv` job).

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
  (outermost first): request id, trace, request id propagation, CORS, time-to-headers timeout,
  inspector capture, body limit, fault injection. JSON 404 fallback.
- `src/server.rs`: `run(config)`, `start(config)`, `start_with_state(state)` -> `RunningServer`
  (bound addresses, `shutdown()`, `wait()`). HTTP bind failure is fatal; HTTPS and gRPC failures only warn.
  Ports may be 0. Graceful shutdown on SIGTERM/SIGINT with a 10 s grace period.
- `src/config.rs`: `Config` from `RUSTYBIN_*` env vars (invalid values warn and default), `Config::for_tests()`.
- `src/state.rs`: `AppState { config, jwt, certs, identity, inspector, health }`, `FromRef` for each part
  (handlers can keep extracting `State<Arc<Config>>`).
- `src/catalog.rs`: route catalogue, the single source of truth (landing page, `/export/*`, README tables)
  plus the consistency tests.
- `src/openapi.rs`: hand-written paths plus `MODULE_PATHS` / `MODULE_COMPONENTS` merge lists; methods are
  aligned with the catalogue (routes registered with `any()` are documented as get/post/put/patch/delete).
- `src/inspector.rs`: bounded ring buffer + broadcast feed of captured requests, `/_rustybin/requests*`.
- `src/control.rs`: `/_rustybin/config`, `/_rustybin/version`, `is_control_path()` (`/_rustybin/*`, `/ui/*`).
- `src/ui.rs` + `ui/`: the web console under `/ui` (vanilla ES modules and CSS, no Node build step;
  `build.rs` embeds every file under `ui/`), plus `/_rustybin/catalog` and `/_rustybin/status`.
  Browser check: `conformance/ui/run.sh` (Playwright).
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
- Per-client state is scoped with `session::session_key` (`X-Rustybin-Session`, else client IP).
- Global mutations call `admin::require_admin`: token set means it is required; no token is open in normal
  mode and forbidden in public mode (`RUSTYBIN_PUBLIC_MODE`).
- Control plane lives under `/_rustybin/*`, the web console under `/ui`; both are excluded from inspector
  capture and fault injection.
- `GET /` must always return 200 (platform liveness check). `/health` is a demo toggle that may return 503.
