# Plan: OpenAPI mocks ("spec bins")

Status: phases 0 to 6 implemented (2026-10-17); mock secrets hidden from read-only callers
(2026-10-18); the remaining gaps from an audit against sections 3 to 10 closed (2026-10-18: safe
reloads, failed file mocks listed with their error, `406`, `insufficient_scope` challenges, the reserved
`/_rustybin` sub path, mock metrics, more deploy checks, captured path parameters, draft diff count,
delay slider). Hosted self-service (open question 1) is the next build, planned in
[hosted-mocks.md](hosted-mocks.md). See section 12 for
where the build differs from this plan. Last updated: 2026-10-18.

## 1. Goal

Let a user upload an OpenAPI (OAS) document and get a working, named mock API in a
few clicks: a stable base URL derived from a name, every operation in the spec
answering with a valid response, every call captured like a request bin, and
optional mocked security in front of it. Users manage all their mocks in one
place (on, off, edit, deploy, delete) from the web console, and can drive the same
thing from a YAML file or the CLI.

The console experience is the priority: someone who has never opened the docs
should be able to drop a spec in, press one button and paste a working URL into a
gateway.

### What users can do

1. **Create a mock from a spec**: upload, drag and drop, or paste OAS 3.0 / 3.1
   (JSON or YAML). The name defaults to a slug of `info.title` and sets the base URL,
   for example `http://localhost:8080/mock/petstore`.
2. **Choose how responses are produced**, per mock and per operation:
   - examples from the spec,
   - data generated from the response schema (deterministic, plausible values),
   - a response the user builds with a schema-aware editor,
   - simple rules ("if path param `id` is `0`, answer `404`").
3. **Deploy with confidence**: before a draft goes live, Rustybin generates a valid
   request and response for every operation, validates both against the spec and
   shows a report. Fix or accept warnings, then deploy.
4. **Manage all mocks**: a list with on/off switches, request counts, last hit,
   security badge, search and bulk actions. Turning a mock off keeps its config and
   history but makes its URL answer `503`.
5. **Toggle mocked security**: enforce the spec's `securitySchemes` (API key, basic,
   bearer, OAuth2 / OIDC via the built-in IdP) or a simple override, with demo
   credentials shown right in the UI.
6. **Watch traffic**: every call to a mock is captured (same model as request bins),
   with the matched operation, validation result and the response that was sent.
7. **Configure as code**: a `rustybin.yaml` that declares mocks (spec path plus
   overrides), loaded at startup, exported from the UI, and managed with
   `rustybin mock ...` CLI subcommands.

### Non-goals (v1)

- Fetching specs from a URL on the server. Server-side fetches were removed for SSRF
  reasons; the CLI and the browser read the file locally and upload its contents.
- Following spec callbacks / webhooks (outbound calls).
- External `$ref`s (other files or URLs). Bundled specs only; the CLI can bundle.
- Swagger 2.0 (added in phase 6).
- Durable storage by default. Mocks live in memory plus whatever the YAML declares
  (optional persistence directory, added in phase 6).

## 2. How it fits the existing code

| Existing piece | Reuse |
|---|---|
| `src/request_bin.rs` (`RequestBins`, `BinRequest`, ring buffer, SSE feed) | Capture store for mock traffic. Factor the per-bin ring + broadcast into a reusable `CaptureLog` so mocks and bins share it. |
| `src/ai/schema.rs` (`generate`, `validate`) | Lift into a shared `src/schema/` module and extend for OpenAPI (see 4.4). The AI module keeps calling it. |
| `ui/lib/schemaform.js` | Grows into the response builder: nested objects, arrays, `oneOf` pickers, formats. |
| `ui/views/bins.js` | Traffic panel pattern (list + detail + live feed) for the mock detail page. |
| `src/jwt_state.rs`, `src/oidc.rs`, `auth_*` modules | Mocked security: JWT verification against the built-in IdP keys, demo clients and users, constant-time key compare. |
| `src/fault.rs` | Already applies to all data-plane routes, so `X-Rustybin-Delay` / `X-Rustybin-Fail` work on mocks for free. |
| `src/admin.rs`, `src/control_auth.rs` | Mock management lives on the control plane (`/_rustybin/mocks*`): reads need `console`, mutations need `admin`. |
| `src/collections/*` | Phase 6: export a mock as Postman / Bruno / Insomnia / Hurl / k6 collections. |
| `serde_yaml`, `reqwest` (already dependencies) | YAML spec + config parsing; CLI `push` to a remote instance. |
| `matchit` (already in the tree via axum) | Per-mock path matcher, compiled on deploy. Add as a direct dependency at the same version axum uses. |

