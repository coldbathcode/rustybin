# Step 17: Secret requests, what step 12 left

## Goal

Three things step 12's build notes list as not built, each small, none changing the rule: a secret
is never a tool input or output, and only a person who may set a secret fulfils a request for it.

## A. A person hears about a waiting request

Today the card shows only inside Management. A signed-in person with `secrets.write` sees a count on
the gear from any view.

- The count comes from the route that already answers it (`GET /_rustybin/secret-requests`, its
  `pending`), never from `/_rustybin/status`, which answers callers who may not know requests exist.
- The console asks with its ordinary polling (`send`, never `EventSource`), only when the feature is
  on and the caller holds the permission, and stops when the tab is hidden.

## B. A note on a decline

- `POST /_rustybin/secret-requests/{id}/decline` takes an optional `note` (at most 300 characters,
  cleaned like the reason by `secret_requests::clean_reason`).
- The agent reads it in `get_secret_request` as `note`. It is a person's text, so it is not
  `untrusted_*`; the form says not to type the secret there, and a note is never in an audit entry.
- `rustybin settings decline <id> --note "..."`.

## C. The agent's own user sees its requests

Today a user whose agent asked sees the agent and not its request unless they hold `secrets.write`.

- The list and the single read answer a caller without `secrets.write` their own agents' requests
  (the rule of `agents::visible_to`), with the same fields.
- Such a caller may decline their own agent's request (it only takes away). Fulfilling keeps
  exactly what it needs today (`settings.write` and the key's group permission).
- This is a change of reach on four routes: the catalogue declares `.authenticated(&[])` with the
  body check listed in `rbac::need::BODY_CHECKS`, and `rbac::access_table` gets its rows.

## Surfaces

API, console (the count on the gear, the note field, the card for a user's own agents), management
MCP (`get_secret_request` answers `note`), CLI (`--note`), `rustybin.yaml` (nothing: a request is not
instance state).

## Tests that fail first

- A user without `secrets.write` lists only their own agents' requests, reads another's as `404`,
  declines their own, and is refused `403 permission_required` on fulfil.
- The note reaches the agent, is capped and cleaned, and a planted secret typed as a note is in no
  audit entry or log line (`nothing_secret_reaches_an_entry_or_a_file` grows a case).
- The browser check: the count shows outside Management, and goes when the request is resolved.

## Done when

The three are on every surface they touch, the parity and access tests pass, and
`docs/src/agent-mcp.md#a-secret-at-an-agents-request` describes them.
