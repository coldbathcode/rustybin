# Step 15: Client ID metadata documents

## Goal

The roadmap's oldest open line: "MCP inspector: client ID metadata documents". An MCP server whose
authorization server says `client_id_metadata_document_supported` is tested the way current clients
sign in to it: the client's id is an https URL of a document that describes the client, with no
registration call.

## Where things stand

- `ui/lib/mcpauth.js` walks discovery, then dynamic client registration (RFC 7591) when the server
  offers it, else asks for a client id. It never offers a metadata document.
- `src/control_oauth.rs` answers `client_id_metadata_document_supported: false` and registers
  clients statelessly.

## Part A: the inspector as a client (no decision needed)

- The instance serves its inspector's document at an open route,
  `GET /_rustybin/oauth/inspector-client.json`: `client_id` (that URL), `client_name`,
  `redirect_uris` (the console's own callback, from the instance's public base URL), `grant_types`,
  `token_endpoint_auth_method: none`. Open, like the other OAuth metadata; it holds nothing secret.
- The walk's step 4 picks in this order: a client id the user gave, a metadata document when the
  server supports it and this instance has an https base URL that is not loopback, dynamic
  registration, then asking. The step's detail says which was used and why the others were not, so a
  person sees why `http://localhost` cannot use a document.
- A choice in the auth form forces one way (document, registration, given id), to test a server's
  support for each.
- A check in `inspector::checks::CHECKS`: the server says it supports documents and accepts ours;
  judged in `run`, documented in `docs/src/mcp/inspector.md`.
- Saved connections keep the chosen way, never a token it did not keep before.

## Part B: the control plane accepting one (owner decision, section 6)

An agent's client names itself to `/_rustybin/oauth/authorize` with a URL. To honour it the
instance fetches that URL: a call out on the word of a caller who has not signed in yet.

Recommended, if built: off by default (`oauth.client_metadata_documents`), fetched through
`egress::Egress` under a purpose of its own with no default allowlist (so only hosts the operator
names), https only, no redirects, a body cap of 5 KiB, cached with a short life, and the consent view
shows the document's host and name as the client's own claim. `rustybin.yaml` never turns it on from
git (`REPOSITORY_REFUSED`). With it off the metadata keeps saying `false`.

Not recommended: the mock authorization server of `/mcp/protected` fetching a stranger's URL on the
data plane. If testing a client's document flow against the mock is wanted, it accepts a URL-shaped
client id without fetching and says so.

## Surfaces