## 3. Concepts and URLs

- **Mock**: a named, deployable unit = one OAS document + settings + per-operation
  overrides + security + a capture log.
- **Name**: `^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$`, unique per instance. Reserved:
  names that clash with reserved words (`_rustybin`, `ui`, ...). Rename is allowed;
  the old URL stops working.
- **Base URL**: `{origin}/mock/{name}`. Spec paths hang off it:
  `GET /pets/{id}` in the spec answers at `/mock/petstore/pets/42`. The spec's
  `servers[0]` path prefix (for example `/v1`) is kept by default and can be
  stripped with `strip_server_prefix: true`.
- **Draft vs live**: edits go to a draft. "Deploy" runs the checks and swaps the
  live revision atomically (`ArcSwap`-style `Arc<RwLock<Arc<Compiled>>>`). The
  last 5 revisions are kept for one-click rollback.
- **Enabled**: a live mock can be switched off. Off answers `503`
  `{"error":"mock disabled","mock":"petstore"}` (configurable to `404`) so gateway
  health checks and circuit breakers have something to react to.

### Data-plane routes (never authenticated by control auth)

| Route | Purpose |
|---|---|
| `ANY /mock/{name}` and `ANY /mock/{name}/{*path}` | The mock. Matching, validation, security and response generation happen inside the handler because the operation set changes at runtime. |
| `GET /mock/{name}/_rustybin/openapi.json` and `.yaml` | The deployed spec with `servers` rewritten to the mock's base URL, ready for a gateway or client to import. The `/_rustybin` sub path is reserved and never matched against operations or captured. |

### Control-plane routes (`/_rustybin/mocks*`, protected by `control_auth::enforce`)

| Route | Purpose |
|---|---|
| `POST /_rustybin/mocks/import` | Parse a spec, return a preview: title, version, suggested name, operations, security schemes, warnings. Creates nothing. Powers the wizard's review step and `rustybin mock validate`. |
| `GET /_rustybin/mocks` | List mocks with status, counts, last hit, security, limits. |
| `POST /_rustybin/mocks` | Create from spec + settings (optionally `deploy: true`). |
| `GET /_rustybin/mocks/{name}` | Full definition (draft and live revision ids, settings, overrides). |
| `PATCH /_rustybin/mocks/{name}` | Update settings or the draft (rename, mode, security, operation overrides, replace spec). |
| `POST /_rustybin/mocks/{name}/enable` and `/disable` | The on/off switch. |
| `GET /_rustybin/mocks/{name}/report` | The deploy checks of the draft (computed when the draft is saved). |
| `POST /_rustybin/mocks/{name}/deploy` | Run checks, publish the draft (`?force=true` deploys with warnings). |
| `POST /_rustybin/mocks/{name}/rollback` | Back to a previous revision. |
| `DELETE /_rustybin/mocks/{name}` | Delete. |
| `GET /_rustybin/mocks/{name}/operations/{op}/sample` | Generate a valid request and response for one operation (`?status=`, `?seed=`). |
| `POST /_rustybin/mocks/{name}/operations/{op}/validate` | Validate a candidate response (or request) body against the schema; used live by the editor. |
| `GET /_rustybin/mocks/{name}/requests`, `.../requests/{n}`, `.../requests/stream` | Captured traffic, same shape as bins. The console reads the stream with `send({stream: true})` + `sseParser`, not `EventSource`. |
| `GET /_rustybin/mocks/{name}/export` | The mock as a YAML document (`rustybin.yaml` fragment), spec embedded or referenced. |
| `POST /_rustybin/mocks/apply` | Declarative apply of a whole `rustybin.yaml` (create, update, optional prune). Used by `rustybin mock push`. |

`{op}` is the `operationId`, or `METHOD path` URL-encoded when the spec has none
(Rustybin synthesises a stable id such as `get_pets_id`).

## 4. Backend design

### 4.1 Module layout

```
src/mocks/
  mod.rs        router(), catalog(), openapi_paths(), MockStore, limits
  spec.rs       parse + normalise OAS 3.0/3.1 (serde_json::Value based), $ref resolution, warnings
  compile.rs    Compiled revision: matchit routers per method, operation table, precomputed samples
  respond.rs    choose status + body: rules, overrides, examples, generated, Prefer header
  validate.rs   request validation (params, headers, body) via src/schema
  security.rs   mocked security evaluation
  checks.rs     deploy checks and report
  config.rs     rustybin.yaml model, load at startup, apply/diff
  handlers.rs   data-plane + control-plane handlers
src/schema/
  mod.rs        moved from src/ai/schema.rs (generate, validate), plus:
  openapi.rs    OAS dialect: components refs, nullable, readOnly/writeOnly, discriminator, formats
```

