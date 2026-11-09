# Configure Rustybin from an AI agent

Every Rustybin instance runs a management [MCP](https://modelcontextprotocol.io) server
at `/_rustybin/mcp`, so an agent such as Claude Code can configure it for you: create
and deploy OpenAPI mocks, change their responses, rules and security, send test
requests, mint tokens, and read what your gateway sent. Ask in plain words ("mock
`openapi/orders.yaml`, secure it with JWT and tell me which token the gateway needs")
and the agent picks the tools.

This is not the [mock MCP server](mcp/index.md) at `/mcp`, which serves fake tools for
MCP gateway demos. Both speak the same protocol versions and transports; the
management server sits on the control plane, so [control-plane
authentication](concepts/control-plane-security.md) protects it, and its tools call the
same code as the REST routes under `/_rustybin/*`: permissions, redaction, size caps
and error messages are identical.

## Connect an agent

The server is at `https://<host>/_rustybin/mcp` (streamable HTTP), with the
credential as `Authorization: Bearer <token>`. In Claude Code:

```bash
claude mcp add --transport http rustybin https://<host>/_rustybin/mcp \
  --header "Authorization: Bearer <token>"
```

[Connect your coding agent](coding-agents.md) has the configuration for Claude
Code, Codex CLI, Cursor, OpenCode, VS Code, Windsurf, Gemini CLI and Zed (with the
token in an environment variable), how to get a credential, the rights to give, each
client's tool limits and a walkthrough of building a test platform.

The [web console](console.md) fills the command in for you: "Connect an agent" in the AI
agents view and "Connect an AI agent" in the Mock APIs view show it, and the
`mcpServers` JSON, with this instance's
URL and the credential the console already holds (the token of your password
sign-in, a control token, the admin token or a visit's manage token). The token stays
masked until you ask to see it; the copy buttons copy the real value. A script gets
the same token from `POST /_rustybin/login` (see [User name and
password](concepts/control-plane-security.md#user-name-and-password)).

Clients that only speak stdio use the bridge built into the binary. It reads one
JSON-RPC message per line on stdin, posts it to the server and writes the answers to
stdout:

```bash
rustybin mcp-stdio --server https://<host>/_rustybin/mcp --token <token>
# a visitor on a self-service demo
rustybin mcp-stdio --server https://<host> --header 'X-Rustybin-Mock-Token: rbm_...'
# a profile of `rustybin login`: its URL and token
rustybin mcp-stdio --profile mocks.example.com
# the instance's URL: the token of the profile kept for it
rustybin mcp-stdio --url https://mocks.example.com
```

`--server` falls back to `RUSTYBIN_MCP_URL` (a bare base URL gets `/_rustybin/mcp`
appended) and `--token` to `RUSTYBIN_TOKEN` or `RUSTYBIN_ADMIN_TOKEN`. `--profile` and
`--url` read the [command line's](cli-guide.md) `profiles.json`, so a local agent manages a
remote instance with the sign-in `rustybin login` already did; without any of them the
default profile is used.

The bridge handles one request at a time and does not relay requests the server
sends while a call runs. Clients on protocol 2026-07-28 get confirmation questions
inside the call's own response, so they work. An older client that declares
elicitation never sees the question through the bridge, and destructive tools fail
after the server's wait: there, leave elicitation out and let the model pass
`"confirm": true` after the user agreed (see [Confirmations](#confirmations)).

On an instance whose control plane asks for credentials you can also leave the
header out and [sign in through the browser](#sign-in-with-oauth).

## Sign in with OAuth

Where the control plane asks for credentials (`RUSTYBIN_CONTROL_AUTH=password`,
the default, `token` or `jwt`), an agent can sign in instead of you pasting a token:

```bash
claude mcp add --transport http rustybin https://<host>/_rustybin/mcp
```

The first call answers `401` with an [RFC 9728](https://www.rfc-editor.org/rfc/rfc9728)
`resource_metadata` pointer. The client reads the metadata, finds Rustybin's own
authorization server at `/_rustybin/oauth`, registers itself and opens your browser
on the console's consent page (`/ui/#/authorize/<id>`). Sign in to the console there
if it asks (with the admin token or a control JWT, as usual), check the agent's name
and where the browser will be sent back to (a warning shows when that is not your own
machine), choose what to grant and approve. The agent gets a token and carries on.

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_challenge}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_metadata}}
```

**What you can grant.** The consent page offers full access (`admin`), read only
(`console`) and traffic only (`inspector`), the same scopes as [control
JWTs](concepts/control-plane-security.md), and lists the permissions the agent will
hold. When a user approves (the console signed in with a user name and password), the
agent [acts for that user](#agents-act-for-a-user): it holds the user's permissions
within the scope, resolved again at every call, never more than the user; a scope is
offered when it gives at least one permission the user holds. Approved with a
credential that is no user (the admin token, a control JWT), the scopes are capped by
that credential as before: a console signed in with a `console` scope token can let an
agent read, not change anything, and an `admin` grant is admin on the management
server, like the admin token there.

**What the token opens.** Only `/_rustybin/mcp` on this instance and host: the token
is bound to that URL (RFC 8707), and every other control-plane route, the data plane's
admin checks and other instances refuse it. Access tokens last an hour; the agent
renews them with a refresh token (30 days) without asking you again, and a refresh can
narrow the scopes, never widen them. A user's tokens carry the user and the user's
sign-in generation, so the user's sign out everywhere, password change, disable or
removal ends them, refresh tokens included.

**The key.** Client registrations and tokens are signed with a key, so no client or
token is stored and nothing grows on the server. The key comes from
`RUSTYBIN_CONTROL_OAUTH_KEY` (32 characters or more), else it is a random key kept in
`<state>/control-oauth.key` with a state directory, else it is random per process; it is
never derived from the admin token. Rotating it signs every agent out. With a key per
process a restart signs agents out too, and several replicas behind one URL need
`RUSTYBIN_CONTROL_OAUTH_KEY` set to the same value (or one state directory). Single
tokens cannot be revoked. Sign-in requests are limited per client address (an IPv6
/64) and for every address together, and one client keeps at most three pending
consents (a new one replaces its oldest).

**Clients.** Registration is open ([RFC 7591](https://www.rfc-editor.org/rfc/rfc7591)),
for public clients with PKCE (S256) only. Redirect URIs are `https`, `http` on a
loopback address (any port when the agent signs in, as native apps pick a free one)
or an app's own scheme (`cursor://...`). A failed request goes back to the agent
only when it waits on a loopback address; for any other redirect URI the browser
shows the error, so the server never redirects anywhere on someone else's behalf.

These are the requests an agent makes, step by step. The console's part is the
consent API, called with its credential:

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_register}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_authorize}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_consent}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_token}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_use}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_refresh}}
```

```hurl
{{#include ../examples/agent/manage_oauth.hurl:oauth_errors}}
```

**An external issuer.** With `RUSTYBIN_CONTROL_OAUTH_ISSUER=https://...` the metadata
names that authorization server instead and the built-in one answers `404`. The tokens
it issues must be the [control JWTs](concepts/control-plane-security.md) the instance
already accepts (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`, `aud` equal to
`RUSTYBIN_CONTROL_JWT_AUDIENCE`): the way for a hosting platform to sign agents in
with its own accounts.

**When it is off.** On an open control plane (nothing asks for credentials, so no
`401` starts the flow), in shared mode (API keys authenticate there), and with
`RUSTYBIN_CONTROL_OAUTH=off`. The metadata documents need no API key even with
`RUSTYBIN_REQUIRE_KEY`, since an agent reads them before it holds anything.
Self-service visitors keep sending their visit's manage token as a header.

## Credentials and what they allow

The agent gets the permissions its credential has, no more: every tool declares the
[permission](concepts/control-plane-security.md#permissions) it needs (the tables
below). `tools/list` shows only the tools the caller may use; a tool called anyway
answers a tool error saying which permission is missing (`permission_required:
mocks.write`), never a protocol error.

| Instance | Credential | Tools |
|---|---|---|
| Open control plane, no admin token | none | All |
| Open control plane with `RUSTYBIN_ADMIN_TOKEN` | none | Reads, tests and traffic; no changes |
| | the admin token (`Authorization: Bearer`, or `X-Rustybin-Admin-Token`) | All |
| `password` control auth (the default) | the token of a sign-in with the console's user name and password (`POST /_rustybin/login`) | All |
| `password` / `token` / `jwt` control auth | no token | `401`, like the rest of the control plane |
| | a sign-in of another user, or a control JWT with `rb_roles` / `rb_perms` | What its roles' permissions allow |
| | control JWT, `console` scope | Reads (settings, saved tests, source control and agents included), tests and traffic |
| | control JWT, `inspector` scope | Traffic only |
| | control JWT, `harness` scope | The harness tools |
| | the admin token, or a control JWT with the `admin` scope | All |
| Public mode | none | Reads, tests and traffic (the inspector needs a session); changes are forbidden |
| Self-service demo | none | Reads and tests, plus `create_mock` (the first one starts a visit) |
| | the visit's manage token (`X-Rustybin-Mock-Token`) | The above, changes to the visit's own mocks, `get_visit`, `extend_visit` |
| Shared mode | an API key (`X-Rustybin-Key`) | The key's own mocks, inspector entries and usage |

`get_instance` reports what the caller is (`caller.admin`, `caller.user`, the user it
acts for, `caller.via`, `caller.scopes`, `caller.roles`, `caller.permissions`), so an
agent can explain why a tool is missing. On a secured instance:

```hurl
{{#include ../examples/agent/manage_secured.hurl:secured_no_token}}
```

```hurl
{{#include ../examples/agent/manage_secured.hurl:secured_console_scope}}
```

```hurl
{{#include ../examples/agent/manage_secured.hurl:secured_admin}}
```

## Managing agents

Every client that calls the server is an **agent**: its credential plus the name it
gives in `clientInfo` (sent with `initialize`, or in `_meta` of a 2026-07-28 call).
Two clients on the same token are two agents; a client that sends no name is
`unknown`. **The identity rule:** the name only tells agents apart in the list. A
revocation, and the rights you set on an agent, follow its credential: every agent of
the same credential (the same admin token, API key, visit, user's sign-in, JWT subject
or OAuth client) is refused while one is revoked, and must also pass the limits set on
the others, so a new client name never starts over. Rules of `rustybin.yaml` still
match by name. On an open control plane an agent is known only by its session label,
which it can change: turn the sign-in on to control agents for real. The console's **AI agents** view (under AI and agents) lists them with
the user each acts for (the **Owner** column, with a filter), their credential, first
and last seen, calls today and in total, open sessions and
status (`active`, `idle`, `revoked`), and opens one to show its calls per tool with
their outcomes (ok, error, denied, asked to confirm), its last errors and its
sessions. The live feed shows calls as they happen. The same data is at
`GET /_rustybin/agents` (`agents.read` for everyone's; a user's own without it):

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_call}}
```

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_list}}
```

**Rights.** An agent never gets more than its credential allows (a user's agent,
more than its user); the operator can give it less. A **ceiling** caps its tools: a
role, built-in or custom (`viewer`, `editor`, a role of your own: the tools whose
permission the role grants), or one of the four 0.3 ceilings, which keep exactly the
tools they had: `read-only` (read tools, no captured traffic; nearest to `viewer`),
`traffic` (every read tool; `viewer` plus `traffic.read`), `manage` (reads and mock
changes; nothing instance-wide, no visit operator tools, no harness; nearest to
`editor`) or `full` (whatever the credential allows: a user's own). A 0.3 name wins
over a custom role of the same name, and a ceiling naming a role that does not exist
(any more) allows nothing. An **allow** list narrows it to the tools named; a **deny** list
removes tools. A tool outside its rights is not in its `tools/list`, and calling it
anyway answers a tool error that says the operator limited the agent. Rights come
from, field by field: the agent's own (set in the console or with `PATCH
/_rustybin/agents/{id}`), else the first matching rule of
[`rustybin.yaml`](mocks/config.md)'s `agents:` section, else (for the ceiling) the
default for a role of its user (the `agents.default_ceiling_for` setting, entries
`role=ceiling` such as `admin=editor`; the first entry whose role the user holds),
else the default ceiling (the `agents.default_ceiling` setting,
`RUSTYBIN_AGENTS_DEFAULT_CEILING`, `full` by default).

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_rights}}
```

```yaml
agents:
  default_ceiling: traffic
  rules:
    - match: { name: "claude-*", credential: oauth }
      ceiling: manage
      deny: [delete_mock, apply_config]
      note: Team assistants sign in with OAuth
    - match: { credential: admin }
      ceiling: full
    - match: { role: admin }
      ceiling: editor
      note: An admin's agents build, they do not administer
    - match: { user: "ci-*" }
      ceiling: viewer
```

`match.name` is the client's name, case insensitive, with `*` as a wildcard;
`match.credential` is `admin`, `user` (any user's agent), `console` (a user's agent on
a sign-in token), `jwt`, `oauth` (a token of the built-in OAuth server, a user's or
not), `key`, `visit` or `open`; `match.user` is the user the agent acts for (a sign-in
name, with `*`, or a user id) and `match.role` a role that user holds (neither matches
an agent that acts for nobody). A saved or environment default ceiling wins over the
file's `default_ceiling`. Whatever a document says, a document pulled from git
included, an agent acting for a user never holds more than the user: a ceiling only
takes away.

**Revoke** refuses every call of the agent (a tool error saying it was revoked), ends
its sessions (their next request answers `403`), and revokes its tokens for their
remaining life: the console sign-in tokens and OAuth access tokens it used, and an
OAuth client's refresh tokens. An OAuth client is shared by every user who registers
the same metadata (registration is open and the client id follows from it), so the
revocation of a user's OAuth agent refuses that user's tokens of the client only,
never another user's. Revoking an agent that is revoked already changes nothing: who
revoked it, and when, stay (the answer says `already_revoked`). **Restore** undoes all
of it. The admin token cannot be
revoked per agent (rotate `RUSTYBIN_ADMIN_TOKEN`; meanwhile every agent on the admin
token is refused, whatever name it sends); API keys are revoked in the key registry. If you gave an agent your own
console token, revoking it signs your console out too. **End sessions** closes the
agent's handshake-era sessions without revoking it.

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_revoke}}
```

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_restore}}
```

```hurl
{{#include ../examples/agent/manage_agents.hurl:agents_end_sessions}}
```

The live feed is Server-Sent Events, one `call` event per tool call (the agent, the
tool, the outcome and the latency) and an `agent` event on revoke and restore:

```bash
{{#include ../examples/concepts/live_feeds.sh:agents_feed}}
```

| Route | |
|---|---|
| `GET /_rustybin/agents` | The agents (each with `owner`, the user it acts for, and `owner_label`) and the policy (default ceiling, the defaults per role, rules); `?owner=<user>` keeps one user's |
| `GET /_rustybin/agents/events` | Server-Sent Events: a `call` per tool call, an `agent` event on revoke and restore (at most 16 listeners) |
| `GET /_rustybin/agents/{id}` | One agent: its user, calls per tool, last errors, sessions, the tools and whether its rights allow each |
| `PATCH /_rustybin/agents/{id}` | `{"rights": {"ceiling", "allow", "deny"} \| null, "note": "..." \| null}`; a ceiling is a role or a 0.3 ceiling |
| `POST /_rustybin/agents/{id}/revoke`, `/restore` | Revoke or restore |
| `DELETE /_rustybin/agents/{id}/sessions` | End its sessions |

Everyone's agents need `agents.read` to see and `agents.manage` to change; a user
without them sees, limits, revokes and ends the sessions of their own (see below).
The registry keeps at most 512 agents (the least recently seen
goes first, agents with rights, a note or a revocation last), 20 errors, 32 sessions
and 16 token ids per agent. With `RUSTYBIN_STATE_DIR`, rights, notes, revocations and
the call totals are kept in `<state>/agents.json`; sessions and errors are not. On an
open control plane anyone may connect under any name, so rights there are advisory:
turn on the console sign-in to control agents for real. **Explain what this agent
did** (with an AI key) summarises an agent's calls, errors and rights.

### Agents act for a user

With the password sign-in, an agent that signs in as a user works on that user's
behalf and never does more than they may: an agent on a user's sign-in token (pasted
as a bearer, or the token of `rustybin login`), and an agent whose OAuth sign-in a
user approved on the consent page. Its permissions are the user's, resolved at every
call (a role change of the user applies to the agent's next call), within the scope
it was given, then its ceiling, its allow and deny lists, the rules and the defaults
take away from that. The admin token and control JWTs stay credentials of their own:
their agents act for nobody, and the list says so (`owner` is `null`, `owner_label`
names the credential).

```hurl
{{#include ../examples/agent/agents_for_users.hurl:user_agent}}
```

```hurl
{{#include ../examples/agent/agents_for_users.hurl:owner}}
```

**Your own agents.** Seeing everyone's agents needs `agents.read` (the viewer role
and up hold it), changing them `agents.manage`. Without them a user sees their own
agents, limits them (rights that only take tools away: a change that would let a tool
through again is refused), revokes them, ends their sessions, and restores one they
revoked themselves; one the operator revoked (or that was revoked when their user was
disabled) is the operator's to restore, and revoking it again first does not make it
theirs.

```hurl
{{#include ../examples/agent/agents_for_users.hurl:role_ceiling}}
```

```hurl
{{#include ../examples/agent/agents_for_users.hurl:own_limits}}
```

**The consent page** lists what the agent will hold: the approving user's permissions
within the scope chosen, never more, and under `projects` (per scope in `grants`, and
for the whole request) what it holds in each of the user's projects, there only. A
user who holds something in a project only may grant a scope that gives them
something there.

```hurl
{{#include ../examples/agent/agents_for_users.hurl:consent}}
```

**When a user goes.** Disabling or deleting a user revokes every agent acting for
them (their calls are refused, their sessions end, their tokens and their OAuth
clients' refresh tokens are refused); enabling the user again leaves the agents
revoked until someone restores them. Signing a user out everywhere, and a change of
their password, ends their agents' tokens and sessions without revoking the agents.
The first admin's password change signs out the first admin only (their sessions,
portal sessions and agents), no longer every user; tokens the OAuth server issued
before 0.5 count as the first admin's, so they end too. Only an OAuth sign-in approved
with the admin token or a control JWT (an agent acting for nobody) outlives it: sign
everyone out (`DELETE /_rustybin/sessions`) to end those.

```hurl
{{#include ../examples/agent/agents_for_users.hurl:user_removed}}
```

Agents recorded before 0.5 (a console sign-in by user name, an OAuth client) are
moved to their user at the first start with the password sign-in: a console record to
the user of that name, an OAuth record to the first admin, with their rights, notes
and revocations.

## Talking to the server

`server/discover` (or `initialize` on the handshake versions) returns the server
`instructions`, which tell the model how to treat captured traffic and confirmations,
and the capabilities: tools, prompts and resources.

```hurl
{{#include ../examples/agent/manage.hurl:discover}}
```

`tools/list` answers 100 tools a page, with `nextCursor` while more follow: the
management server has more than that, so a client reads every page. Some clients
read only the first (see [Tool limits](coding-agents.md#tool-limits)): for them,
`?page_size=200` on the MCP URL (at most 1000) sends every tool on one page.

```hurl
{{#include ../examples/agent/manage.hurl:tools_list}}
```

Every result has a one-line text summary plus `structuredContent`, with links the user
can open: the mock's URL, its console page and a `curl` for a sample request.

```hurl
{{#include ../examples/agent/manage.hurl:get_instance}}
```

## Tools

Arguments mirror the REST bodies and [`rustybin.yaml`](mocks/config.md), so what you
learn in one carries over to the other. The **Permission** column names what each
tool needs; "or owner" means a visitor or an API key may use it on its own mocks of a
self-service instance (the instance's mocks need the permission), and "the visit" a
running visit's manage token. The visit operator tools are listed only with
self-service on.

### Instance

| Tool | Permission | |
|---|---|---|
| `diagnose_instance` | `settings.read` | The [diagnostics](troubleshooting.md#ask-the-instance-first): each check with an id, a status and the fix; the summary names checks only, quoted file and remote text is under `untrusted_message` |
| `get_instance` | `instance.read` | Version, modes, control auth, limits, the public configuration, the caller's rights, console and docs URLs. Where an agent starts |
| `find_endpoints` | `instance.read` | Search the [route catalogue](concepts/catalogue.md) by text or category ("anthropic streaming"): path, methods, summary, an example request |
| `get_health` / `set_health` | `instance.read` / `settings.write` | The `/health` demo toggle |
| `get_usage` | `instance.read` | The plan, its limits and what is left, for the caller (their key in shared mode), so an agent can explain a `429` |
| `get_key_usage` | `settings.read` | Per-key daily totals in shared mode, paginated |
| `reset_flaky` | `instance.read` | Reset the `/flaky` counters of the caller's session between test runs; `scope: "all"` needs admin |

### OpenAPI mocks

| Tool | Permission | |
|---|---|---|
| `list_mocks` | `mocks.read` | Name, URL, state, security, calls, owner; filter by text or state |
| `get_mock` | `mocks.read` | Operations with hit counts, settings (secrets redacted unless the caller may change the mock), deploy report, revisions, undeployed draft changes |
| `preview_spec` | `mocks.read` | Parse a spec without creating anything: title, operations, security schemes, warnings, deploy checks |
| `get_mock_spec` | `mocks.read` | The spec as uploaded, to edit and send back with `update_mock`; or with `served: true` the OpenAPI document the mock serves |
| `get_mock_revision` | `mocks.read` | A [kept revision](mocks/index.md#comparing-a-refused-edit) (`name`, `revision`): its spec and settings and what the draft changed since; read a `409`'s `expected_revision` to see what the other change did |
| `create_mock` | `mocks.write` or owner | From spec text or an object (JSON or YAML; Swagger 2.0 is converted) with optional name, responses, security and operations; deployed unless `deploy: false` |
| `update_mock` | `mocks.write` or owner | Replace the draft's spec or settings, or rename it; `deploy: true` publishes the revision it wrote; `expected_revision` refuses a draft changed meanwhile (see [edit conflicts](mocks/index.md#two-people-editing-one-mock)) |
| `set_operation` | `mocks.write` or owner | One operation's response override, [rules](mocks/responses.md), mode and public flag; takes `expected_revision` |
| `test_rules` | `mocks.read` | Which rule would answer a captured call, every condition explained; try new rules without saving |
| `check_mock` | `mocks.read` | The deploy report: a valid request and response per operation |
| `deploy_mock` | `mocks.write` or owner | Publish the draft; `force` only after the user accepted the check errors; `expected_revision` publishes only that draft revision (an error with what changed when the draft moved on) |
| `rollback_mock` | `mocks.write` or owner | Make an earlier revision live again (confirmed) |
| `set_mock_enabled` | `mocks.write` or owner | On or off (off answers `503`) |
| `duplicate_mock` | `mocks.write` or owner | Copy under a new name |
| `delete_mock` | `mocks.delete` or owner | The mock, its revisions, stored data and traffic (confirmed) |
| `sample_request` | `mocks.read` | A valid request for an operation, as `curl` and as parts, plus the response the mock would send |
| `validate_payload` | `mocks.read` | Check a request or response body against an operation's schema, for example after a gateway transformation |
| `export_collection` | `mocks.read` | The mock as a Postman collection, Hurl file, curl script, `.http` file, k6 script, Bruno collection or Insomnia export, optionally against a gateway's base URL (see [collection exports](exports.md)) |
| `get_mock_data` / `clear_mock_data` | `mocks.read` / `mocks.data` or owner | What a `stateful` mock stored; clearing is confirmed |
| `get_mock_traffic` / `clear_traffic` | `traffic.read` / `mocks.data` or owner | The calls a mock received: operation, status, validation and auth result, how the response was chosen; clearing is confirmed |
| `reload_mocks` | `config.apply` | Reload the file-defined mocks (`RUSTYBIN_CONFIG_FILE`, `RUSTYBIN_MOCKS_DIR`), as SIGHUP does |

`get_mock` and `check_mock` carry the example `coverage` (which operations lack an
example), the input of `generate_examples`.

### AI assist and AI-built mocks

Every model call is an [assist task](ai-assist.md) with the user's own key. Keys are
never tool inputs and never returned: the operator sets them in the console or with
`rustybin settings set-secret`; `assist_status` shows `{set, hint}`.

| Tool | Permission | |
|---|---|---|
| `assist_status` | `assist.use` | Whether assist is on, the provider and models, the caps, each provider's key state, the tasks, the usage total |
| `list_assist_models` | `assist.use` | The configured provider's models (as last loaded, merged with the curated table) and the model of every job; choose with `update_settings` on `assist.model`, `assist.fast_model` or `assist.job.<task>` |
| `load_assist_models` | `settings.write` | Ask the provider for its model list with the stored key (confirmed; one outbound call, no tokens) |
| `assist_usage` | `settings.read` | Calls, tokens and the estimated cost since start, per model and per task |
| `test_assist` | `settings.write` | One small call to the configured provider |
| `run_assist_task` | `assist.use` | A read task with its inputs (`free_answer`, `explain`, `suggest`, `request_examples`, `tool_arguments`, `server_json`, `summarise_for_portal`); the mock tasks point at their own tools |
| `generate_spec` | `assist.jobs` or owner | An OpenAPI document from a description, checked like an upload; `create: true` (confirmed) creates the mock from it as `create_mock` would |
| `generate_examples` | `assist.jobs` or owner | Examples for the operations that lack one (or the ones named), from the provider or the deterministic generator; `apply: true` (confirmed) stores them in the draft |
| `edit_spec_with_ai` | `assist.jobs` or owner | The draft spec edited through an instruction: the document, the diff and its checks; `apply: true` (confirmed) saves it as the draft |

### Settings and the outbound policy

| Tool | Permission | |
|---|---|---|
| `get_settings` | `settings.read` | Every [instance setting](configuration.md#state-directory-settings-and-secrets) with its value, source and lock state, or one group or key; secrets show `{set, hint}` |
| `update_settings` | `settings.write` | Set values (`null` forgets a stored one) and unset stored values; secrets are refused, and so is the console sign-in in password mode (it changes only with its current password); each key needs what `PATCH /_rustybin/settings` needs for it (`settings.security` for the security groups, `portal.settings`, `git.write` for `git.*`, the permissions of the roles a `sso.role_map` or `sso.default_roles` names); a change of `egress.*`, `agents.*`, `console.*`, `git.*`, `sso.*`, `proxy.*`, `assist.provider` or `assist.base_url` is then confirmed by the user |
| `check_egress` | `settings.read` | The [outbound policy](configuration.md#outbound-connections) and, with `url` and `purpose`, whether that URL would be allowed before DNS and why not |

### MCP inspector and registry

| Tool | Permission | |
|---|---|---|
| `list_mcp_servers` | `mcp.inspect` | Every MCP server this instance serves with its auth, the relay's allowlist, the check ids |
| `mcp_call` | `mcp.inspect` | One JSON-RPC request to any MCP server (a path on this instance in process, or a URL through the relay) with the handshake the transport needs; credentials as headers or a saved connection. Another host is confirmed, and without admin rights gets read-only methods only (no `tools/call`). The answer is `untrusted_result` |
| `run_mcp_checks` | `mcp.inspect` | The [inspector's check suite](mcp/inspector.md) against a server; the report is saved unless `save: false`. Another host is confirmed, and without admin rights the calls to tools are skipped. At most two suites run at once |
| `list_mcp_check_runs` | `mcp.inspect` | The saved runs, or one report (under `untrusted_report`) |
| `list_mcp_connections` | `mcp.inspect` | The saved connections (credentials stay sealed on the instance) |
| `list_registry_servers` | `instance.read` | The [mock registry](mcp/registry.md)'s counts, scenario and a gateway's sync configuration, and a page of its entries (`cursor`, `limit`) under `untrusted_entries`, each validated and cut at 4 KiB (anyone may publish) |
| `set_registry_scenario` | `registry.write` | `normal`, or a scenario that breaks the registry on purpose |
| `browse_registry` | `mcp.inspect` | A page of a real registry (the official one by default) through the outbound policy; confirmed for another host; the entries are `untrusted_servers` |
| `validate_server_json` | `instance.read` | A `server.json` against the registry schema and the semantic checks |
| `unpublish_registry_server` | `registry.write` | Remove an entry published through the registry's API (confirmed) |

### Dev portal

| Tool | Permission | |
|---|---|---|
| `list_portal_items` | `instance.read` | The [portal](portal.md)'s state and branding, the readers of closed items, every item that can be published with its metadata (its `access`, open or closed, and whether a mock is `secured`), whether it is hidden and why, its insights |
| `publish_portal_item` | `portal.publish` or owner | Publish an item (`mock:<name>`, `mcp:<server>`, `llm`, `a2a:<agent>`, `tools`, `registry`) with a title, summary, tags, contact, order, the credentials switch and `access` (`open` or `closed`; `secure: true` turns a mock's mocked security on to close it); confirmed when the portal is on and open to anyone |
| `unpublish_portal_item` | `portal.publish` or owner | Remove an item |
| `set_portal_branding` | `portal.settings` | The `portal.*` settings: `enabled`, `title`, `intro`, `contact`, `auth`, `base_url`, `visitors` |
| `get_portal_insights` | `instance.read` | Views, downloads and Try it calls per item |

### Configuration as code and git

| Tool | Permission | |
|---|---|---|
| `export_config` | `mocks.read` | [`rustybin.yaml`](configuration-as-code.md) version 2 for an admin (the whole instance, secrets as references), the mocks the caller may see (version 1) for others, or one mock with `name`; `format` yaml or json |
| `diff_config` | `config.apply` | What applying a document would change, section by section (a dry run) |
| `apply_config` | `config.apply` | Apply a version 1 or 2 document; `dry_run` reports only; `prune` (confirmed) makes a present section the whole truth |
| `git_status` | `git.read` | The [repository](source-control.md), the working copy, the drift, ahead and behind (`fetch: true`), `untrusted_divergence` when the branch has diverged (the commits of each side, what each way out would change), whether commits are signed, the timeline and the last commits |
| `git_init` | `git.write` | Clone the remote (`git.remote`, `git.branch` settings) or start an empty working copy |
| `git_commit` | `git.write` | Export the instance into the working copy and commit with a message; `checkpoints` names local [checkpoints](checkpoints.md#in-the-configuration-repository) to commit next to the document (never by the automatic mode) |
| `git_push` | `git.write` | Push the branch; confirmed, since the configuration leaves the host |
| `git_pull` | `git.write` | Fetch and fast-forward; `apply: true` applies the repository's document (confirmed unless `dry_run`) |
| `git_resolve` | `git.write` | The ways out of a [diverged branch](source-control.md#a-diverged-branch) (`git_status` shows `untrusted_divergence`: what differs): `with: instance` (this instance's copy, committed on top of the remote, never a forced push), `repository` (the repository's document applied here) or `pull_request` (this instance's side to a new `branch` for a pull request at the git host, nothing applied). Confirmed; `dry_run: true` previews without asking |
| `git_deploy_key` | `git.write` | The deploy key's public half; `generate: true` makes one, `replace: true` (confirmed) replaces it |
| `git_branches` | `git.read` | The remote's [branches](source-control.md#branches) (`untrusted_branches`: the names are the repository's) and the one the instance is on |
| `git_switch_branch` | `git.write` | A branch per environment: move the instance to another branch of the remote (`dry_run: true` answers what applying its document would change), or start one from the instance with `create: true` (committed and pushed). Confirmed unless `dry_run` |
| `git_signing_key` | `git.write` | [Signed commits](source-control.md#signed-commits): the signing key's public half and fingerprint to register at the git host; `generate: true` makes one, `replace: true` (confirmed) replaces it. The private half is never an input or an output |

The token for an https remote is the secret `git.token`: an agent cannot set it.

### Agents, users and sign-in sessions

| Tool | Permission | |
|---|---|---|
| `list_agents` / `get_agent` | `agents.read` or your own | The [agents](#managing-agents) with the user each acts for (`owner` keeps one user's) and the policy; one agent with its calls per tool, errors, sessions, rights and recent activity. Without `agents.read`, [your own](#agents-act-for-a-user) |
| `set_agent_rights` | `agents.manage` or your own | A ceiling (a role, or `read-only`, `traffic`, `manage`, `full`), allow and deny lists, a note; `reset: true` clears the agent's own rights. Never on the caller's own agent or an agent of the caller's credential; without `agents.manage`, your own agents and only to limit them |
| `revoke_agent` / `restore_agent` | `agents.manage` or your own | Revoke (confirmed; an agent cannot revoke itself) and restore (never an agent of the caller's credential; without `agents.manage`, one of yours you revoked) |
| `end_agent_sessions` | `agents.manage` or your own | Close an agent's handshake-era sessions (never of an agent of the caller's credential) |
| `list_sessions` | `users.read` | The console's sign-in sessions, when the password sign-in is on; `user` lists one user's |
| `end_session` | `users.manage` | End one session by id; `user` signs one user out everywhere, `all: true` everyone (both confirmed) |
| `list_users` | `users.read` | The [users](users.md): roles, status (active, invited, disabled), last sign-in, the open invitation (never its link); `user` for one with its permissions |
| `invite_user` | `users.manage` | Create a user, invited, and answer the one-time link to send them (confirmed); `reset: true` sends a user who exists a new link (a password reset); `sso: true` adds a [single sign-on](sso.md) user instead (no link) |
| `update_user` | `users.manage` | Roles, display name, email, disabled, a single sign-on user's `pinned_roles`; confirmed when it grants a privileged role. The guard rails apply (the last enabled admin stays, nobody changes their own roles) |
| `delete_user` | `users.manage` | Delete a user (confirmed; never the last enabled admin, never the first admin) |
| `list_roles` | `users.read` | The built-in and custom roles with their permissions and holders, and the permission table; `name` for one |
| `save_role` | `users.manage` | Create or change a custom role (confirmed when it is privileged) |
| `delete_role` | `users.manage` | Delete a custom role nobody holds |
| `test_sso` | `settings.write` | [Single sign-on](sso.md): its status, and the identity provider's discovery document and keys fetched now, the redirect URI to register, the role map's unknown roles. The `sso.*` settings change with `update_settings` |
| `list_projects` | `instance.read` | The [projects](projects.md) the caller may see: the caller's roles there, what each holds, the members (with `users.read`); `name` for one |
| `save_project` | `users.manage` | Create or change a project: description and members (`{user, roles}`, the whole list); confirmed when it gives a privileged role |
| `delete_project` | `users.manage` | Delete a project (confirmed); one that holds mocks, saved tests or checkpoints needs `move_to` (another project, or `instance`) |
| `list_audit` | `audit.read` | The [audit log](audit-log.md), newest first: who changed what, sign-ins and refusals, with the filters of `GET /_rustybin/audit` (`actor`, `action`, `target`, `since`, `until`, `result`) and `cursor`; names people typed under `untrusted_actor_name`, `untrusted_targets` and `untrusted_request_id` |

No tool takes or returns a password or a hash: a person joins through the link
`invite_user` answers and sets their own password in the browser, and changes it in
the console or with `rustybin passwd`. A caller grants only roles whose permissions it
holds, and manages only users whose permissions it holds.

The tools on mocks, saved tests and checkpoints take a [project](projects.md) into
account: `list_mocks`, `list_tests` and `list_checkpoints` take `project` as a filter,
`create_mock`, `save_test` and `save_checkpoint` the project to create in,
`update_mock` a `project` to move the mock to (`mocks.write` in both), and
`invite_user` `projects` (roles in projects). A member of a project sees these tools
when they hold the permission there; the call checks it on each resource, and another
project's resource answers as if it did not exist.


### Traffic

| Tool | Permission | |
|---|---|---|
| `list_requests` / `get_request` | `traffic.read` | The [request inspector](concepts/inspector.md), newest first, with filters (path, method, status, session, since); scoped by key in shared mode |
| `wait_for_requests` | `traffic.read` | Wait up to `timeout_secs` (default 30, at most 60) for the next captured requests matching a filter (path prefix, method, header, session, or a mock's calls) and return them |
| `clear_requests` | `traffic.clear` | Empty the inspector (or one session's entries) before a test run (confirmed) |
| `create_bin` / `get_bin` | `instance.read` / `traffic.read` | A [request bin](reference/request-bin.md) owned by the caller's session, and what arrived in it |
| `list_push_notifications` | `traffic.read` | What the [A2A](a2a/index.md) push notification sink received for a sink id |

`wait_for_requests` is for "I'll send it through the gateway now, tell me what arrived":
the agent starts the wait, you send the call, the answer lists what Rustybin received.
At most 100 waits run at once (20 in public mode); more answer a tool error asking to
retry.

### Testing

| Tool | Permission | |
|---|---|---|
| `try_request` | `instance.read` | Send a request to this instance's own data plane and return status, headers, body and timing |
| `issue_token` | `instance.read` | A token from the built-in [identity provider](reference/oidc.md) (client credentials of the demo client, with the asked scopes and audience), returned with its decoded claims and expiry |

`try_request` runs **in process** through the full application, with the same
middleware a real client meets (plan limits, fault injection, inspector capture), and
never opens a socket: it cannot reach another host. Control-plane paths (`/_rustybin/*`,
`/ui/*`) are refused, since managing the instance is what the other tools are for. The
request is captured under the session `mcp-<id>` (or the caller's
`X-Rustybin-Session`), so `list_requests` finds it. It carries the caller's API key in
shared mode, never the credential the agent connected with (admin token, control
token or manage token): add headers to the request when the target needs them. It
comes from the caller's own address, not `127.0.0.1`, so per-address limits count it
as the caller's. It suits MCP clients without a
shell; Claude Code may just use `curl`.

```hurl
{{#include ../examples/agent/manage.hurl:create_mock}}
```

```hurl
{{#include ../examples/agent/manage.hurl:try_request}}
```

```hurl
{{#include ../examples/agent/manage.hurl:control_plane_refused}}
```

A token from `issue_token` opens mocks with JWT security, the OAuth-protected mock MCP
server and a gateway's JWT validation:

```hurl
{{#include ../examples/agent/manage.hurl:issue_token}}
```

### Saved tests, gateway scenarios and checkpoints

| Tool | Permission | |
|---|---|---|
| `list_tests` | `tests.read` | The [saved tests](saved-tests.md), one by name, or all of them as a Hurl file (`format: hurl`); requests are under `untrusted_*` |
| `save_test` | `tests.write` | A test from a captured request (`capture`: its id) or a written one, with assertions; secret-looking headers and query values must be `{{variables}}` |
| `delete_test` | `tests.write` | Delete a saved test (confirmed) |
| `run_tests` | `tests.run` | Run every saved test or the ones named, in process or through each test's gateway; confirmed when a test goes through a gateway or sends a method other than `GET`, `HEAD` or `OPTIONS`; one run at a time. Results under `untrusted_results`, JUnit with `format: junit`; answers the kept run's `run_id` and the change since the previous run |
| `list_test_runs` | `tests.read` | The [history of runs](saved-tests.md#history), newest first (`limit`, `cursor`): when, the trigger, who, the counts, the failing tests, newly failing and newly passing |
| `get_test_run` | `tests.read` | One kept run by id: each test's result and failures under `untrusted_results`, JUnit with `format: junit` |
| `clear_test_runs` | `tests.write` | Forget every kept run, the tests stay (confirmed) |
| `list_test_schedules` | `tests.read` | The [test schedules](saved-tests.md#schedules), or one by name, with their status: next time, last result and run, skipped runs, the last notice |
| `save_test_schedule` | `tests.write` | Save or replace a schedule: `every` or `at`, `names` or `prefix`, `enabled`, `webhook`, `notify_recovery` (confirmed when it names a new webhook) |
| `delete_test_schedule` | `tests.write` | Delete a schedule, the tests and the history stay (confirmed) |
| `run_test_schedule` | `tests.run` | Run a schedule now, kept in the history as `schedule:<name>` (confirmed when its tests go through a gateway or change state, or it has a webhook) |
| `list_gateway_scenarios` / `get_gateway_scenario_run` | `instance.read` | The [gateway scenarios](gateway-scenarios.md) and the caller's runs with their last verdicts; the credential-injection key is shown to admin callers only (`key_hidden`) |
| `start_gateway_scenario` | `scenarios.run` | Prepare a run: its upstream and the steps to configure on the gateway |
| `check_gateway_scenario` | `scenarios.run` | Send the run's test traffic through the gateway (egress purpose `scenario`) and compare what the client got with what reached the upstream, with the scenario's test parameters (`limit`, `half_open_ms`, `size_limit`, `allowed_origin`, `refused_origin`, `upstream_cors`, `add_request_header`, `add_request_value`, `remove_request_header`, `add_response_header`) (confirmed) |
| `end_gateway_scenario` | `scenarios.run` | Remove what the run created, nothing else (confirmed) |
| `list_checkpoints` | `mocks.read` | The [checkpoints](checkpoints.md), or one with the mocks it holds |
| `save_checkpoint` | `checkpoints.write` | Save the instance's API mocks (with their data) and the health toggle; `replace` overwrites one of that name; at most 20 |
| `restore_checkpoint` | `checkpoints.restore` | Bring the mocks back to a checkpoint (API mocks it does not hold are deleted); `dry_run: true` answers the same without a change or a confirmation; confirmed otherwise; `from: "git"` restores the configuration repository's checkpoint of that name |
| `delete_checkpoint` | `checkpoints.write` | Delete a checkpoint, not the instance's mocks (confirmed) |
| `list_repository_checkpoints` | `git.read` | The checkpoints committed to the configuration repository (at the working copy's `HEAD`), each with what it holds or why it cannot be restored, under `untrusted_checkpoints` |

### Harness

Only when [load test mode](harness.md) is on (`RUSTYBIN_HARNESS=on`). Rights: **harness**
is the `harness` or `admin` scope, or the admin token on an open control plane.

| Tool | Permission | |
|---|---|---|
| `harness_status` | `harness.read` | Whether harness mode is on, the node's roles, caps, engine, allowed targets, the active run, the console URL. Where an agent starts |
| `list_scenarios` / `get_scenario` / `save_scenario` | `harness.read` / `harness.run` | The built-in scenarios to copy, one as an object and YAML (credentials redacted, as on the REST route), and saving your own by name |
| `list_patterns` | `harness.read` | The pattern types and their parameters, from the scenario schema |
| `validate_scenario` / `preview_scenario` | `harness.read` | Problems against the node's caps, the normalised scenario and its summary; the expected requests per second of every layer. Neither sends anything |
| `create_run` | `harness.run` | Validate, pin and (when configured) verify the targets and **start sending load** from this node, or submit to a cluster with `cluster_id`. Confirmed (elicitation, else `confirm: true`) |
| `start_run` | `harness.run` | Start a submitted cluster run on every client node at once. Confirmed |
| `update_run` | `harness.run` | Tune a running run: multiplier, pause, hold, skip, `set`, layers, faults. Confirmed |
| `list_runs` / `get_run` | `harness.read` | The node's runs, and the live view of one (progress, counts, latency, the last second, saturation, `report_ready`) |
| `stop_run` | `harness.run` | Stop a run early (it drains and reports); with `cluster_id`, on every client node |
| `get_run_report` | `harness.read` | The final report as JSON, text or JUnit |
| `harness_stop` | `harness.run` | The kill switch: all load on this node stops now (no confirmation) |
| `import_traffic` | `harness.read` | A HAR file or saved captures as a scenario's source, without credentials; sends nothing |
| `step_aside` | `harness.cluster` | Before maintenance: the leader hands the cluster to the first in the succession, with the run still on |
| `create_cluster` / `list_cluster_nodes` / `export_upstreams` / `get_leader` | `harness.cluster` / `harness.read` | A cluster led by this node (the join token once), its members, the server nodes' URLs for the gateway's upstream pool, and who leads with the succession and the history of leader changes |

Starting load reaches outside the instance, so `create_run`, `start_run` and `update_run`
carry `openWorldHint` and ask the user to confirm; targets must be in
`RUSTYBIN_HARNESS_TARGETS` whatever the tool. The prompt `load_test_gateway` walks an
agent from the allowlist to a validated scenario, a dry run at a low rate and the real
run, and `rustybin://harness.schema.json` is the scenario schema.

### Self-service

Only on instances with [self-service mocks](mocks/self-service.md).

| Tool | Permission | |
|---|---|---|
| `get_visit` | the visit | Time left, hours used today, what the next extension needs |
| `extend_visit` | the visit | One more period without the email step. When an address is needed the answer says so and links the console, where the form shows the consent choice and privacy notice |
| `list_visits`, `end_visit`, `pause_self_service`, `resume_self_service` | `visits.operate` | The [operator routes](mocks/self-service.md#operators); ending a visit and pausing are confirmed |

A visitor's agent needs no credential for its first mock: `create_mock` starts a visit
and returns its manage token, which the agent then sends as `X-Rustybin-Mock-Token`.
Other people's visitor mocks do not exist for the caller (`404`), exactly as on the
REST routes.

```hurl
{{#include ../examples/agent/manage_self_service.hurl:ss_agent_create}}
```

```hurl
{{#include ../examples/agent/manage_self_service.hurl:ss_agent_token}}
```

```hurl
{{#include ../examples/agent/manage_self_service.hurl:ss_agent_cleanup}}
```

## Prompts and resources

Clients that support MCP prompts offer these as commands:

| Prompt | Arguments | |
|---|---|---|
| `build_instance` | `brief`, `portal` | From a brief to a working instance: the AI provider, describe and create each API, fill the examples, publish to the portal, export the configuration, offer the git push (see [the walkthrough](#build-an-instance-with-an-agent)) |
| `mock_an_api` | `spec_path`, `name` | From a spec, or a description when there is no file, to a deployed, tested mock: preview, create, check, try a request, hand over the URL and a collection |
| `publish_portal` | `items` | Metadata per item, publish, switch the portal on, verify what visitors see |
| `backup_to_git` | `remote`, `branch` | Connect the repository, commit with a message from the drift, push (confirmed), confirm the result |
| `test_mcp_server` | `url`, `auth` | Reachability through the relay, connect, the check suite, the report |
| `onboard_agent` | `name` | The credential model, the agent's record, a proposal of rights for the operator to confirm |
| `debug_gateway_call` | `path` | Find the request a gateway sent and explain what arrived (headers, auth, validation) against what was expected |
| `load_test_gateway` | `target`, `rate` | On a [harness](harness.md) node: check the allowed targets, build and validate a scenario, preview it, dry-run it at a low rate, then run it for real and read the report |
| `secure_a_mock` | `name`, `mode` | Pick a mocked security mode and show the credentials a gateway must present |

| Resource | |
|---|---|
| `rustybin://config.schema.json` | The JSON Schema of `rustybin.yaml` version 2, for writing configuration files |
| `rustybin://catalogue` | The route catalogue as JSON |
| `rustybin://harness.schema.json` | The JSON Schema of a [harness](harness.md) scenario, with the node's caps as bounds |
| `rustybin://portal/catalog` | What visitors of the [dev portal](portal.md) see, as the caller's request reaches the instance |
| `rustybin://cli.md` | The [command line reference](cli.md), for the commands to suggest when something is done from a shell or CI |

```hurl
{{#include ../examples/agent/manage.hurl:resources}}
```

## Build an instance with an agent

The `build_instance` prompt carries an agent from a brief ("an orders API and a
parcels API for the checkout team, published for the mobile developers") to a working
instance. These are the calls it makes, on an instance with the mock provider; every
one is a `tools/call` of the same server. The agent starts with the provider:

```hurl
{{#include ../examples/agent/manage_build.hurl:build_assist_status}}
```

It describes an API and shows the result to the user before anything is created:

```hurl
{{#include ../examples/agent/manage_build.hurl:build_generate_spec}}
```

Creating from the generated document is confirmed, like every change an AI wrote
(the user confirms through elicitation, or the agent passes `confirm: true` once they
agreed):

```hurl
{{#include ../examples/agent/manage_build.hurl:build_create}}
```

`check_mock` says which operations lack examples; `generate_examples` fills them
(from the provider, or the deterministic generator with `source: generated`), and
`deploy_mock` publishes the draft:

```hurl
{{#include ../examples/agent/manage_build.hurl:build_coverage}}
```

```hurl
{{#include ../examples/agent/manage_build.hurl:build_examples}}
```

Publishing to the portal is confirmed when the portal is open to anyone:

```hurl
{{#include ../examples/agent/manage_build.hurl:build_publish}}
```

The whole instance comes back as a version 2 document, with secrets as references:

```hurl
{{#include ../examples/agent/manage_build.hurl:build_export}}
```

Without a repository `git_status` says what to set, and a push is always confirmed:

```hurl
{{#include ../examples/agent/manage_build.hurl:build_git}}
```

The agent sees itself in the agents list, and can test the mock's own MCP server
through `mcp_call` (the server's answer is third-party data, under `untrusted_result`):

```hurl
{{#include ../examples/agent/manage_build.hurl:build_agents}}
```

```hurl
{{#include ../examples/agent/manage_build.hurl:build_mcp_call}}
```

Unpublishing and deleting are the usual tools (the delete confirmed):

```hurl
{{#include ../examples/agent/manage_build.hurl:build_cleanup}}
```

The same walk runs in the [MCP conformance suite](https://github.com/coldbathcode/rustybin/tree/main/conformance/mcp)
with the official Python SDK.

## Safety

### Confirmations

Tools that destroy something carry the MCP `destructiveHint` annotation (reads carry
`readOnlyHint`) and ask before acting: `rollback_mock`, `delete_mock`,
`clear_mock_data`, `clear_traffic`, `apply_config` with `prune`, `clear_requests`,
`end_visit`, `pause_self_service`, `revoke_agent`, `unpublish_registry_server`,
`git_resolve` and `git_switch_branch` (not their dry runs), `git_deploy_key` and
`git_signing_key` with `replace`, `end_session` with `all` or `user`,
`delete_user`,
`delete_test`, `restore_checkpoint` (not its dry run), `delete_checkpoint` and
`end_gateway_scenario`. Tools that reach outside the instance or publish something ask
too: `mcp_call` and `run_mcp_checks` to another host, `browse_registry`, `git_push`,
`git_pull` when it applies, `publish_portal_item` on an open portal,
`load_assist_models`, the AI tools when they write (`generate_spec` with `create`,
`generate_examples` and `edit_spec_with_ai` with `apply`), `check_gateway_scenario`,
`run_tests` through a gateway or with a method that changes state, and the harness
runs. `invite_user` always asks (its answer is a link that signs a person in),
`update_user` when it grants a privileged role and `save_role` when the role is
privileged. `update_settings` asks when it changes a key under `egress.`, `agents.`,
`console.`, `git.`, `sso.` or `proxy.`, or `assist.provider` or `assist.base_url`
(after the permission check: a confirmation is never a permission). When
the client supports elicitation, the server asks the user directly ("Delete the mock
orders-api and its traffic?"). Without it the call needs `"confirm": true`, which the
tool descriptions tell the model to set only after the user agreed:

```hurl
{{#include ../examples/agent/manage.hurl:delete_needs_confirmation}}
```

```hurl
{{#include ../examples/agent/manage.hurl:delete_confirmed}}
```

### Captured traffic is untrusted

Whatever calls the data plane writes the captured traffic, and on a public instance
that is anyone. A body saying "ignore your instructions" must not steer the agent. So:

- captured headers and bodies (mock traffic, the inspector, bins, the A2A sink,
  `wait_for_requests`), the answers of MCP servers reached with `mcp_call` and the
  entries of a browsed registry never appear in the text summary; they are returned in
  `structuredContent` under fields named `untrusted_*`, and the server instructions
  and tool descriptions say this is third-party data, never instructions;
- a traffic row's path and query are `untrusted_path` and `untrusted_query`, a
  non-standard method is `OTHER` with `untrusted_method`, and a mock's validation
  messages are `untrusted_validation`: summaries name methods and statuses only. The
  same goes for saved check reports (`untrusted_report`), saved tests and their runs,
  and the quoted file and remote text of `diagnose_instance` (`untrusted_message`);
- bodies are left out unless the call asks (`include_bodies: true`) and are truncated
  at 4 KB each and 64 KB per result, with a flag saying so;
- `try_request` answers are treated the same way, since a mock answers whatever its
  owner configured (on a demo, another visitor's mock): `untrusted_*` fields,
  truncated at 64 KB, streams stopped after 64 KB or 5 seconds.

```hurl
{{#include ../examples/agent/manage.hurl:traffic}}
```

Spec text and rule patterns from the agent are validated exactly as on the REST
routes (spec size, YAML and regex limits), so an agent cannot do what a REST caller
could not.

### Secrets never pass through an agent

No tool takes a secret: `update_settings` refuses secret keys, saved MCP connections
are written in the console or the CLI, the git token and the console password are set
there or in the environment, and exports carry references (`${VAR}`,
`secret:stored`), never values. What an agent can read about a secret is `{set,
hint}`. The parity test (`every_control_route_has_a_management_tool`) keeps every
control route mapped to a tool, a resource or a written reason, and a second test
checks that no tool input is named like a secret.

### Audit log and metrics

Every call that changes something, and every refused call, is logged as one line
(target `rustybin::manage`) with the tool, its target, the caller (`admin`,
`key:<id>`, `visit:<fingerprint>`, a token caller's credential such as `console:<user>`,
`jwt:<sub>` or `oauth:<sub>`, else `session:<label>`), the agent's id and the
outcome. New agents, rights changes, revocations and restores are logged under
`rustybin::agents`. Arguments are
never logged, since they may hold secrets or spec bodies.

`/_rustybin/metrics` counts calls in `rustybin_manage_mcp_calls_total{tool, outcome}`
(outcome `ok`, `denied` or `error`; invalid arguments count as `error`, and a request
refused before it reaches a tool, such as an unknown tool name, is not counted). Tool calls are control-plane requests,
so [plan limits](concepts/plans-and-limits.md) apply as for the REST routes.

## What to ask

- "Mock `openapi/orders.yaml`, check it, and give me a Hurl collection pointed at
  `https://gateway.local/orders`."
- "Make `GET /orders/{id}` answer 404 when the id is 0, then show me that it does."
- "Secure the orders mock with JWT, issue me a token with `orders:read`, and try a
  call with it."
- "I'm sending a request through the gateway now: tell me what headers arrived."
- "Why am I getting 429s?"
- "Export everything as `rustybin.yaml` so I can commit it."
- "Build the mocks for the checkout team from this brief and publish them to the
  portal."
- "Back this instance up to `git@github.com:team/mocks.git`."
- "Run the MCP checks against `https://mcp.acme.example/mcp` with this bearer token."
- "What has the `claude-code` agent been doing, and should it keep admin rights?"

## Not in this version

- Platform operations (creating or deleting instances) and the settings that only the
  environment sets (ports, TLS, the state directory, the control auth mode): those stay
  in the deployment. The [instance settings](configuration.md#state-directory-settings-and-secrets) are
  tools.
- Setting a secret (a provider key, the git token, a saved connection's credential,
  the console password): the console, the CLI or the environment.
- Fetching a spec from a URL on the server: the agent reads the file and sends its
  content.
- The self-service email step: consent stays in the console.
- OAuth sign-in for self-service visitors and shared-mode key holders (they send their
  manage token or API key as a header), and client ID metadata documents (the server
  would have to fetch a URL).

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `POST /_rustybin/mcp` (the server), `GET /_rustybin/agents` (each agent's `owner`, `?owner=`), `GET /_rustybin/agents/events`, `GET`, `PATCH /_rustybin/agents/{id}` (a role as the ceiling), `POST /_rustybin/agents/{id}/revoke \| restore`, `DELETE /_rustybin/agents/{id}/sessions`, the OAuth routes under `/_rustybin/oauth` (the consent request's `permissions`, `grants` and `approver`) |
| Console | The **AI agents** view (the list with the **Owner** column and its filter, rights with a role as the ceiling, revoke, the live feed, **Connect an agent** with the `claude mcp add` command), `#/authorize/<id>` for the agent sign-in (the permissions the agent will hold) |
| CLI | `rustybin agents list [--owner U] \| get \| rights [--ceiling ROLE] \| revoke \| restore \| end-sessions \| tail`, `rustybin mcp-stdio` ([reference](cli.md#agents)) |
| Management MCP | `list_agents` (`owner`), `get_agent`, `set_agent_rights` (a role as the ceiling), `revoke_agent` (never oneself), `restore_agent`, `end_agent_sessions` (rights, restore and end sessions never on an agent of the caller's own credential; a user's own agents without `agents.read` / `agents.manage`); the prompt `onboard_agent` |
| `rustybin.yaml` | `agents:` (`default_ceiling`, `rules` by client name, credential kind, user and role, a role as the ceiling), `settings: {agents.default_ceiling_for: [...]}` ([the whole instance as code](configuration-as-code.md)) |
| Environment | `RUSTYBIN_AGENTS_DEFAULT_CEILING`, `RUSTYBIN_CONTROL_OAUTH`, `RUSTYBIN_CONTROL_OAUTH_ISSUER`, `RUSTYBIN_CONTROL_OAUTH_KEY` ([configuration](configuration.md#control-plane-and-console)) |