API (the document's route in the catalogue and OpenAPI, `.open()`), console (the walk and the form),
management MCP (the inspect tools take the same choice), CLI (`rustybin mcp` flags follow the tool's
parameter), `rustybin.yaml` (a saved connection's choice; the setting of part B).

## Tests that fail first

- Against an in-process authorization server that supports documents: the walk uses ours, no
  registration call is made, the token is issued.
- The instance on `http://127.0.0.1`: the walk says why a document cannot be used and registers.
- The document's `redirect_uris` match what the walk sends, byte for byte.
- Part B: off by default; on, a host outside the allowlist is refused before any call; a redirect, a
  body over the cap and a `client_id` that differs from the URL are refused.

## Done when

The inspector signs in with a document where the server supports one, the check reports it, the
docs page shows the walk, and part B is built or its row of section 6 says no.

## Build notes (built 2026-11-12, parts A and B)

The owner said go for both parts, with the recommendation of section 6 for part B.

**Part A, where it lives.** `src/mcp/inspector/clientdoc.rs` (the document, `unusable_reason`,
`status`, the route's handler; the path is `control_oauth::INSPECTOR_CLIENT_PATH`, which
`is_public_path` and `is_discovery_path` name, so the route needs no credential and no API key in
any mode). `GET /_rustybin/mcp-servers` and `list_mcp_servers` answer `client_document` (`url`,
`redirect_uri`, `usable`, `reason`, `document`, `ways`) for the address the caller used. The
walk's choice is one pure function, `chooseClient` of `ui/lib/mcpauth.js`; the form's **Client**
select is `#mcp-oauth-way`. The check is `client_document`, the last row of `CHECKS`.

**Part B, where it lives.** `src/control_oauth/client_docs.rs` (the setting, `check_url`, `parse`,
the fetch, the cache in `OAuthState.documents`), `Purpose::ClientMetadata` (`client_metadata`), the
consent API's `client_document: {url, host}` and `ui/views/authorize.js`.

Where the build differs from the plan, and why:

- **The management tools take no "same choice".** The OAuth walk is the console's (a browser, a
  popup, a person); the tools never walked it and a token is never a tool input. So no tool gained
  a parameter. An agent reads the document and whether it is usable from `list_mcp_servers`, asks
  the question with `run_mcp_checks` (the `client_document` check) and sees a saved connection's
  choice in `list_mcp_connections`. The CLI gained `rustybin mcp client-document` and
  `mcp save-connection --oauth-client`; `mcp check --checks client_document` runs the check.
- **`rustybin.yaml` holds no saved connection**, so no choice either: saved connections keep
  credentials and stay in `<state>/mcp-inspector.json` (the choice is `auth.oauth.client` there,
  checked by `AuthSpec::validate`). The document carries part B's setting, never from git.
- **The check does not sign in.** There is nobody to approve, so it starts one authorization
  request with the document as the client and follows nothing: a sign-in page or a redirect to one
  passes, an error fails. It is the one check that runs without a connection (a server behind OAuth
  answers 401), and it offers the document of the address the check was asked at
  (`RunSpec.base_url`).
- **"The walk uses ours, no registration call, the token is issued" is a browser test**, since the
  walk is the console's: `run_client_documents` of `conformance/ui/console_e2e.py`, against an
  authorization server written in the test. The instance there is on `http://127.0.0.1`, so the
  automatic walk says why not and registers (the plan's second test) and the document is used
  when forced (the first). On the Rust side the check's tests see the document offered with no
  registration call and the redirect URI equal to the document's, byte for byte.
- **A forced way is tried even where it is unlikely to work** (the server does not say it supports
  documents, or the instance is on loopback): that is what forcing is for. It stops only where it
  cannot be tried (client credentials, or the console open at another address than the
  document's callback).
- **The setting is `oauth.client_metadata_documents` in the `agents` group**, so the AI agents
  view's settings form shows it without new console code. It has no environment variable (as step
  12's `agents.secret_requests`). `oauth.` is a new prefix of three tables: it needs
  `settings.security` (`settings::SECURITY_PREFIXES`), `update_settings` asks the user to confirm
  it (`SENSITIVE_PREFIXES`), and a document from git never changes it
  (`configdoc::REPOSITORY_REFUSED_PREFIXES`, by prefix rather than the plan's
  `REPOSITORY_REFUSED`, so a later `oauth.*` setting is covered too).
- **`https` only, with one exception the owner should look at:** plain `http` is taken for a
  loopback host, which the outbound policy refuses anyway unless `egress.allow_loopback` is on.
  Without it nothing could test or show the feature (the outbound client trusts the public web
  roots only, so a test cannot serve `https`): the unit tests, the Hurl example and the browser
  check all serve a document on `127.0.0.1`. Every other `http` URL is refused before any call.
- **The cache**: a document for five minutes, a refusal for 30 seconds, 64 entries, the oldest
  dropped. A refusal of the outbound policy is not kept (nothing was called, and the operator may
  be about to allow the host). A sign-in request with a document counts against its address's
  allowance before the instance calls out.
- **The token endpoint** takes a document's URL as a client only while the setting is on: turning
  it off ends those clients' refreshes too.
- **One Rustybin cannot fetch another's inspector document.** The outbound policy never calls a
  `/_rustybin` or `/ui` path on any host, and the inspector's document is at
  `/_rustybin/oauth/inspector-client.json`. So an instance with part B on refuses the inspector
  of another Rustybin as a document client (the inspector registers there instead, as before).
  Left as it is: the rule is an invariant of the policy, and the case is one Rustybin inspecting
  another's management server. If it is wanted, the document needs a second address outside the
  control plane.
- **The mock authorization server of `/mcp/protected` is unchanged.** It keeps a store of known
  clients; taking a URL-shaped client id there is not trivial, and it never fetches.

Not built: a diagnostics check for "on, and nothing in `egress.allow`" (the consent never starts
and the error page says why); showing a document's `client_uri` or logo on the consent page;
fetching a cached document again on demand.

Tests seen failing first: `every_control_route_has_a_management_tool`,
`the_command_tree_is_consistent_with_the_catalogue` and `readme_endpoints_block_is_up_to_date`
failed on the new route until its rows existed. The other tests were written with the code; each
protection was then removed once and its test seen to fail: the switch ignored
(`off_by_default_and_nothing_is_fetched`), the `client_id` comparison and the size cap
(`a_redirect_a_large_body_and_a_foreign_client_id_are_refused`; the size case first passed under
the mutation because it used the constant, and now uses a literal), the token endpoint's check of
the switch and the git prefix (`off_by_default...`, `on_a_document...`), the loopback reason
(`a_loopback_or_plain_http_instance_says_why...`, `the_check_is_skipped_with_its_reason`), the
check without a connection (`the_check_offers_the_document_and_never_registers`), and `oauth.`
out of the security and confirmation prefixes (`update_settings_tool_needs_settings_security`,
`settings_and_egress_tools`). Not seen failing by removal: the redirect case (the outbound client
never follows one, and the status check after it refuses a `302` either way) and the refusal
before any call (it is the outbound policy's own).

## For the merge

**`docs/src/changelog.md`** (under the next version):

```markdown
New:

- **MCP inspector: client ID metadata documents.** The instance serves the inspector's client
  document at `GET /_rustybin/oauth/inspector-client.json` (open; `rustybin mcp client-document`),
  and the OAuth walk sends its URL as the client id, with no registration call, to an
  authorization server that says `client_id_metadata_document_supported`. Step 4 of the walk says
  which client was used and why the others were not (an instance on `http://localhost` cannot
  offer a document and registers). **Client** in the auth form forces a document, registration or
  a given id; a saved connection keeps the choice (`auth.oauth.client`, `mcp save-connection
  --oauth-client`). `GET /_rustybin/mcp-servers` and `list_mcp_servers` answer `client_document`
  (the URL, whether a server can use it, why not).
- **MCP inspector: the `client_document` check.** Does the server's authorization server say it
  supports documents, and does it take this instance's? It runs without a connection and signs
  nobody in. Every check run gains its row (skipped with the reason where it does not apply).
- **OAuth sign-in to `/_rustybin/mcp`: client ID metadata documents, off by default.** With the
  new `oauth.client_metadata_documents` setting on, an agent's client may name itself with the
  `https` URL of a document instead of registering. The instance fetches it under the new outbound
  purpose `client_metadata`, which has no default (only hosts in `egress.allow`): no redirects, at
  most 5 KiB, kept five minutes. The consent page shows the document's host and says the name is
  the client's own claim. The metadata says `client_id_metadata_document_supported: false` while
  the setting is off. A document from git never changes the setting.

Changed:

- Settings under `oauth.` need `settings.security`, and `update_settings` asks the user to confirm
  a change of one.
```

**`docs/src/roadmap.md`**: remove "MCP inspector: client ID metadata documents" from under
consideration and from the list of what is left. Candidates the build named: a second address for
the inspector's document outside the control plane, so one Rustybin can take another's; a
diagnostics check for the setting on with an empty allowlist.

**`docs/plans/core-next/master-plan.md`**: row 15 done 2026-11-12 (parts A and B); the row of
section 6 for 15 B answered yes, as recommended.

**`CLAUDE.md`**, a paragraph for step 15:

> Step 15 (client ID metadata documents; `docs/plans/core-next/step-15-client-id-metadata-documents.md`,
> user docs `docs/src/mcp/inspector.md#the-inspector-as-a-client-with-a-metadata-document` and
> `docs/src/agent-mcp.md#sign-in-with-oauth`): the inspector's document
> (`src/mcp/inspector/clientdoc.rs`, `GET /_rustybin/oauth/inspector-client.json`, open; the walk's
> `chooseClient` in `ui/lib/mcpauth.js`; the `client_document` check) and the control plane
> accepting one (`src/control_oauth/client_docs.rs`, the setting `oauth.client_metadata_documents`,
> off by default, `Purpose::ClientMetadata` with no default). Rules it left: the redirect URI the
> walk sends is the document's, byte for byte, and the walk uses a document only when
> `clientdoc::unusable_reason` is `None` (https, not loopback) unless a person forced it; a saved
> connection keeps the way (`auth.oauth.client`), never a token it was not given; a check that
> needs no connection is skipped by name in the "not connected" loop of `checks::run` and run
> after it; the control plane fetches a client's document only with the setting on, through
> `client_docs::client` (the URL rules first, the outbound policy before any call, no redirect, 5
> KiB, the `client_id` equal to the URL, a bounded cache), and counts the sign-in request before
> it calls out; what a document says is the client's claim (the consent API's `client_document`,
> shown as text with its host); a setting under `oauth.` is a security setting
> (`settings::SECURITY_PREFIXES`, `SENSITIVE_PREFIXES`, `configdoc::REPOSITORY_REFUSED_PREFIXES`);
> the mock authorization server of `/mcp/protected` never fetches a client's URL.

**Upgrade notes**: none needed to keep an instance working (part B is off by default). A saved
check run and a JUnit report gain one row, `client_document`.

**Shared tables this step touched** (for conflicts at the merge): `checks::CHECKS` (a row at the
end), `Purpose` and `Purpose::ALL` (15 to 16), `settings::specs()` (one `extend`),
`settings::SECURITY_PREFIXES`, `mcp::manage::settings::SENSITIVE_PREFIXES`,
`configdoc::REPOSITORY_REFUSED_PREFIXES` (3 to 4), `COVERAGE` in `src/mcp/manage/tests.rs` (one
row after `/_rustybin/mcp-servers`), `COMMANDS` (the row `mcp client-document` after `mcp
end-relay`, a flag on `mcp save-connection`), the description of `list_mcp_servers` in `TOOLS` (no
new tool), the regenerated `README.md` and `docs/src/cli.md`, the purposes table and the settings
list of `docs/src/configuration.md`, `conformance/ui/console_e2e.py` (`run_client_documents`,
called after `run_jwt`).
