# Step 03: Record a mock from a real upstream

## Goal

"Mock the customer's own API in five minutes": Rustybin sits in front of a real API as a recording
proxy, the presenter (or a test suite) sends traffic through it, and the recording becomes a mock with
an OpenAPI spec and stored examples taken from the real answers.

## Where things stand

- Captures exist (`src/inspector.rs`, the mocks' capture log), and stored examples per operation
  (`OperationSettings.examples`, `StoredExample` with a source).
- A spec can come from a description (`spec_from_description`), a URL (`mocks/fetch.rs`) or a file;
  harness traffic can be imported from HAR and captures (`harness/import.rs`).
- Secret-looking headers and query values become `{{NAME}}` variables in saved tests
  (`replay::request_from_capture`).
- Nothing forwards traffic to a real upstream and keeps what came back.

## Model

- A recording: `name`, `upstream` (a base URL), `mock` (the mock it will become), state (`recording`,
  `stopped`, `built`), the exchanges kept (request and response, bounded: 500 exchanges, 256 KiB per
  body, an hour unless stopped), owned by the caller like a scenario run.
- The proxy: `ANY /record/{name}/{*path}` forwards to the upstream through `egress::Egress` under a new
  `Purpose::Record` with no default allowlist, pinned addresses, no redirects followed (a redirect is
  recorded as an answer), a timeout, the caller's request headers minus hop-by-hop and minus
  Rustybin's own; the answer goes back to the client unchanged and is kept.
- Redaction on the way in, before anything is kept: `Authorization`, cookies, API key headers and
  query values become `{{NAME}}` as saved tests do; response `Set-Cookie` is dropped; the kept data
  never holds a credential and is never written to an audit entry.
- Build: the exchanges are grouped into operations (method plus a path template inferred from varying
  segments: numbers, UUIDs, and segments that vary across exchanges), and a spec is inferred by code:
  parameters from the template and query keys, request and response schemas inferred from the JSON
  bodies (types, required when always present, enums for small fixed sets, formats for dates, UUIDs
  and emails), one stored example per status per operation (`source: recorded`). The result is a
  draft revision of the mock, compiled and checked like any other (`Svc::compiling`), with the deploy
  report. The OpenAPI version follows the instance's default.
- Optional polish: "Improve with AI" runs the existing `edit_spec` task on the inferred draft, through
  the AI-built mocks' diff; never automatic.

## Surfaces

| Surface | What changes |
|---|---|
| API | `/_rustybin/recordings` (list, start), `/_rustybin/recordings/{name}` (get, stop, delete), `/_rustybin/recordings/{name}/build` (with `dry_run`), and the data plane `/record/{name}/{*path}`; permissions `mocks.write` |
| Console | A "Record from an API" entry on the Mocks view: the upstream URL, the proxy URL to copy, live exchanges as they arrive, Stop, Build with the spec diff |
| Management MCP | `start_recording`, `get_recording`, `stop_recording`, `build_mock_from_recording`, `delete_recording`; recorded bodies under `untrusted_*` |
| CLI | `rustybin record start <name> --upstream <url>`, `record status | stop | build | delete` |
| `rustybin.yaml` | Nothing: a recording is not instance state, the built mock is (it exports as any mock) |

Docs: a new page `docs/src/mocks/record.md`, Hurl examples recording against an in-process upstream
(this instance's own petstore), "On every surface" table.

## Owner decisions (master plan section 6)

- Inference by code first; AI assist optional.
- Self-service visitors do not get recording (refused by permission, and the route is not
  `.self_service()`).

## Rules

- Reaching out is `Purpose::Record`, `mocks.write`, the operator's allowlist; `egress::dry_check`
  answers before a recording starts so the console can say why a host is refused.
- A built mock's examples come from a real system: the docs say to review them before publishing the
  mock to the portal, and the build answer lists every value that looked like personal data (emails,
  phone numbers, card numbers) with its operation, without the value.

## Tests that fail first

- Recording against an in-process upstream: the exchanges are kept, the client's answers are
  byte for byte the upstream's.
- `Authorization: Bearer xyz`, a cookie and `?api_key=` never appear in what is kept, exported or
  answered.
- A host outside the allowlist is refused before any connection; a redirect is not followed.
- Build: `/pets/1`, `/pets/2`, `/pets/abc` give `/pets/{id}`; the inferred spec compiles and every
  recorded request passes the mock's own request validation.
- The caps: the 501st exchange is refused with a reason, a large body is truncated and the example
  marked so.

## Done when

A recording against a real API builds a working mock on every surface, nothing secret is kept, and
the page shows the whole loop with tested examples.

## Build notes (2026-11-11)

Built whole with the recommended answers (inference by code, no recording for visitors).
Where the build differs from the plan above:

- **Tighter memory bounds.** 8 recordings (not 16), 64 KiB kept per body (not 256 KiB), and
  4 MiB of bodies per recording in all; what is over is forwarded but not kept (`not_kept`).
  At the plan's numbers a full instance could have held gigabytes.
- **The proxy's address carries a random id**, never the recording's name: the proxy is a data
  plane route (`/record/{id}/...`) and must not be guessable, or anyone could use the instance to
  reach an allowed host.
- **Starting never reaches out and never refuses for the policy.** It answers what
  `Egress::dry_check` says (`egress`), and the proxy refuses each request until the host is
  allowed, counted in `refused`. This keeps the CLI's examples runnable and tells a person what
  to allow before any traffic.
- **Permissions**: listing is `mocks.read`, what a recording kept is `traffic.read` (it is
  captured traffic, as in the inspector), starting, stopping, building and deleting are
  `mocks.write`. The ceilings follow (the read-only tool `get_recording` counts as traffic).
- **The build creates a new mock; it never replaces one.** A name in use is refused through the
  ordinary create (`409`), so the build goes through `Svc::create` and the compile pool with no
  path of its own.
- **Personal data** is found by email, phone and card number (Luhn checked) patterns in the
  stored examples and named by kind and operation.
- **The docs example cannot record for real**: no docs instance allows an outbound host, which
  is the point of the purpose having no default. The example shows the flow up to the refusal;
  the record, forward byte for byte, redact and build loop is the integration test
  `a_recording_through_the_proxy_builds_a_working_mock` against the instance's own petstore.
- **Improve with AI** stays the mock page's existing AI edit; the recording panel links to it in
  words rather than adding a second entry.

### For the merge

New module `src/recordings/` (`AppState.recordings`, `Purpose::Record`, `ExampleSource::Recorded`),
four control routes and the proxy route (catalogue, OpenAPI, README tables, `NEW_SINCE_0_4`,
the management `COVERAGE` table), six tools at the end of `TOOLS` (the frozen rights table and
the documented counts: 151 tools for an admin), `rustybin record`, the console's Mock APIs view.