State: `MockStore` is module-local and attached with `Extension` (no new `AppState`
field unless the console status endpoint needs it, in which case add one field as
bins do).

### 4.2 Spec parsing

- Accept JSON or YAML (sniff first non-space byte). Parse to `serde_json::Value`
  and normalise into a small typed model (`Operation { id, method, path_template,
  params, request_body, responses, security, tags, summary }`). Do not take a
  third-party OpenAPI crate dependency: most cover only 3.0, and we need lenient
  parsing with warnings, not hard failures.
- OAS 3.0 and 3.1. Detect from `openapi`; Swagger 2.0 gets a clear error message
  and a pointer to converters (until phase 6).
- `$ref`: local JSON pointers only (`#/components/...`), resolved lazily with a
  depth limit and cycle detection. External refs produce an error naming the ref.
- Warnings, never silent drops: operations without responses, unsupported
  `pattern`s, conflicting path templates (matchit rejects `/a/{x}` + `/a/{y}`),
  unknown security scheme types, servers with variables, and so on. The wizard shows
  these.
- Bounds: spec size (1 MiB normal, 256 KiB public mode), at most 500 operations,
  max `$ref` depth 32, YAML alias expansion capped (billion laughs).

### 4.3 Request handling pipeline (`ANY /mock/{name}/{*path}`)

1. Look up the mock (O(1) map). Missing: `404` with a hint. Disabled: `503`.
2. Strip the server prefix if configured, match `method + path` with the compiled
   matchit router. No path match: `404`; path matches but not the method: `405`
   with `Allow`. Both as `application/problem+json` and both captured.
3. Mocked security, if enabled (4.6). Failure: `401` / `403` with
   `WWW-Authenticate`, captured with the reason.
4. Request validation, by mode: `off`, `warn` (default: respond normally, add
   `X-Rustybin-Mock-Validation: failed` and record the errors in the capture), or
   `enforce` (`400` problem+json listing each error with a JSON pointer).
5. Pick the response (4.5), apply per-mock and per-operation `delay_ms`, render.
   Response headers declared in the spec are generated too. Always add
   `X-Rustybin-Mock: {name}` and `X-Rustybin-Mock-Operation: {op}`.
6. Capture: method, path, matched operation, params, headers, body (64 KiB cap),
   validation result, chosen status, response body (truncated), latency.

No `unwrap`/`expect` on this path. Every header value from config or spec is
validated when the revision is compiled, so rendering cannot fail at request time.

### 4.4 Schema-based generation and validation

Extend the existing generator (`ai::schema`, moved to `src/schema`) with:

- `#/components/schemas/...` refs (generic JSON pointer resolution against the spec
  root, not only `$defs`).
- OAS 3.0 `nullable`, `readOnly` (omitted from generated requests), `writeOnly`
  (omitted from responses), `example` / `examples`, `discriminator` + `oneOf`
  (pick a mapping, set the property), `additionalProperties`, `format` (uuid,
  date, date-time, email, uri, hostname, ipv4/6, int32/int64, byte, binary).
- `pattern`: best effort with a small generator for common shapes (character
  classes, quantifiers, anchors); unsupported patterns fall back to a placeholder
  and a deploy-check warning.
- **Deterministic output**: seeded by mock seed + operation id + status + request
  path params, so `GET /pets/42` always returns the same pet and a demo is
  repeatable. Properties whose name matches a path parameter (`id`, `petId`) echo
  the request value.
- Validation reuses `schema::validate`, extended for the same keywords, returning
  a list of `{pointer, message}` rather than the first error.

### 4.5 Choosing a response

Per operation, first match wins:

1. **Prefer header** (Prism compatible, handy for tests and gateway demos):
   `Prefer: code=404`, `Prefer: example=notFound`, `Prefer: dynamic=true`.
   Also `X-Rustybin-Mock-Status: 404` for clients that cannot set `Prefer`.
2. **Rules** (user defined, ordered): a condition on path param, query param,
   header or JSON body pointer (`equals`, `exists`, `matches`), then a status +
   response source. Example: `params.id == "0"` -> `404`.
3. **Operation override**: a static response the user built (status, headers,
   body) or a forced mode.
