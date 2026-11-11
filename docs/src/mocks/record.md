# Record a mock from a real API

Point a recording at a real API, send traffic through it, and Rustybin builds a mock of
that API: a spec inferred from the traffic, with what the real API answered as the
mock's examples. It is the quickest way to mock a customer's own API: no spec needed,
only a few requests.

1. **Start a recording** with a name and the real API's base URL. The answer is a proxy
   URL on this instance.
2. **Send traffic to the proxy URL**, as you would to the API (a browser, curl, a test
   suite, an app pointed at it). Each request goes to the real API and its answer comes
   back unchanged; the exchange is kept.
3. **Build the mock.** Requests with the same method and path shape become one
   operation (`/pets/1` and `/pets/2` are `/pets/{petId}`); parameters, request bodies
   and responses get schemas from what was seen (types, the fields always present as
   required, formats such as `date`, `uuid`, `email`); each status keeps up to two
   recorded answers as [stored examples](ai.md) (source `recorded`). The mock is compiled,
   checked and deployed like any other. `dry_run` answers the inferred spec and makes
   nothing.

```hurl
{{#include ../../examples/mocks/record.hurl:start}}
```

## Reaching the real API

The recording reaches out on your word, so the rules of a
[spec fetched from a URL](index.md#a-spec-from-a-url) hold:

- Starting, stopping, building and deleting a recording need `mocks.write`; reading what
  it kept needs `traffic.read`. A visitor of a public instance never records.
- The proxy goes through the [outbound policy](../concepts/control-plane-security.md)
  under the purpose `record`, which has no default: add the API's host to
  `RUSTYBIN_EGRESS_ALLOW`. Starting tells you what the policy says (`egress`), and until
  the host is allowed each request is refused with the reason:

```hurl
{{#include ../../examples/mocks/record.hurl:proxy}}
```

- Addresses are pinned and redirects are not followed: a redirect is recorded as the
  answer it is. The proxy waits 30 seconds for the API and passes on at most 8 MiB.
- The proxy URL carries a random id of the recording (96 bits), never its name, and it
  refuses everything once the recording is stopped (`410`) or deleted (`404`).

## What is kept

A recording keeps what the mock needs and nothing that would let someone call the real
API as you:

- **Request headers are not kept** (only the content type): `Authorization`, cookies and
  API key headers never are. They are still forwarded to the API, so it answers as it
  would.
- **Query values and JSON fields with secret-looking names** (`key`, `token`, `secret`,
  `password`, `signature`, `auth`, `session`, `cookie`, `credential`) become `{{NAME}}`,
  in requests and answers alike.
- **Response cookies are not kept.** Binary bodies are kept as their size only.
- At most 500 exchanges, 64 KiB of each body (a longer one is cut and marked
  `truncated`) and 4 MiB in all; what is over is forwarded but not kept (`not_kept`).
  A recording lasts an hour, at most 8 exist at once, and each belongs to the session
  that started it.

```hurl
{{#include ../../examples/mocks/record.hurl:list}}
```

## Building

```hurl
{{#include ../../examples/mocks/record.hurl:build}}
```

The build answer lists the operations it made (method, path template, how many
exchanges, the statuses seen), how many examples it stored, and **what looked like
personal data** in the recorded answers: email addresses, phone numbers and card
numbers, by kind and operation, never the value. The examples are what the real API
answered, so review them before you [publish the mock](../portal.md) or share an
export of it. A mock of that name must not exist yet: build under another name with
`mock`, or delete the old one first. **Improve with AI** on the mock's page (an
[AI edit](ai.md)) can rename operations and add descriptions afterwards, through a diff
you accept.

```hurl
{{#include ../../examples/mocks/record.hurl:delete}}
```

Deleting a recording never deletes the mock it built.

## On every surface

| Surface | Where |
|---|---|
| API | `GET`, `POST /_rustybin/recordings`, `GET`, `DELETE /_rustybin/recordings/{name}`, `POST .../stop`, `POST .../build`; the proxy `/record/{id}/...` |
| Console | **Mock APIs**, **Record from an API**: start, the proxy URL to copy, the counts, **Stop**, **Preview**, **Build mock**, **Delete** |
| CLI | `rustybin record list`, `start <name> --upstream URL [--mock NAME]`, `show`, `stop`, `build [--mock NAME] [--dry-run]`, `delete` ([reference](../cli.md#record)) |
| Management MCP | `list_recordings`, `start_recording`, `get_recording` (what it kept under `untrusted_recorded`), `stop_recording`, `build_mock_from_recording` (`dry_run`'s spec under `untrusted_spec`), `delete_recording` (confirmed) |
| `rustybin.yaml` | Nothing for the recording, which is transient; the mock it built exports as any mock |
