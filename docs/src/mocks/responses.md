# Responses, rules and validation

The examples on this page use a small shop API created like this (two named
examples for `200`, one for `404`, and a `POST /carts` with a request body):

```hurl
{{#include ../../examples/mocks/responses.hurl:create_shop}}
```

## How a response is chosen

For each request that matched an operation (and passed [mocked security](security.md)
and, in `enforce` mode, request validation), the first of these that applies wins:

1. **The client's preference.** `Prefer: code=404` picks the status,
   `Prefer: example=lamp` a named example, `Prefer: dynamic=true` generated data
   (several directives can be combined: `Prefer: code=404, dynamic=true`). Clients
   that cannot set `Prefer` send `X-Rustybin-Mock-Status: 404` and
   `X-Rustybin-Mock-Example: lamp`. A status or example preference skips the rules
   and overrides below.
2. **Rules** of the operation, in order: the first rule whose conditions all hold
   answers with its `respond`.
3. **The operation's `respond` override**: a fixed status, example, body and
   headers for every call.
4. **The default**: the lowest documented `2xx` (`200` when none is documented),
   with the body produced by the response mode of the operation (`mode`) or of the
   mock (`responses.mode`).

The status chosen by a preference, rule or override then gets its body: a literal
`body` if one is configured, else the named `example`, else the mode. A status the
spec does not document answers an `application/problem+json` body when it is an
error status (empty otherwise); `204`, `304` and `1xx` are always empty. Response
headers documented in the spec are sent with values from their examples or
schemas; configured `headers` are added on top (and replace documented ones with
the same name). The media type follows `Accept` among the response's `content`
entries, JSON by default. Bodies are JSON or plain text: a non-JSON media type
(`application/xml`, `text/csv`) is served as declared when its example is a string,
and otherwise as `application/json` (Rustybin does not write XML or CSV from a
schema).

`X-Rustybin-Mock-Source` says what happened: `example:<name>` (a named example),
`example` (an unnamed one), `example:ai:<name>` or `example:generated:<name>` or `example:user:<name>` (a stored one), `generated`, `static` (a configured body), `empty`, or
`undocumented`, prefixed with `prefer `, `rule:<name or number> ` or `override `
when one of those chose the response.

```hurl
{{#include ../../examples/mocks/responses.hurl:default_example}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:prefer}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:undocumented}}
```

## Response modes