4. **Mock default mode**:
   - `examples` (default): the first `2xx` response's named example or `example`,
     falling back to generated when the spec has none.
   - `generated`: always from the schema.
   - `static`: return the user's body or, if none, an empty `2xx`.
   - Phase 6: `stateful` (in-memory CRUD keyed by resource path, bounded).
5. Content negotiation: pick the media type from `Accept` among the response's
   `content`; `406` only when `enforce` validation is on.

### 4.6 Mocked security

Off by default. When on, one of:

- `spec` (default when the spec declares security): honour the operation's
  `security` (or the global one), including alternatives (OR) and combined schemes
  (AND):
  - `apiKey` in header / query / cookie: any key from the configured list
    (default: one generated key, shown in the UI).
  - `http basic`: configured users (default `demo` / `demo`).
  - `http bearer`: any token from a list, or JWT verified against the built-in IdP
    (`jwt_state`), with optional `aud` / `iss` checks.
  - `oauth2` / `openIdConnect`: JWT from the built-in IdP; required scopes from the
    spec are checked against the token's `scope` claim (`403 insufficient_scope`).
    The served spec rewrites the flows' URLs to the built-in IdP, so a client can
    really obtain a token.
  - `mutualTLS`: verified client cert (reuses the mTLS module's header / TLS
    extraction).
- `api_key`, `basic`, `bearer`, `jwt`: one scheme on every operation, ignoring the
  spec, for specs that declare nothing.

Per-operation opt-out (`public: true`) for things like health endpoints. Secrets in
config may use `${ENV_VAR}` references. Keys are compared in constant time
(`control_auth::constant_time_eq`).

### 4.7 Deploy checks

`POST .../check` (and every deploy) walks each operation and reports:

- spec warnings from parsing,
- a generated **request** (params + body) that passes request validation, shown as
  a ready curl command,
- the **response** each configured source would return, validated against its
  schema (status by status for overridden ones),
- rules that can never match or reference unknown params,
- security config that cannot be satisfied (for example `oauth2` scopes but JWT
  verification off).

Errors block the deploy (unless `force`), warnings do not. The report is cached on
the revision so the console shows it instantly.

### 4.8 Limits (CLAUDE.md: every piece of state is bounded)

| | Normal | Public mode |
|---|---|---|
| Mocks per instance | 100 | 50 (mutations need the admin token) |
| Spec size | 1 MiB | 256 KiB |
| Operations per mock | 500 | 200 |
| Rules per operation | 20 | 10 |
| Static response body | 256 KiB | 64 KiB |
| Revisions kept | 5 | 2 |
| Captured requests per mock | 200 (1 MiB) | 50 (256 KiB) |
| Lifetime of UI/API-created mocks | none | 24 hours |

Mocks declared in `rustybin.yaml` never expire and count towards the instance cap.
Metrics: the route label stays the `MatchedPath` template (`/mock/{name}/{*path}`);
a separate `mock` label is only added in normal mode and is bounded by the mock cap.

### 4.9 Config file and startup

- `RUSTYBIN_CONFIG_FILE` (default: `./rustybin.yaml` if present) or `--config`.
  Invalid entries warn and are skipped, matching how `Config` treats bad env vars;
  a mock whose spec fails to parse is created disabled with the error visible in
  the console rather than aborting startup.
- `RUSTYBIN_MOCKS_DIR`: every `*.yaml` / `*.json` spec in the directory becomes a
  mock with default settings (name from file name). Zero-config path for Docker:
  `-v ./specs:/app/mocks`.
- Reload: `POST /_rustybin/mocks/apply` or `SIGHUP` re-reads the file and applies
  the diff (no restart, no lost traffic history for unchanged mocks).
- Optional sample: a built-in `petstore` mock (on by default outside public mode)
  so the feature is visible on first run and the catalogue examples have a target.

### 4.10 Registration checklist (per CLAUDE.md "Adding a module")

- `pub mod mocks;` and `ROUTERS` in `src/lib.rs`, `SOURCES` in `src/catalog.rs`,
  `MODULE_PATHS` / `MODULE_COMPONENTS` in `src/openapi.rs`.
- New catalogue category `category::MOCKS` in `CATEGORY_ORDER`.
- Catalogue examples target the built-in `petstore` mock; examples that depend on
  state use `.expect_status(..)` / `.skip_check(..)`.
- Regenerate README tables.

## 5. `rustybin.yaml` format

