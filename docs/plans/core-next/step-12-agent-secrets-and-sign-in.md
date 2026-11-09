# Step 12: A secret at an agent's request, and an agent sign-in from the CLI

Core next, the agents step. Section 5 of the master plan holds the choices: a secret is still never
a tool input, the CLI walks the control plane's own OAuth, and OAuth for visitors and key holders is
not built.

## Goal

Two things an agent on the management MCP server could not do without a person pasting something:

- **Get a secret set.** Today `update_settings` refuses a secret and points at the console, the CLI
  or the environment, and the conversation stops there. Now the agent asks for the secret to be set,
  a person who may set it types the value in the console or gives it to the CLI on stdin, and the
  agent reads that it was done. The value never passes through the agent, a tool, a log or the
  audit trail.
- **Sign in from the terminal.** `rustybin mcp-stdio` needs a token someone pasted (the admin token,
  or the profile's sign-in token of a person). `rustybin login --agent` walks the built-in OAuth
  server in the browser instead, as an MCP client with a browser does, and keeps the tokens in the
  profile for `mcp-stdio`.

## A. A secret set at an agent's request

### The rule that stays

A secret is never a tool input or output. `every_tool_is_fully_defined` is not changed and gets no
exception: the two new tools take a setting's key, a reason and a request id, and answer a request's
state. What confirms is a person, out of band: the console or the CLI, each with its own credential.

### The model (`src/secret_requests.rs`, `AppState.secret_requests`)

A request is: an id (`sr_` and 16 hex characters, a name, never a capability), the setting's key,
the agent (id, client name, the credential's label, the owner as the agents view shows it), the
reason, when it was made, when it expires, and its state: `pending`, `fulfilled`, `declined` or
`expired`, with who resolved it and when.

- **Off until someone turns it on.** The setting `agents.secret_requests` (a switch, default off,
  group `agents`, so changing it needs `settings.security` and `update_settings` asks the user to
  confirm). While it is off the asking tool answers how to turn it on.
- **What may be asked for.** A setting that exists and is a `secret()` spec. Never
  `console.password_hash` (a password passes through nobody but its owner). Never a key
  `RUSTYBIN_SETTINGS_LOCK` holds (nobody could fulfil it; the answer names the variable).
- **Who may ask.** The tool declares `settings.write`, the permission `update_settings` declares,
  so the ceilings treat it the same way (`full`, or a role that grants it: never `read-only`,
  `traffic` or `manage`), and a revoked agent is refused by `tools::call` before the tool runs. The
  caller must also hold what changing a setting of that group needs (`settings.security` for
  `sso.*`, `git.write` for `git.*`, `portal.settings` for `portal.*`): everything a person needs to
  set it except `secrets.write`. No new permission.
- **Who may fulfil.** Whoever may set that secret today: `settings.write`, `secrets.write` and the
  group's permission, through `Settings::update`, so the lock, the size and character checks and
  the spec's own check apply unchanged. Nobody fulfils more than they could set themselves, and a
  refusal names the missing permission. Declining and listing need `secrets.write`.
- **Bounds.** 32 pending in all, 3 pending per agent, 10 asked per agent per hour, 128 kept. A
  second ask for the same key by the same agent while one is pending answers the pending one.
- **Expiry.** A pending request expires 15 minutes after it was made. A resolved one (fulfilled,
  declined, expired) stays readable for an hour, then goes. On the injectable `limits::Clock`.
- **Ownership.** `get_secret_request` answers the calling agent's own requests only; another
  agent's id is "no such request". The REST list is everyone's and needs `secrets.write`.
- **Where they live.** In memory, as the OAuth sign-ins under way, the single sign-on codes and the
  gateway scenario runs do: a request that outlives a restart would outlive the conversation that
  made it. Nothing in `rustybin.yaml` (a pending request is not instance state); the switch is a
  setting, so a document carries it.
- **Never in an answer:** the value, a hint beyond the settings API's `{set, hint}` (the answers
  carry `set` only), or a link that grants anything (the console address needs a sign-in).

### The reason is an agent's claim

The reason is text an agent wrote, shown to a person who is about to type a secret: the place a
prompt injection would aim at. So: control characters dropped, capped at 300 characters, stored as
text and rendered as text (the console sets `textContent`, the CLI prints it through
`logging::printable`). The console shows it under "The agent says", apart from what the instance
knows itself: which agent (its name is a claim too, and says so), whose it is, which credential,
which setting (the registered title and key) and when it expires. The reason is never in the audit
log. REST answers carry it as `untrusted_reason`, and the agent's client name as
`untrusted_agent_name`.

### Surfaces

| Surface | What |
|---|---|
| API | `GET /_rustybin/secret-requests`, `GET /_rustybin/secret-requests/{id}`, `POST .../{id}/fulfil` (`{"value"}`), `POST .../{id}/decline` |
| Console | Management, every tab: a card "Secrets an agent asked for" with the value field, Set and Decline |
| Management MCP | `request_secret` (asks), `get_secret_request` (reads the caller's own) |
| CLI | `rustybin settings requests`, `settings fulfil <id> [--stdin]`, `settings decline <id>` |
| `rustybin.yaml` | the `agents.secret_requests` setting; requests themselves are not instance state |

Asking is a tool only: an agent exists on the management MCP server (a credential and a client
name), a REST caller is nobody's agent and sets the secret itself or cannot.

Where the client supports URL elicitation (the `elicitation.url` capability), `request_secret`
tells the person at the agent where to go: the console's Management address, never a form (nothing
typed there could be the value).

### Audit

Asking is the tool call's entry (`request_secret`, targets `setting:<key>` and
`secret-request:<id>`, event `secret_request`). Fulfilling and declining are the routes' entries
(event `secret_fulfilled` / `secret_declined`, targets the request, the setting and the agent).
None holds the value or the reason. `nothing_secret_reaches_an_entry_or_a_file` plants a value
through the fulfil route and a secret-looking reason through the tool, and greps.

