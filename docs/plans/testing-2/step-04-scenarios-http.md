# Step 04: Scenarios: CORS and header transformation

## Goal

Two more outcomes: the gateway answers CORS preflights for an allowed origin (and not for another),
and the gateway adds, removes or rewrites headers on the way up and down.

## Model

- `cors`: the check sends an `OPTIONS` preflight from an allowed origin and from a refused one (both
  parameters), then a simple request; expected: the allowed preflight answers the CORS headers
  without reaching the upstream (or reaching it, when the operator says the upstream answers CORS),
  the refused one gets none.
- `header_transformation`: the operator names a request header the gateway adds, one it removes
  and a response header it adds; the check sends the removed one, and reads the upstream capture and
  the answer.

## Surfaces

As step 03: catalogue rows, the docs page.

## Tests

As step 03, with the stand-in gateway answering preflights and transforming headers.

## Build notes (2026-11-03)

- **Test parameters have kinds now**: `catalog::Kind` (`Integer`, `Bool`, `Origin`,
  `HeaderName`, `HeaderValue`) on each `Param`; the catalogue entry and the run show
  `type` (plus `format` and `maxLength` for text, `minimum` and `maximum` for
  integers) and the default when there is one. `CheckBody` is the gateway URL plus a
  flattened map of parameters, and `params_of` takes that map: an unknown name is a
  `400` naming the parameters (it was serde's `unknown field`), a known one of another
  scenario or out of its kind or bounds is a `400` as before. Origins are checked and
  normalised as a browser sends them (`scheme://host[:port]`, http or https, at most
  200 characters, no path, query or credentials, lowercased; `refused_origin` must
  differ). Header names are checked by `http`, at most 64 characters, lowercased, and
  a list is refused (hop-by-hop headers, `Host`, `Content-*`, `Date`, `Server`,
  `Vary`, `Origin`, `X-Request-Id`, `X-Rustybin-*`, `Access-Control-*`): the instance
  or the check sets them. `add_request_value` is 1 to 200 visible ASCII characters.
  The CLI flags are derived from `PARAMS` in the dispatch (`--upstream-cors` is a
  switch); the MCP tool copies every parameter by name. A new test
  (`every_test_parameter_has_a_cli_flag_and_a_tool_property`) keeps the flag list,
  the usage line and the tool schema in step with `PARAMS`.
- **The instance's own CORS layer stays out of a `cors` run.** tower-http's
  `CorsLayer` answers every `OPTIONS` itself (the comment on `cors_layer` said it
  looked for `Access-Control-Request-Method`; corrected), so a preflight the gateway
  let through would never reach the run's mock nor the inspector, and every answer
  would carry `Access-Control-Allow-Origin: *` for any origin. `scenarios::own_cors`,
  outside both CORS layers, turns an `OPTIONS` to a live `cors` run's mock into a
  `GET` marked with an extension, `restore_preflight`, just inside `CorsLayer`, turns
  it back, and the instance's `Access-Control-*` and `Vary` headers are removed from
  that mock's answers. Sending the request through a second copy of the router was
  tried first and fails: `Router::layer` middleware runs after routing, so the path
  parameters were inserted twice. Every other path is untouched; with
  `RUSTYBIN_CORS_ORIGINS=off` nothing is held.
- **The upstream's CORS answer is the middleware's, not the mock's settings.** With
  `upstream_cors: true` the check records `allowed_origin` on the run
  (`upstream_answers_cors` in the run view) and `own_cors` adds
  `Access-Control-Allow-Origin` (that origin), `Vary: origin` and, on preflights,
  `Access-Control-Allow-Methods: GET, HEAD, OPTIONS`, the requested headers and a max
  age to the mock's answers. The origin is a per-check parameter, so baking it into
  the mock would mean recompiling and replacing a live mock on every check. The mock
  has `GET /cors` and an `OPTIONS /cors` answering `204`.
- **The checks**: `cors` sends three requests, labelled in the verdict
  (`label`): a preflight (`Origin`, `Access-Control-Request-Method: GET`) from each
  origin, then a `GET` from the allowed one. Four expectations: the allowed preflight
  `2xx` with `Access-Control-Allow-Origin` (the origin or `*`) and
  `Access-Control-Allow-Methods` with `GET` (or `*`); it did not reach the upstream,
  or, with `upstream_cors`, it did; the refused preflight answered without
  `Access-Control-Allow-Origin` for its origin or `*`; the `GET` answered `2xx` with
  it. `header_transformation` sends one `GET /headers` carrying
  `remove_request_header` with a value made for the check (`scn-<12 hex>`) and judges
  the first upstream capture's headers and the answer's: the request succeeded, the
  added header reached the upstream (with `add_request_value` when given), the removed
  one did not, the response header is on the answer. `Sent` keeps the answer's
  headers (64 at most, 512 characters each) and each capture's headers, never
  serialised: the expectations describe header values (absent, present, the allowed
  origin, `*`, another origin, the value the client sent) instead of repeating what
  the gateway wrote.
- **The stand-in gateway** now forwards every request header and returns every
  response header (hop-by-hop ones dropped), and grows a CORS policy (preflights
  answered for one origin, or let through) and header rules (remove a request
  header, add a response header). The tests: both scenarios pass through a gateway
  that behaves; both fail with their expectation named through a pass-through proxy
  and against the instance itself; `upstream_cors` passes through a pass-through
  proxy and fails on a gateway that answers preflights itself; custom origins
  (normalised) and header names with a value; an instance with
  `RUSTYBIN_CORS_ORIGINS=off`; the instance's CORS still on for other mocks;
  parameter validation.
- **Docs**: two rows, the setup words and a table of every test parameter in
  `gateway-scenarios.md`, a Hurl example of a refused origin
  (`scenarios.hurl:origins`), the catalogue count 9 in the Hurl file and
  `conformance/ui` (which also prepares a CORS run, checks its text fields and
  checkbox, and sees a path in an origin refused).