```yaml
version: 1

mocks:
  - name: petstore
    spec: ./specs/petstore.yaml        # path relative to this file; or spec_inline: {...}
    enabled: true
    strip_server_prefix: false
    responses:
      mode: examples                    # examples | generated | static
      seed: 42
      delay_ms: 0
      validate_requests: warn           # off | warn | enforce
      disabled_status: 503
    security:
      enabled: true
      mode: spec                        # spec | api_key | basic | bearer | jwt
      api_key:
        keys: ["demo-key", "${PETSTORE_KEY}"]
      basic:
        users: { demo: demo }
      jwt:
        issuer: builtin                 # verify with the built-in IdP keys
        audience: petstore
    operations:
      getPetById:
        rules:
          - when: { path.petId: { equals: "0" } }
            respond: { status: 404, example: notFound }
          - when: { header.x-tier: { equals: gold } }
            respond: { status: 200, body: { id: 1, name: "Goldie", tag: "vip" } }
        delay_ms: 150
      deletePet:
        respond: { status: 204 }
      listPets:
        public: true                    # no security on this operation

  - name: payments
    spec: ./specs/payments.json
    enabled: false
```

The schema is published as JSON Schema at `/_rustybin/mocks/config.schema.json` so
editors (YAML language server) give completion and validation.
`GET /_rustybin/mocks/{name}/export` emits exactly this format, so "build it in the
UI, commit the YAML" is a supported workflow.

## 6. CLI

Extend `src/main.rs` with subcommands (hand-rolled like today, or `clap` if the
surface grows; keep MSRV 1.88 in mind):

```
rustybin [--config rustybin.yaml]                 run the server (existing behaviour)
rustybin mock validate <spec>                     parse + checks, print warnings, exit non-zero on errors
rustybin mock init <spec> [--name n] [--generated] [--secure]
                                                  print a rustybin.yaml snippet for the spec
rustybin mock sample <spec> <operationId> [--status 200] [--request]
                                                  print a generated valid response (or request)
rustybin mock push [--server URL] [--token T] [--prune] [rustybin.yaml]
                                                  apply the file to a running instance
rustybin mock list|enable|disable|delete <name> [--server URL] [--token T]
```

`validate`, `init` and `sample` run fully offline (no server). Remote commands use
`RUSTYBIN_SERVER` / `RUSTYBIN_TOKEN` env vars as defaults and talk to the
control-plane API (admin token or `admin` scoped JWT). Remote commands read local
spec files and upload contents, which is how "spec from a URL" works without the
server fetching anything. Docker usage is documented as
`docker exec rustybin rustybin mock list`.

## 7. Console (UI) design

New nav entry under **Instance**: "Mock APIs" (`#/mocks`), placed above Request
bins. Vanilla ES modules like the rest of `ui/`, no build step.

### 7.1 Mocks list (`#/mocks`)

- Empty state is the upload target itself: a large drop zone ("Drop an OpenAPI
  file, paste it, or try the Petstore sample") so the first action is obvious.
- Table: name + base URL (copy button), title / version, operation count, on/off
  switch, security badge (lock icon + scheme), requests in the last hour,
  last hit ("12 s ago"), draft-changes dot. Row click opens the detail.
- Search, filter chips (On, Off, Secured, Has draft), bulk enable / disable /
  delete with confirmation.
- Toolbar: "New mock", "Import YAML", "Export all as YAML".

### 7.2 Create wizard (modal, three steps, keyboard friendly)

1. **Spec**: drop a file, browse, or paste. Parsing happens immediately via
   `POST /_rustybin/mocks/import`; errors point at the line when available.
2. **Review**: title, version, operations grouped by tag (method badges),
   detected security schemes, warnings in a collapsible list. **Name** field
   prefilled from the title, with the live base URL preview underneath and inline
   availability / format validation.
3. **Behaviour**: three radio cards with one-line explanations:
   "Use spec examples" (recommended), "Generate realistic data", "I'll define
   responses". A "Secure this mock" switch (pre-checked when the spec declares
   security) and a "Validate requests" select (Off / Warn / Enforce).

Primary button: **Create and deploy**. Secondary: "Create as draft". After deploy,
land on the detail page with a success banner containing the URL, a copy button
and a ready curl for the first `GET` operation.

### 7.3 Mock detail (`#/mocks/{name}`)

Header: name, base URL + copy, status pill (Live / Off / Draft changes), the on/off
switch, **Deploy** button (disabled without draft changes; shows a diff count),
overflow menu (Rename, Duplicate, Export YAML, Download spec, Rollback, Delete).

