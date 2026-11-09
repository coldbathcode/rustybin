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
