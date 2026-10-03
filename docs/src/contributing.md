# Contributing

## License and CLA

Rustybin is licensed under the AGPL-3.0, with a commercial license available
(see `LICENSING.md` in the repository). To keep dual licensing possible, every
contributor signs the Contributor License Agreement (`CLA.md`) once: a bot
comments on your first pull request with a one-line sign-off. You keep the
copyright in your contribution. The name and logo are covered by
`TRADEMARKS.md`.

## Build and test

```bash
cargo build
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test                                   # unit tests + tests/integration.rs, a few seconds
RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints   # regenerate the README endpoint tables
docs/examples/run.sh                         # the documentation examples (needs hurl)
mdbook build docs                            # the documentation site (needs mdbook)
```

The MSRV is Rust 1.86 (`rust-version` in `Cargo.toml`, the Dockerfile builder
image and the CI `msrv` job). `.cargo/config.toml` makes the resolver prefer
dependency versions compatible with it, so `cargo update` cannot silently raise it.

## Architecture

The crate is a library plus a thin binary:

- `src/main.rs`: the CLI (`--print-endpoints-markdown`, `--version`, `--help`).
- `src/lib.rs`: the module list, `ROUTERS` (every module's router) and `build_app`,
  which wraps them in the middleware stack (outermost first): request id, tracing,
  request id propagation, CORS, time-to-headers timeout, inspector capture, body
  limit, fault injection. Unknown routes get a JSON 404.
- `src/server.rs`: the HTTP (required), HTTPS and gRPC (optional) listeners and
  graceful shutdown.
- `src/config.rs`: `Config` from `RUSTYBIN_*` variables.
- `src/state.rs`: `AppState { config, jwt, certs, identity, inspector, health }`,
  with `FromRef` for each part.
- `src/catalog.rs`: the [route catalogue](concepts/catalogue.md) and its
  consistency tests; `src/openapi.rs` merges the per-module OpenAPI fragments.
- Cross-cutting helpers: `src/session.rs` (session key, client IP, request origin),
  `src/admin.rs` (admin guard), `src/fault.rs`, `src/inspector.rs`,
  `src/content_negotiation.rs`.
- Feature modules: one file or directory per area (`src/ai/`, `src/mcp/`,
  `src/a2a/`, `src/oidc/`, `src/graphql.rs`, ...), and the web console in
  `src/ui.rs` plus `ui/` (embedded at build time by `build.rs`).

## Adding a module

1. Create `src/foo.rs` with:
   - `pub fn router(state: &AppState) -> Router<AppState>`; create module-local state
     there and attach it with `Extension` (add an `AppState` field only when several
     modules must share the state);
   - `pub fn catalog() -> Vec<Endpoint>` describing **every** route it registers,
     with runnable examples (`.expect_status(..)` or `.skip_check(..)` for examples
     that do not return a normal response, `.websocket()` / `.sse()` for non-plain
     HTTP);
   - `pub fn openapi_paths() -> serde_json::Value` (and optionally
     `openapi_components()`).
2. Register it once in each of the three lists: `pub mod foo;` and `ROUTERS` in
   `src/lib.rs`, `SOURCES` in `src/catalog.rs`, `MODULE_PATHS` (and
   `MODULE_COMPONENTS`) in `src/openapi.rs`. New categories go in
   `catalog::category` and `CATEGORY_ORDER`.
3. Regenerate the README tables (`RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`).
4. Document it: a page under `docs/src/` (and `docs/src/SUMMARY.md`), with every
   request example as a Hurl file under `docs/examples/<area>/` included through
   `\{{#include file:anchor}}` and run by `docs/examples/run.sh`.

The catalogue tests fail when a `.route("...")` literal is missing from the
catalogue, a catalogue path or method is missing from the OpenAPI document, or an
example returns 404 or 405 against the full application.

## Rules

- Never write the em dash or en dash characters, in code, comments, docs or commit
  messages (use commas, colons, parentheses or " - "). CI checks every tracked text
  file.
- Vendor neutral: no gateway vendor branding in code or UI (documentation may name
  gateways as examples).
- Nothing reachable from a request may panic: no `unwrap()` / `expect()` on request
  paths.
- Every piece of in-memory state reachable from requests is bounded (a capacity cap
  plus a TTL or eviction).
- Escape user input reflected into HTML or XML, and validate header values.
- Keep tests fast: use `crate::test_support`, never generate RSA keys per test,
  never write into `certs/` or fixed `/tmp` paths (use `tempfile`).

## Conformance suites

Official SDKs and tools drive the protocol mocks; CI runs them with the pinned
versions in `conformance/requirements.txt`:

| Suite | Command | Clients |
|---|---|---|
| Mock LLM | `PYTHON=... conformance/ai/run.sh` | `openai`, `anthropic`, `google-genai`, `httpx` |
| MCP | `PYTHON=... conformance/mcp/run.sh [--inspector]` | the Python MCP SDK, optionally the MCP Inspector CLI (Node) |
| A2A | `PYTHON=... conformance/a2a/run.sh` | `a2a-sdk` |
| OAuth / OIDC | `PYTHON=... conformance/oauth/run.sh` | `httpx` |
| Web console | `PYTHON=... conformance/ui/run.sh` | Playwright (Chromium) |

## Continuous integration

`.github/workflows/ci.yml`: rustfmt, clippy, tests, the MSRV build, the Docker
build, the no-dash check, the documentation job (mdBook build and every
documentation example against a fresh server) and the SDK conformance job.
`.github/workflows/docs.yml` publishes the mdBook site to GitHub Pages on pushes to
`main`.