Tabs:

- **Operations**: grouped by tag, each row: method badge, path, summary, response
  source chip (Example / Generated / Custom / Rules), lock icon if secured, hit
  count. Selecting a row opens the operation editor on the right.
- **Operation editor** (the "help the user build it" part):
  - Status tabs for each documented response (`200`, `404`, ...), plus "Add
    status".
  - Source selector: Example (dropdown of named examples) / Generated (with
    "Regenerate" and seed) / Custom.
  - Custom opens a **schema-driven form**: fields for every property with types,
    enums as selects, formats with suitable inputs (date pickers, UUID with a
    "new" button), nested objects as collapsible groups, arrays with add / remove
    rows, `oneOf` as a variant picker, required markers and descriptions. A
    **Form / JSON** toggle keeps both in sync; JSON mode validates live (debounced
    call to `.../validate`) with errors pinned to fields.
  - "Fill from example" and "Fill with generated data" buttons to start from
    something valid rather than an empty form.
  - Response headers editor (reuses `headerEditor`), delay slider.
  - Rules builder: "When [path param v] [id v] [equals v] [0] respond [404 v]
    with [example notFound v]", drag to reorder, live "would match" preview
    against the last captured request.
- **Try it**: pick an operation, the request is prefilled from a generated valid
  sample (params, headers, body, auth with the demo credentials), send it, see the
  response and the validation result side by side. "Copy as curl".
- **Security**: master switch, mode, per-scheme panels showing the demo
  credentials with copy buttons ("Use this header: `x-api-key: demo-key`"),
  "Get a token" button that calls the built-in IdP for OAuth2 / JWT, and a
  per-operation public/secured matrix.
- **Traffic**: live list of captured calls (reuses the bins layout) with extra
  columns: matched operation, status, validation (tick / warning), auth result.
  Detail shows request, response sent, validation errors with pointers.
- **Deploy report**: last check result per operation (request sample, response
  sample, warnings), "Run checks" button, and revision history with rollback.

### 7.4 UX principles

- Every screen shows the URL to copy and a curl that works as is.
- Defaults that work: create + deploy with zero edits must produce a mock where
  every operation answers with a schema-valid body.
- Explain errors in user terms ("`price` must be a number, got "abc"") with a
  link to the field, never raw serde messages.
- Destructive actions confirm; on/off does not (it is reversible and instant).
- Works at phone width; accessible (labels, focus order, `aria-live` for toasts),
  light and dark themes via existing tokens.
- Mock management respects control auth: with `RUSTYBIN_CONTROL_AUTH` on and a
  `console` (read-only) token, edit controls are hidden and a hint explains why.

## 8. Phases