## B. An interactive agent sign-in from the CLI

`rustybin login --agent [--url U] [--profile P] [--scope S] [--no-browser] [--wait S]`:

1. `POST /_rustybin/oauth/register` as a public client named `Rustybin CLI` with the redirect URI
   `http://127.0.0.1/callback` (the server matches a loopback URI without its port, RFC 8252). The
   metadata is the same at every sign-in, so the client id, and with it the agent, is the same one:
   its rights and a revocation survive a new sign-in.
2. A loopback listener on a free port; the browser opens
   `/_rustybin/oauth/authorize` with PKCE (S256), `state` and the listener's address
   (`Env.browser` in tests, `--no-browser` prints the address).
3. The user approves on the console's `#/authorize/<id>` view; the browser comes back to the
   listener, which answers only the callback carrying this sign-in's `state`.
4. `POST /_rustybin/oauth/token` with the code and the verifier. The access token (an hour), the
   refresh token (thirty days) and the client id go in the profile under `agent`, beside the
   person's `token`, in `profiles.json` (0600).

The tokens are the authorization server's as it is today: nothing longer-lived is invented.
`rustybin mcp-stdio` uses a profile's agent sign-in when it has one, renews the access token with
the refresh token when it is about to expire (and once after a `401`), and writes the new pair
back. When the refresh is refused (revoked, signed out everywhere, the user disabled, thirty days
passed) it says to sign in again.

**The credential is for `mcp-stdio` only.** These tokens are accepted on `/_rustybin/mcp` and
nowhere else (`control_auth::enforce`; `admin::admin_check` never accepts them): unchanged. The
other commands never send a profile's agent sign-in; a profile that holds only an agent sign-in,
and an agent token given as `--token` or `RUSTYBIN_TOKEN`, are refused before any call with a
message that says so (exit 3), instead of the instance's `401`. Help and the docs say it plainly.

The agent this creates is `Credential::User { id, client }` for the approving user: listed in AI
agents with its owner, limited and revoked like any other. `rustybin logout` forgets the profile's
agent sign-in too (revoking is `rustybin agents revoke`).