| `responses.mode` | Body of a documented response |
|---|---|
| `examples` (default) | The media type's `example`, else its first named example, else an example [stored with the mock](ai.md#examples-and-the-alert-when-a-spec-has-none) (written by AI, the generator or a person), else data generated from the schema (schema `example` values are used where they exist) |
| `generated` | Always generated from the schema; falls back to the examples only when there is no schema |
| `static` | Only bodies you configured (`respond.body`, `example`); anything else is empty |
| `stateful` | A small in-memory store behind the mock: what clients create, change and delete is what later reads return ([below](#stateful-mode)) |

A per-operation `mode` overrides the mock's. Examples are returned exactly as
written in the spec.

Generated data is **deterministic**: the same schema, seed and request give the
same body, so a demo or a test is repeatable. Values come from, in order, the
schema's `example` / `examples` (in `examples` mode), `const`, `enum`, `default`,
then plausible data for the type, `format` (uuid, date, date-time, email, uri,
hostname, ipv4, ipv6, byte, ...), `pattern`, numeric bounds, string lengths and the
property name (`name`, `email`, `city`, ...). `$ref`, `allOf`, `oneOf` / `anyOf`
(with `discriminator`), `nullable` and 3.1 type arrays, `additionalProperties`,
`minItems` / `maxItems` and `uniqueItems` are honoured; `readOnly` properties are
left out of generated requests and `writeOnly` ones out of responses.
`responses.seed` (default `1`) changes the data; the sample endpoint takes
`?seed=` to preview another seed.

Top-level properties named like a path parameter **echo the request's value** when
it fits the schema: `GET /products/5` generates `"productId": 5`, and with exactly
one `...id` parameter a property called `id` gets it too.

```hurl
{{#include ../../examples/mocks/responses.hurl:mode_generated}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:mode_static}}
```

### Stateful mode

In `stateful` mode the mock remembers data, keyed by the request path, so a demo
can create something and read it back:

| Request | Answer |
|---|---|
| `POST /pets` | Stores the body (on top of the documented response's example) under its id field, or a new id when the body has none: the next integer when the collection's ids are integers, else a UUID |
| `GET /pets` | The collection, in insertion order; `?limit=` and `?offset=` page it, and a wrapper object (`{"total": 9, "data": [...]}`) keeps its shape with `total` / `count` updated |
| `GET /pets/{id}` | The stored item |
| `PUT /pets/{id}` | Replaces the item (or creates it) |
| `PATCH /pets/{id}` | Merges the body's fields into the item |
| `DELETE /pets/{id}` | Removes it; the item is returned when the spec documents a response body |

A collection starts with the items of its list operation's example the first time
it is used. A missing item answers `404`, with the spec's own `404` response when it
documents one, else `application/problem+json`. Operations that do not look like
CRUD (a `POST` on an item, a path ending in an action) answer as in `examples` mode.
`X-Rustybin-Mock-Source` names the action (`state create`, `state read`, `state miss`,
...).

The store is bounded per mock (1,000 items and 2 MiB, 200 items and 256 KiB in public
mode): the oldest items are evicted to make room, an item larger than the whole
budget gets `507`, and collections count too, since a path parameter above the
collection (`/users/{uid}/pets`) opens one per URL. `GET /_rustybin/mocks/{name}/state`
shows what a mock stored and `DELETE` resets it. Stateful mode is not available for
[self-service](self-service.md) visitor mocks.

## When a spec has no examples

A mock reports its **example coverage**: the operations whose success responses have
no example, in the spec or stored. The detail page shows a banner with **Generate
with AI** and **Use generated values**, the deploy report carries a warning of class
`no_examples`, and `GET /_rustybin/mocks/{name}/coverage` answers it. See [Build
mocks with AI](ai.md#examples-and-the-alert-when-a-spec-has-none).

## Operation settings

`PUT /_rustybin/mocks/{name}/operations/{op}` replaces one operation's settings in
the draft (`null` or `{}` clears them); deploy to make them live. The same object
sits under `operations.<operationId>` in `PATCH` bodies and in
[`rustybin.yaml`](config.md#reference).

| Field | Meaning |
|---|---|
| `respond` | The operation's override: `status`, `example` (a named example of that status), `body` (JSON is serialised; a string is sent as is, labelled JSON when it parses as JSON, else text) and `headers` |
| `rules` | Ordered list of `{name?, when, respond}`; the first match wins (20 per operation, 10 in public mode) |
| `mode` | `examples`, `generated` or `static` for this operation |
| `examples` | Examples stored with the mock, by status: `{"200": [{"name", "value", "source": "user" or "ai" or "generated"}]}`; served where the spec has none for the status, and by name with `Prefer: example=<name>`. At most 8 per status. See [Build mocks with AI](ai.md) |
| `delay_ms` | Delay before answering (instead of `responses.delay_ms`), capped at 30 s (10 s in public mode) |
| `public` | No [mocked security](security.md#public-operations) on this operation |

`GET /_rustybin/mocks/{name}/operations/{op}` returns the operation with its
schemas inlined (what the console's editor builds its forms from), its settings,
default status, effective mode, check result, a sample request and its URL.

```hurl
{{#include ../../examples/mocks/responses.hurl:operation_settings}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:operation}}
```

## Rules

`when` maps request values to conditions; every condition must hold:

| Key | Value |
|---|---|
| `path.<name>` | A path parameter |
| `query.<name>` | A query parameter (the first one with that name) |
| `header.<name>` | A request header (case-insensitive name) |
| `body.<dotted.path>` | A value in a JSON body: `body.coupon`, `body.items.0.quantity` |
| `body` | The whole body, as text |

| Condition | Holds when |
|---|---|
| `equals: v` | The value equals `v`, compared as text (`13` equals `"13"`) |
| `not_equals: v` | The value is missing or differs from `v` |
| `exists: true` / `exists: false` | The value is present / absent |
| `matches: "regex"` | The value matches the regular expression (up to 256 characters) |

Several conditions in one entry must all hold (`{equals: "a", exists: true}`).
The rule's `respond` works like the operation's override; without `status` the
default status is used, without `body` or `example` the body comes from the mode.
Rules that can never match (an unknown path parameter, a body rule on an operation
without a request body) are reported as warnings by the deploy checks.

```hurl
{{#include ../../examples/mocks/responses.hurl:rules}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:body_rule}}
```

## Request validation

`responses.validate_requests` decides what happens to requests that do not match
the spec. Path, query, header and cookie parameters are checked (text is coerced to
the schema type first, so `?limit=5` is an integer), as are required parameters,
the request body's presence, content type, JSON syntax and schema.

| Mode | Effect |
|---|---|
| `off` | Nothing is checked |
| `warn` (default) | The mock answers normally, adds `X-Rustybin-Mock-Validation: failed` and records the issues in the capture's `validation` |
| `enforce` | `400` `application/problem+json` with an `errors` list of `{pointer, message}`, nothing else is answered; a request whose `Accept` excludes every media type of the chosen response gets `406` with the `available` types |

Pointers address a virtual document: `/path/productId`, `/query/limit`,
`/header/x-request-id`, `/body/items/0/quantity`.

Array and object parameters are read in the default `form` style (`?tags=a,b` or
repeated `?tags=a&tags=b`); `spaceDelimited`, `pipeDelimited` and `deepObject` are
not interpreted yet, so validate such operations in `warn` mode. One validation makes
at most 500,000 schema checks: a schema whose `anyOf` / `oneOf` alternatives refer back
to themselves stops there with a `validation stopped` issue instead of running for
minutes.

```hurl
{{#include ../../examples/mocks/responses.hurl:validation_warn}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:validation_enforce}}
```

In `off` and `warn` mode the media type falls back to JSON (or the first one
documented) when `Accept` matches nothing, so a strict client still gets an answer.

## Deploy checks

Every draft is checked when it is created or edited, and a deploy is refused
(`422`, the report in the body) while the checks report **errors** (unless
`?force=true`). For each operation the checks generate a valid **request**
(required parameters, a body, demo credentials when security is on) and render
every response the mock can send (the default, each rule and override, each
documented status), validating each body against its schema.

| Level | Examples |
|---|---|
| Error (blocks the deploy) | A configured `body` that does not match the schema; an `example` name that the status does not document |
| Warning | A spec example or generated data that does not match its schema; a rule on an unknown path or query parameter or on a body the operation does not have; a rule that can never match (`exists: false` on a path parameter, `equals` and `not_equals` of the same value, an `equals` value its `matches` rejects) or is never reached (an earlier rule matches every request); settings for an operationId the spec does not have; security on in `spec` mode while the spec declares none; empty API key or basic user lists for schemes in use; security schemes that are not mocked; a `pattern` too complex for the generator; a documented response header whose value cannot be sent; every parse warning (undeclared path parameters, operations without responses, unsupported keywords, server-owned headers such as `Content-Length`, ...) |

```hurl
{{#include ../../examples/mocks/responses.hurl:checks_block}}
```

`GET /_rustybin/mocks/{name}/report` returns the full report of the draft: `ok`,
`errors`, `warnings`, `spec_warnings`, mock-level `findings`, and per operation its
`status` (`ok`, `warning`, `error`), `findings`, the sample `request` and each
checked response (`source`, `status`, `body_source`, `valid`, `issues`). The
console's **Deploy report** shows it with a curl command per request.

```hurl
{{#include ../../examples/mocks/responses.hurl:report}}
```

## Samples and validation

Two read-only helpers power the console's editor and "Try it", and are handy in
scripts:

- `GET .../operations/{op}/sample` generates a valid request and the response the
  mock would send, validated: `?status=` picks a documented status, `?mode=`
  (`examples`, `generated`, `static`), `?example=` a named example and `?seed=` a
  seed.
- `POST .../operations/{op}/validate` validates a body:
  `{"kind": "response", "status": 200, "body": ...}` (the default kind) against the
  response schema, or `{"kind": "request", "body": ...}` against the request body
  schema. It returns `valid`, `has_schema` and the `issues` (pointers relative to
  the body).

```hurl
{{#include ../../examples/mocks/responses.hurl:sample}}
```

```hurl
{{#include ../../examples/mocks/responses.hurl:validate}}
```