Each phase ends green on `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, docs examples and the include checker.

| Phase | Scope | Exit criteria |
|---|---|---|
| 0. Groundwork | Move `ai::schema` to `src/schema` (no behaviour change); factor the bin ring buffer + broadcast into a reusable capture log. | Existing AI and bin tests pass untouched. |
| 1. Core mocks (API only) | `spec.rs`, `compile.rs`, `respond.rs` (examples + generated), data-plane routes, control CRUD, enable/disable, deploy without checks, capture, served spec, built-in petstore, limits. | Petstore 3.0 and a 3.1 fixture: every operation answers schema-valid JSON; on/off works; catalogue and OpenAPI tests pass. |
| 2. Console v1 | List view, create wizard, detail with Operations (read-only), Traffic, on/off, delete, export YAML. | Playwright: upload sample, deploy, call it, see the request, switch off, see `503`. |
| 3. Builder and checks | Static overrides, rules, `Prefer` handling, request validation modes, deploy checks + report, revisions + rollback, schema form editor, Try it. | Deploy report lists a valid request and response for each operation; editing a custom body with an invalid field blocks deploy with a pointer. |
| 4. Mocked security | `security.rs` for spec mode and overrides, IdP integration, served spec flow URL rewrite, Security tab. | Hurl examples for API key, basic, bearer JWT with scopes (401, 403, 200). |
| 5. Config as code and CLI | `rustybin.yaml` load + `RUSTYBIN_MOCKS_DIR`, apply endpoint + reload, JSON Schema for the file, CLI subcommands, Docker docs. | `rustybin mock validate/init/sample` offline; `push` round-trips an export; compose example with mounted specs. |
| 6. Extras | Swagger 2.0 import, stateful CRUD mode, optional persistence (`RUSTYBIN_MOCKS_STATE_DIR`), collection exports for mocks, host-based routing (`{name}.mock.example.com`). | Individually scoped later. |

Rough size: phases 0 to 2 are the MVP worth demoing; 3 and 4 make it the
"super user friendly" version described above.

## 9. Testing

- Unit: spec parsing (3.0, 3.1, refs, cycles, external ref error, YAML bombs),
  matcher (static vs param priority, conflicts, trailing slash, percent
  encoding), generator determinism and validity (property test: for every fixture
  operation, `validate(generate(schema), schema)` is OK), validation pointers,
  rules, `Prefer`, each security scheme, limits and eviction.
- Fixtures under `tests/fixtures/openapi/`: petstore (3.0), a 3.1 spec with
  `type: [string, "null"]`, a spec with `allOf` / `oneOf` + discriminator, a spec
  with every security scheme type, a deliberately broken spec. Small files, no
  generation at test time, no writes outside `tempfile`.
- Integration (`tests/integration.rs`): start the server with a temp
  `rustybin.yaml`, call mocks over HTTP, reload, verify state.
- Docs: Hurl files under `docs/examples/mocks/` run by `docs/examples/run.sh`
  (open, plan and secured instances).
- UI: extend `conformance/ui/run.sh` with the wizard happy path and the on/off
  switch, in open mode and jwt mode (read-only token hides edit controls).

## 10. Documentation

- `docs/src/mocks/index.md`: what it is, 60-second quick start (console and CLI).
- `docs/src/mocks/responses.md`: modes, examples, generation, rules, `Prefer`.
- `docs/src/mocks/security.md`: each scheme with Hurl examples and the IdP flow.
- `docs/src/mocks/config.md`: `rustybin.yaml` reference, `RUSTYBIN_MOCKS_DIR`,
  reload, Docker / compose examples.
- `docs/src/mocks/cli.md`: subcommands.
- Update `docs/src/console.md`, `configuration.md`, `SUMMARY.md`, README tables,
  `CLAUDE.md` layout section.

## 11. Open questions

1. **Public / hosted mode**: names are a global namespace, but bins are
   session-scoped data-plane resources. Recommendation: v1 is control-plane only
   (self-hosted demos, the main use case); for hosted mode later, append a random
   suffix (`petstore-3fa9`) and scope ownership by session like bins.
2. **URL shape**: `/mock/{name}` (recommended, no clash with existing routes) vs
   host-based `{name}.mock.<domain>` (nicer for gateway upstreams, needs wildcard
   DNS and TLS; phase 6).
3. **Persistence**: is in-memory plus `rustybin.yaml` enough, or should UI edits
   survive restarts by default when a state directory is mounted?
4. **Relationship with request bins**: keep them separate views (recommended) or
   present bins as "mocks without a spec" in one list.
5. **Built-in sample**: ship `petstore` on by default, or only when
   `RUSTYBIN_MOCKS_SAMPLE=true`?

## 12. As built (differences from the plan)

- **Schema module**: the AI generator (`src/ai/schema.rs`) was left untouched. Mocks have their own
  OpenAPI-dialect generator and validator in `src/mocks/schema.rs` (reusing the AI module's plausible-string
  helper), so AI behaviour cannot regress.
- **Capture log**: mocks have their own bounded capture log (`src/mocks/capture.rs`) instead of a refactor of
  `RequestBins`; it records the mock's decision (operation, validation issues, auth result, response sent).
- **Path matching**: a small segment matcher (literal, `{param}`, `prefix{param}suffix`) instead of `matchit`, so
  conflicting templates load with a warning instead of failing.
- **Validation leniency**: `oneOf` needs at least one matching branch (overlapping branches are common), and
  `additionalProperties: false` inside an `allOf` part does not reject properties declared by sibling parts.
- **Routes**: the dry-run check is `GET /_rustybin/mocks/{name}/report` (a read, so `console` tokens can see it);
  added `GET/PUT /_rustybin/mocks/{name}/operations/{op}` (operation detail with inlined schemas for the editor,
  and per-operation settings), `GET /_rustybin/mocks/export` (all mocks), `POST /_rustybin/mocks/reload`,
  `DELETE /_rustybin/mocks/{name}/requests` and `GET /_rustybin/mocks/config.schema.json`. The names `import`,
  `export`, `apply` and `reload` are reserved.
- **Config file**: `RUSTYBIN_CONFIG_FILE` (or `rustybin --config <file>`, or `./rustybin.yaml` when it exists).
  `POST /_rustybin/mocks/apply` accepts `spec_inline` only; `rustybin mock push` inlines spec files.
- **CLI**: offline `validate`, `init`, `sample`, `check-config`; remote `push`, `list`, `enable`, `disable`,
  `deploy`, `delete`, `export`.
- **JSON key order**: `serde_json` now has `preserve_order`, so spec paths, properties and security requirements
  keep their declaration order everywhere (the MCP envelope renderer was updated to match).
- **Public mode**: management is control plane, so without an admin token public mode is read-only and the
  per-session mock cap was not needed; it returns with hosted self-service (open question 1).
- **CLI defaults**: `--server` defaults to `$RUSTYBIN_SERVER`, else `http://localhost` on `RUSTYBIN_HTTP_PORT`
  (the server's own default, so `docker exec <container> rustybin mock list` works; the image puts `/app` on
  `PATH`).