## C. Not in this step

OAuth sign-in for self-service visitors and shared-mode key holders. It reopens step 04b of the
hosted demo (`control_oauth::enabled` stays off on a self-service instance and in shared mode).

## Tests that fail first

- `secret_requests::tests`: the caps (pending, per agent, per hour, kept), expiry and the hour a
  resolved request stays, on a manual clock; the same ask twice answers one request; the reason is
  cleaned and capped.
- `mcp::manage` tests: `request_secret` is refused while the switch is off, for an unknown key, a
  key that is no secret, `console.password_hash`, a locked key, a caller without `settings.write`
  or the group's permission, an agent under the `manage` ceiling, a revoked agent; an agent reads
  its own request and not another's; no answer holds the value.
- REST: fulfil writes the secret (the setting reads `set`), needs `secrets.write`, `settings.write`
  and the group's permission (a `403` naming the one missing), answers `409` under
  `RUSTYBIN_SETTINGS_LOCK`, `410` for an expired or resolved request, refuses an empty value;
  decline; the list needs `secrets.write`.
- Audit: `every_mutating_route_and_tool_writes_one_entry` covers the new routes and tool;
  `nothing_secret_reaches_an_entry_or_a_file` with the planted value and reason.
- Parity: `every_control_route_has_a_management_tool`, `the_command_tree_is_consistent_with_the_catalogue`,
  `every_control_route_declares_a_permission`, `every_tool_declares_a_permission` (the docs' table),
  `ceilings_keep_their_tool_counts` (two rows).
- CLI: `login_agent_walks_the_control_planes_oauth` (the browser driven through `Env.browser`, the
  profile, the agent in `agents list` with its owner, `mcp-stdio` picking the agent token, a
  refresh, a revoked agent's refresh refused); the other commands refuse an agent token with the
  message; `settings fulfil` reads the value from stdin and never takes it as an argument.
- Console: `conformance/ui` fulfils a request and declines one.

## Build notes (2026-11-09)

- **Where it is.** The requests, their routes, catalogue and OpenAPI in `src/secret_requests.rs`
  (`AppState.secret_requests`, tests in `src/secret_requests/tests.rs`); the two tools in
  `src/mcp/manage/secrets.rs` (at the end of `TOOLS`, 141 for an admin); the CLI's verbs in
  `src/cli/commands.rs` (`settings requests | fulfil | decline`); the console's card in
  `ui/lib/secretrequests.js`, mounted by `ui/views/manage.js` above the tabs. The agent sign-in in
  `src/cli/agent_login.rs` (`login`, `refresh`, `AgentSession`, `is_agent_token`),
  `Profile.agent` in `src/cli/profiles.rs`, the bridge's use of it in `src/mcp/manage/stdio.rs`,
  tests in `src/cli/tests/agent.rs`. Nothing changed in `src/control_oauth.rs`, `src/login.rs` or
  `src/control_auth.rs`: the server side of the sign-in is the one MCP clients with a browser
  already walk. User docs: `docs/src/agent-mcp.md` ("A secret at an agent's request", "Connect an
  agent"), `docs/src/cli-guide.md`; examples `docs/examples/agent/secret_requests.hurl` and the
  `secret_requests` anchor of `docs/examples/cli/cli.sh`.
- **Decision: the permission for asking is `settings.write`, and no permission is new.** The tool
  declares what `update_settings` declares, so every ceiling and every role reads it the same way
  (`ceilings_keep_their_tool_counts`: `request_secret` an admin change, `get_secret_request` an
  admin read, which the `read-only` and `traffic` ceilings let through as they do `test_sso`).
  Beyond it the caller needs what a setting of that group needs without being a secret
  (`settings::extra_permissions(key, false)`), checked against the caller as `update_settings`
  checks its keys: the agent's ceiling is applied to the tool's own permission only, exactly as
  there. Reading and declining need `secrets.write` (the routes' declaration); fulfilling adds
  `settings.write` and the group's permission (a row of `BODY_CHECKS`). The summary of
  `secrets.write` in the permission table says so.
- **Decision: the caps.** 32 pending in all, 3 pending per agent, 10 asked per agent per hour,
  128 kept, 15 minutes to answer, an hour readable once resolved. When the store is full an ask is
  refused; nothing is evicted before its time, so the per-hour count stays exact.
- **Decision: in memory.** As the OAuth sign-ins under way (`control_oauth::OAuthState`), the
  single sign-on codes and the gateway scenario runs. A restart drops every request, which an
  agent reads as "no such request" and asks again.
- **Decision: off by default** (`agents.secret_requests`, no environment variable, as
  `agents.default_ceiling_for`). With `RUSTYBIN_SETTINGS_LOCK` the switch is locked off like every
  lockable setting without a variable, which fits: nothing could be fulfilled there. Turning the
  switch off stops new requests; the ones that wait stay answerable until they expire.
- **A revoked agent's request.** Asking is refused by `tools::call` like every tool. A request
  made before the revocation is declined at the fulfil (`410`, `reason: agent_revoked`), nothing
  is set. Disabling or deleting the agent's user revokes the agent (testing-2 step 12), so the
  same holds.
- **Deviation: elicitation on 2026-07-28 only.** The design said URL elicitation where the client
  supports it. It is sent inside the call's own answer (`input_required`, the client calls again
  and the second call finds the request it made). A handshake-era session is not asked: the
  server would hold the tool call open until the client answered, the stdio bridge does not relay
  such requests at all, and there was no test for it. Never a form in either era.
- **Deviation: the audit target is `secret-request:<id>`** (a hyphen), the name the audit layer
  derives from the route, used by the tool's entry too.
- **Asking has no route and no command**: an agent exists on the management server only. The
  CLI's example and the Hurl example ask with a `tools/call`.
- **The sign-in keeps one registration.** `Rustybin CLI` with `http://127.0.0.1/callback`: client
  ids are stateless and signed, so the same metadata is the same client id and the same agent
  (`user:<id>:oauth:client:<fingerprint>` with the client's name from `clientInfo`). A revoked
  agent's new sign-in is refused at the token endpoint until someone restores it.
- **Decision: `login --agent` leaves the default profile alone** when there is one, so a person's
  commands keep going to the instance they signed in to. `login` (the person's) keeps the
  profile's agent sign-in when it is for the same URL. `logout` forgets both.
- **No `iss` check on the callback** (RFC 9207): the command line talks to one authorization
  server, at the URL it was given, and the code is redeemed there or nowhere; `state` and PKCE
  bind the answer to this sign-in.
- **The clear refusal is the command line's.** An agent token is recognised by its claims
  (`rb_use`, a client id of ours) without verifying it, only to choose the message; the instance
  still answers `401` to anyone who sends one elsewhere, unchanged.
- **`loopback_answer` takes the parameter's name** (`cli_state` for single sign-on, `state` here).
- **Console.** The AI agents policy card's button is now "Save the agents settings" (it saves the
  switch too). The card of requests shows only for a caller with `secrets.write`.
- **Not built.** OAuth for visitors and key holders (section C). A note from the person to the
  agent on a decline. Telling a signed-in person about a waiting request outside Management (a
  badge on the gear). A pending request is not offered to the agent's own user when they lack
  `secrets.write` (they see the agent, not its requests).

## For the merge

These files were left alone on purpose; the text for each:

**`CLAUDE.md`**, a paragraph after the core-next one ("Rustybin 0.6, more from an instance"), or
appended to it when the three steps land together:

> Step 12 (agents; `docs/plans/core-next/step-12-agent-secrets-and-sign-in.md`): a secret set at
> an agent's request (`src/secret_requests.rs`, `AppState.secret_requests`, the tools in
> `src/mcp/manage/secrets.rs`, `ui/lib/secretrequests.js`, `rustybin settings requests | fulfil |
> decline`; user docs `docs/src/agent-mcp.md#a-secret-at-an-agents-request`) and the agent
> sign-in from the CLI (`rustybin login --agent`, `src/cli/agent_login.rs`, `Profile.agent`, used
> and renewed by `rustybin mcp-stdio`). Rules it left: a secret is still never a tool input or
> output (`request_secret` and `get_secret_request` carry a setting's key, a reason and a state;
> `every_tool_is_fully_defined` is unchanged), and fulfilling has no tool; a value an agent asked
> for is written by a person's own call through `Settings::update`, with what setting that secret
> needs (`settings::check_keys`), never by a path of its own; what an agent wrote for a person to
> read (its reason, its name) is answered as `untrusted_*`, cleaned and capped
> (`secret_requests::clean_reason`), rendered as text, and never in an audit entry; requests are
> in memory, bounded and expiring, and no document carries one; an agent sign-in's tokens are
> for `/_rustybin/mcp` only, so no command but `mcp-stdio` sends `Profile.agent`, and
> `Ctx::client` refuses an agent token before any call (`agent_login::ONLY_MCP`); the CLI
> registers one client (`agent_login::CLIENT_NAME`, `REDIRECT_URI`), so a new sign-in is the same
> agent.

Also in the Layout list: `src/secret_requests.rs` next to `src/settings.rs`, and `agent_login.rs`
in the command line paragraph.

**`docs/src/changelog.md`**, under the next version's **New**:

> - [A secret at an agent's request](agent-mcp.md#a-secret-at-an-agents-request): with the
>   `agents.secret_requests` setting on (off by default), an agent asks for a secret setting to
>   be set (`request_secret`: the setting and a reason) and a person types the value in
>   **Management** or gives it to `rustybin settings fulfil <id>` on stdin, or declines. The value
>   never passes through the agent: the tools answer a request's state only. Requests expire after
>   15 minutes; `GET /_rustybin/secret-requests`, `POST .../{id}/fulfil | decline`
>   (`secrets.write`, and what setting that secret needs).
> - [An agent's sign-in from the command line](agent-mcp.md#connect-an-agent): `rustybin login
>   --agent` walks the instance's own OAuth sign-in in the browser and keeps the agent's tokens in
>   the profile; `rustybin mcp-stdio --profile <name>` uses and renews them. The credential works
>   with `mcp-stdio` only, and the other commands say so.

and under **Changed**:

> - The management MCP server has 141 tools for an admin (`request_secret`,
>   `get_secret_request`); the count in `docs/src/coding-agents.md` and in
>   `users_and_roles_tools_invite_confirm_and_never_carry_a_password` moves with the other steps'
>   tools.
> - `profiles.json` may hold an `agent` object per profile. An older `rustybin` reads the file
>   and ignores it.

**`docs/src/roadmap.md`**, "Under consideration": remove the line "**The command line**: an
interactive agent sign-in from the CLI." and change the agents line to:

> - **Agents**: OAuth sign-in for self-service visitors and shared-mode key holders.

and add both to the list of what is built, linking `agent-mcp.md#a-secret-at-an-agents-request`
and `agent-mcp.md#connect-an-agent`.

**`docs/plans/core-next/master-plan.md`**: step 12 done 2026-11-09, this file as its step file.

**Shared tables this step appended to** (a conflict there is two appends, keep both): `TOOLS`
and its dispatch arms and `tool_targets` keys (`src/mcp/manage/tools.rs`), `OLD_RIGHTS`
(`src/mcp/manage/agents/tests.rs`), `COVERAGE` (`src/mcp/manage/tests.rs`, and the tool count
141 there), `COMMANDS` and the `login` row (`src/cli/commands.rs`; three OAuth rows left
`UNCOVERED`), `NEW_SINCE_0_4`, `BODY_CHECKS`, `ROUTERS`, `SOURCES`, `MODULE_PATHS`,
`MODULE_COMPONENTS`, `settings::specs()`. Generated files to regenerate after the merge rather
than merge by hand: `README.md` (`RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`) and
`docs/src/cli.md` (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`).
