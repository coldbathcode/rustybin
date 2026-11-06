# Step 01: One visitor never reads another's traffic

## Goal

Make `docs/src/concepts/public-instances.md` true: a client cannot read another client's traffic.
`traffic.read` is one of the anonymous reads, and the captures of an instance mock were not scoped,
so any caller on a public instance read every visitor's calls to the sample, credentials included.

## Model

- A capture records its `Sender` (a session or an API key), never in an answer.
- `Svc::traffic_scope` says what a caller reads: its own calls for a caller without a credential on
  a public instance and for an API key on an instance mock, else all of them.
- Every reader goes through the scope: `requests`, `request_entry`, the stream, `test_rules`,
  `get_mock_traffic`, `wait_for_requests`.

## Surfaces

No new route, tool or setting, so the parity tests are untouched. API, console, MCP and CLI all read
through the service. `rustybin.yaml`: none, captures are never in a document.

## Tests

`a_visitor_reads_only_its_own_calls_of_an_instance_mock`, `an_address_session_is_never_claimed_to_read_traffic`,
`a_visitor_reads_all_the_traffic_of_its_own_mock`, `a_traffic_stream_carries_only_the_callers_own_calls`,
`a_key_reads_only_its_own_calls_of_an_instance_mock`, `a_visitor_reads_only_its_own_calls_through_the_tools`;
the Hurl example `ss_own_traffic`.

## Build notes (built 2026-11-06)

- **Found by running it**: an anonymous sweep of the control routes under the demo's environment, then
  a second visitor reading the first's `Authorization`, `X-Api-Key`, `Cookie` and query string.
- **The rule tester was a reader too**: `rules/test` takes a capture number and answers with the
  values it saw, so it needed the scope as much as the list.
- **Shared mode had the same hole**: with self-service a key reaches `/_rustybin/mocks*`, so `Sender`
  has a `Key` variant. Two variants, not a prefixed string: a session name may hold a `:`, and
  `key:ka1` as a session would otherwise claim that key's traffic.
- **`total` differs by scope**: for `Own` it counts the caller's calls still in the ring, because the
  mock's count would tell a visitor how much others use it. The counters of a mock's detail
  (`requests_total`, `last_hit`, the per-operation stats) stay: they hold no content.
- **Not narrowed**: outside public mode an anonymous caller on an open control plane reads all
  traffic as in 0.4 (a team's bench), and signed-in users holding `traffic.read` read all of it.
- **A mock's own MCP server**: `serve` gets no key there and the built request has no session header,
  so those calls carry the address. The error is on the safe side: a caller may miss its own call,
  never see another's.
- **Seen to fail**: the four first tests failed before the fix; the key test and the wait assertion
  were added with it, so each was run with its protection removed and failed.