- **Control auth**: the `console` scope may call the two `POST` routes that change nothing (`import` and
  `.../operations/{op}/validate`); every other mutation still needs `admin`.
- **Console**: bulk enable/disable/delete, "Duplicate" (`POST /_rustybin/mocks/{name}/duplicate`), the rule
  "would match" preview against a captured call (`POST .../operations/{op}/rules/test`, allowed for `console`),
  drag to reorder rules, the uploaded spec as written (`GET /_rustybin/mocks/{name}/spec`) and per-operation hit
  counts are all built.
- **Phase 6**: Swagger 2.0 import (`swagger2.rs`), `stateful` mode (`state.rs`), persistence
  (`RUSTYBIN_MOCKS_STATE_DIR`, `persist.rs`), collections (Postman, Hurl, curl, `.http`, k6; `collections.rs`;
  Bruno and Insomnia were not added) and host-based routing (`RUSTYBIN_MOCKS_HOST`, `host.rs`).
- **Hidden secrets**: API keys, bearer tokens and basic passwords are only shown to callers who may change the
  mock (control-auth admin grant and the admin guard). Everyone else gets `"<redacted>"` in `settings` and
  `credentials` (with `"secrets_hidden": true`) and `<api-key>` / `<token>` / `<base64 user:password>`
  placeholders in sample requests, the deploy report, collections and YAML exports. `"<redacted>"` is rejected
  as a value so redacted settings cannot be saved back. Captured traffic still shows the headers clients sent,
  like request bins.
- **Reloads** (4.9): a file that cannot be read or parsed changes nothing; a bad entry (unknown field,
  wrong type, unreadable spec file) is skipped on its own and its mock keeps the last good revision. A file
  mock whose definition fails carries a `load_error` (list, detail, console badge and banner); a new one is
  created switched off and not deployed, so its URL answers `503` with the error. The API apply endpoint
  still refuses a whole document with an unknown field.
- **Metrics**: mocks are counted in their own series, `rustybin_mock_requests_total{mock,outcome}`
  (normal mode only, at most 1024 series), rather than a `mock` label on `rustybin_requests_total`.
- **Deploy checks** (4.7): rules that can never match (contradictory conditions) or are shadowed by an
  earlier catch-all rule, empty key or user lists for schemes in use, schemes that are not mocked,
  `pattern`s the generator cannot satisfy, and response header values that cannot be sent. Not checked:
  whether the instance can verify `mutualTLS` (it depends on the listener and `RUSTYBIN_MTLS_IN_HEADER`)
  and whether tokens will carry the configured `aud`.
- **Spec response headers**: framing and connection headers (`Content-Length`, `Transfer-Encoding`,
  `Connection`, ...) and invalid names are dropped with a parse warning.
- **JWT issuer**: `jwt.issuer: builtin` (the section 5 example) means "any issuer": the signature already
  ties the token to the built-in IdP.
- **Defaults kept**: the default API key is the fixed `demo-key` (not a generated one) so docs, tests and
  demos stay copy-pasteable; the switched-off answer is `application/problem+json` with a `mock` member
  rather than `{"error":"mock disabled"}`, like every other mock error.
- **Testing** (9): spec fixtures live inline in the test modules instead of `tests/fixtures/openapi/`; the
  mocks Hurl files run on the open and secured documentation instances (not the plan instance, which
  adds nothing mock specific). The deploy report shows each response's source and validity; the body
  itself is one click away in the operation editor and Try it.
- **Open questions** (section 11) were settled with the recommendations: control-plane management only,
  `/mock/{name}` URLs, in-memory plus `rustybin.yaml`, separate views for bins and mocks, and the `petstore`
  sample on by default outside public mode.

