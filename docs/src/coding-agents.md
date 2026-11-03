# Connect your coding agent

Your coding agent can build your test platform on a Rustybin instance for you:
OpenAPI mocks with examples and security, the mock LLM wired into your gateway,
gateway scenarios, saved tests, load tests, the dev portal and a git backup. It does
this through the instance's [management MCP server](agent-mcp.md), which has a tool
for every control-plane route, so whatever the console or the command line can do,
the agent can do too.

This page is for developers who use Claude Code, Codex CLI, Cursor, OpenCode, VS Code
with Copilot, Windsurf, Gemini CLI or Zed. It shows how to connect each one, which
rights to give it, and what to ask once it is connected. What the server offers
(every tool, the confirmations, the OAuth flow in detail) is on [the management MCP
page](agent-mcp.md).

## What you need

Three things:

1. **The instance URL**, for example `http://localhost:8080` or
   `https://mocks.example.com`.
2. **The MCP URL**: the instance URL plus `/_rustybin/mcp`, for example
   `https://mocks.example.com/_rustybin/mcp`. It speaks streamable HTTP. This is not
   `/mcp`, the [mock MCP server](mcp/index.md) for MCP gateway demos.
3. **A credential**, unless the client signs in with OAuth:

| Way | What you get | Lasts |
|---|---|---|
| The console: **AI agents**, **Connect an agent** | The command and JSON with the token the console holds, masked until you ask | That token's life (12 hours for a password sign-in) |
| `rustybin login --url <instance>` | A profile on your machine; the [stdio bridge](#clients-that-only-speak-stdio) uses it | 12 hours, then sign in again |
| `POST /_rustybin/login` | A sign-in token, from a script | 12 hours |
| `RUSTYBIN_ADMIN_TOKEN` set on the instance | The admin token, sent as a bearer | Until you rotate it |
| A [control JWT](concepts/control-plane-security.md#signed-access-tokens-jwt-mode) | A token with the scopes you chose (`admin`, `console`, `inspector`) | Its `exp` |
| OAuth sign-in from the client | A token bound to `/_rustybin/mcp`, approved in the console, renewed by the client | An hour, refreshed for 30 days |

A script gets a sign-in token like this (the user name and password of the console;
`rustybin` / `rustybin` until you change it):

```hurl
{{#include ../examples/concepts/login.hurl:login}}
```

That token opens the management server like the rest of the control plane:

```hurl
{{#include ../examples/concepts/login.hurl:mcp}}
```

Keep the token in an environment variable, not in a file you commit. The examples
below read it from `RUSTYBIN_TOKEN`:

```bash
export RUSTYBIN_TOKEN='<token>'
```

A 12 hour sign-in token suits a session at the keyboard. For an agent you keep
connected, use OAuth where the client supports it (it renews its own token), or the
admin token on an instance only you use.

## A local or a remote instance

**On your laptop.** Start the image with an admin token you choose, so the agent's
credential does not expire:

```bash
export RUSTYBIN_TOKEN=$(openssl rand -hex 32)
docker run -d --name rustybin -p 8080:80 \
  -e RUSTYBIN_ADMIN_TOKEN="$RUSTYBIN_TOKEN" \
  -e RUSTYBIN_STATE_DIR=/app/certs/state -v rustybin-data:/app/certs \
  ghcr.io/coldbathcode/rustybin:latest
```

The MCP URL is `http://localhost:8080/_rustybin/mcp`. The state directory keeps the
mocks, settings and agents' rights across restarts (the image makes `/app/certs`
writable; see [My changes disappeared after a
restart](troubleshooting.md#my-changes-disappeared-after-a-restart)). The token
stays in your shell's `RUSTYBIN_TOKEN`, where the client configurations below read
it. With `RUSTYBIN_CONTROL_AUTH=open` instead, nothing asks for a credential and the
agent needs no header: fine on a laptop that nobody else can reach, never on a
shared network or the internet. On an open control plane anyone can connect under
any name, so the agent rights below are advisory there.

**A remote instance.** Use HTTPS, change the console password (**Management,
Sign-in**), and give each person's agent its own credential: OAuth sign-in gives
every client its own token and its own entry in the agents list, which you can limit
or revoke without touching anyone else. See [control-plane
security](concepts/control-plane-security.md) and, for an instance on the internet,
[the threat model](concepts/public-instances.md).

## Configure your client

Each client below reads the token from `RUSTYBIN_TOKEN` where it can expand a
variable, and can sign in with OAuth instead of a header. Each section ends with
how to check the connection and the stdio bridge as a fallback. Replace `<host>`
with your instance's host (and port).

### Claude Code

One command, for every project on your machine (`--scope user`; the default,
`local`, is this project only and private to you):

```bash
claude mcp add --transport http --scope user rustybin https://<host>/_rustybin/mcp \
  --header "Authorization: Bearer $RUSTYBIN_TOKEN"
```

The shell puts the token itself in `~/.claude.json`. To keep only a reference,
which Claude Code expands from the environment when it starts, write the server as
JSON in single quotes, or in a project's `.mcp.json` (`--scope project`, a file you
can commit; Claude Code asks before it uses it):

```bash
claude mcp add-json --scope user rustybin \
  '{"type":"http","url":"https://<host>/_rustybin/mcp","headers":{"Authorization":"Bearer ${RUSTYBIN_TOKEN}"}}'
```

```json
{
  "mcpServers": {
    "rustybin": {
      "type": "http",
      "url": "https://<host>/_rustybin/mcp",
      "headers": { "Authorization": "Bearer ${RUSTYBIN_TOKEN}" }
    }
  }
}
```

**OAuth:** add the server without `--header`, then run `/mcp` in a session (or
`claude mcp login rustybin`) and approve in the browser. With a header set, Claude
Code reports a failed connection on a `401` and does not offer OAuth.

**Check:** `claude mcp list`, or `/mcp` in a session. **Prompts:** `/` lists them
as `/rustybin:build_instance (MCP)`; type `/mcp__rustybin__build_instance` with the
arguments after it, one word each. **stdio:** `claude mcp add --transport stdio
rustybin -- rustybin mcp-stdio --profile <name>`. Tool permission rules take
`mcp__rustybin__*` or one tool's full name, `mcp__rustybin__delete_mock`.

### Codex CLI

Codex reads the token from the variable you name, so the file never holds it:

```bash
codex mcp add rustybin --url https://<host>/_rustybin/mcp --bearer-token-env-var RUSTYBIN_TOKEN
```

That writes `~/.codex/config.toml`; a project's `.codex/config.toml` is read only
in a project you trusted:

```toml
[mcp_servers.rustybin]
url = "https://<host>/_rustybin/mcp"
bearer_token_env_var = "RUSTYBIN_TOKEN"
tool_timeout_sec = 120
```

Codex has no `${VAR}` expansion in this file; `bearer_token_env_var` (or
`env_http_headers` for another header, such as `X-Rustybin-Mock-Token`) is the way.
Raise `tool_timeout_sec` from its 60 seconds for `wait_for_requests` and the AI
tools.

**OAuth:** leave the token out, then `codex mcp login rustybin` (or `/mcp login
rustybin` in the session). Codex registers itself and waits on a loopback port,
which Rustybin accepts. **Check:** `codex mcp list`, or `/mcp` in the session.
**Filter:** `enabled_tools = [...]` and `disabled_tools = [...]` in the server's
table. **Prompts:** Codex does not offer MCP prompts; ask in plain words.
**stdio:** `codex mcp add rustybin -- rustybin mcp-stdio --profile <name>`.

### Cursor

`~/.cursor/mcp.json` for every project, `.cursor/mcp.json` for one (both are read;
the project's entry wins). Cursor expands `${env:NAME}` in `url` and `headers`:

```json
{
  "mcpServers": {
    "rustybin": {
      "url": "https://<host>/_rustybin/mcp?page_size=200",
      "headers": { "Authorization": "Bearer ${env:RUSTYBIN_TOKEN}" }
    }
  }
}
```

Set the variable in your shell profile and restart Cursor, so the editor sees it.

**OAuth:** leave `headers` out and authenticate the server under **Customize,
MCPs**; in the command line agent, `agent mcp login rustybin`. Cursor registers
`http://localhost:8787/callback` and `cursor://anysphere.cursor-mcp/oauth/callback`,
both accepted. **Check:** **Customize, MCPs** shows the server and its tools; the
Output panel's "MCP Logs" shows errors; `agent mcp list-tools rustybin` from the
command line. **Tools:** Cursor reads only the first page of `tools/list`, hence
`?page_size=200` on the URL: every tool on one page (see [Tool limits](#tool-limits));
a tool's name in the chat panel's tool list turns it off. **stdio:** `"command": "rustybin", "args":
["mcp-stdio", "--profile", "<name>"]`.

### OpenCode

`~/.config/opencode/opencode.json` for every project, `opencode.json` (or
`.opencode/opencode.json`) in a project. OpenCode expands `{env:NAME}`, never
`$NAME`. Remote servers try OAuth unless you turn it off:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "rustybin": {
        "type": "remote",
        "url": "https://<host>/_rustybin/mcp",
        "oauth": false,
        "headers": { "Authorization": "Bearer {env:RUSTYBIN_TOKEN}" }
      }
    }
  }
}
```

That is the shape of OpenCode 2; the 1.x shape, still read, puts `rustybin`
directly under `mcp` with `"enabled": true`.

**OAuth:** leave out `oauth: false` and the header, then `opencode mcp auth
rustybin` (or `/mcps` in the session). **Check:** `opencode mcp list` shows
`connected`. **Tools:** OpenCode reads every page; by default the agent reaches MCP
tools through Code Mode rather than the model's tool list, which suits a server this
size. **Prompts:** `/rustybin:build_instance`, with the arguments in order.
**stdio:** `"type": "local", "command": ["rustybin", "mcp-stdio", "--profile",
"<name>"]`.

### VS Code with Copilot

Copilot's agent reads the portable `mcpServers` format: `.mcp.json` at the
workspace root, or `~/.copilot/mcp-config.json` for every workspace. It expands
`${NAME}`:

```json
{
  "mcpServers": {
    "rustybin": {
      "type": "http",
      "url": "https://<host>/_rustybin/mcp",
      "headers": { "Authorization": "Bearer ${RUSTYBIN_TOKEN}" }
    }
  }
}
```

The older VS Code format, `.vscode/mcp.json` or the user profile's `mcp.json`
(**MCP: Open User Configuration**), has `servers` and can prompt for the token
once and keep it in VS Code's secret storage. The Copilot harness, now the default,
does not receive servers that use `${input:...}`, so prefer the variable or OAuth:

```json
{
  "inputs": [
    { "type": "promptString", "id": "rustybin-token", "description": "Rustybin token", "password": true }
  ],
  "servers": {
    "rustybin": {
      "type": "http",
      "url": "https://<host>/_rustybin/mcp",
      "headers": { "Authorization": "Bearer ${input:rustybin-token}" }
    }
  }
}
```

**OAuth:** leave the header out; VS Code opens the browser when it first connects.
It registers `http://127.0.0.1:33418/` and `https://vscode.dev/redirect`, both
accepted. **Check:** **MCP: List Servers**, then **Show Output**. **Tools:** a chat
request takes at most 128 tools from every server together; with other servers
connected, turn some off under **Configure Tools** (or **Chat: Open Customizations,
Tools**) or narrow the agent. **Prompts:** `/rustybin.build_instance` in the chat.
**stdio:** `"type": "stdio", "command": "rustybin", "args": ["mcp-stdio",
"--profile", "<name>"]`.

### Windsurf (Devin Desktop)

Windsurf is now Devin Desktop, and its agent reads the Devin configuration:
`~/.config/devin/mcp_config.json` for every project, `.devin/mcp_config.json` in a
project (committed) and `.devin/mcp_config.local.json` beside it (git-ignored, the
place for a personal token):

```json
{
  "mcpServers": {
    "rustybin": {
      "url": "https://<host>/_rustybin/mcp",
      "transport": "http",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

The same from its command line: `devin mcp add -s user rustybin
https://<host>/_rustybin/mcp -H "Authorization: Bearer $RUSTYBIN_TOKEN"`. Older
Windsurf releases read `~/.codeium/windsurf/mcp_config.json`, with `serverUrl`
instead of `url` and `${env:RUSTYBIN_TOKEN}` expanded in headers.

**OAuth:** leave the header out; the server shows **Needs authentication** with an
**Authenticate** button (or `devin mcp login rustybin`). **Check:** **Customize,
MCPs**, or `devin mcp list`. **Tools:** Cascade took at most 100 tools in total;
narrow the agent if tools are missing. **Prompts:** `/mcp__rustybin__build_instance`.
**stdio:** `"command": "rustybin", "args": ["mcp-stdio", "--profile", "<name>"]`.

### Gemini CLI

`~/.gemini/settings.json` for every project, `.gemini/settings.json` in a project.
`httpUrl` is streamable HTTP (`url` would be SSE), and `$NAME` or `${NAME}` is
expanded in any value:

```json
{
  "mcpServers": {
    "rustybin": {
      "httpUrl": "https://<host>/_rustybin/mcp?page_size=200",
      "headers": { "Authorization": "Bearer ${RUSTYBIN_TOKEN}" }
    }
  }
}
```

`gemini mcp add --transport http --scope user rustybin 'https://<host>/_rustybin/mcp?page_size=200'
--header "Authorization: Bearer $RUSTYBIN_TOKEN"` does the same, with the token
itself in the file.

**OAuth:** leave the header out; Gemini CLI starts the sign-in on the `401`, or run
`/mcp auth rustybin`. It needs a browser on the same machine. **Check:** `/mcp` in a
session, or `gemini mcp list`. **Tools:** Gemini CLI reads only the first page of
`tools/list`, hence `?page_size=200` on the URL (see [Tool limits](#tool-limits));
`includeTools` and `excludeTools` filter what it read. **Prompts:**
`/build_instance`, with `--brief="..."` or the arguments in order. **stdio:**
`"command": "rustybin", "args": ["mcp-stdio", "--profile", "<name>"]`.

### Zed

Zed connects to remote servers itself (**Settings, AI, MCP Servers, Add Server, Add
Remote Server**), in `context_servers` of your settings (`~/.config/zed/settings.json`)
or a project's `.zed/settings.json`. Zed does not expand environment variables
there, so a header holds the token itself: use OAuth instead, or keep the header in
your user settings, never in a project you commit.

```json
{
  "context_servers": {
    "rustybin": {
      "url": "https://<host>/_rustybin/mcp?page_size=200",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

**OAuth:** leave `headers` out and Zed asks you to authenticate. **Check:** the
server's dot turns green in **Settings, AI, MCP Servers**. **Tools:** Zed reads only
the first page of `tools/list`, hence `?page_size=200` on the URL (see [Tool
limits](#tool-limits)); an agent profile can turn single tools off. **stdio:**
`"command": "rustybin", "args": ["mcp-stdio", "--profile", "<name>"]`.

## Clients that only speak stdio

The `rustybin` binary has a bridge for clients without remote MCP support. It reads
one JSON-RPC message per line on stdin and posts it to the server:

```bash
# the profile of `rustybin login --url https://mocks.example.com`
rustybin mcp-stdio --profile mocks.example.com
# or the URL and a token
rustybin mcp-stdio --server https://mocks.example.com/_rustybin/mcp --token "$RUSTYBIN_TOKEN"
```

Configure it as the client's command, `rustybin` with the arguments `mcp-stdio
--profile <name>`. Profiles are named after the host (`localhost-8080` for
`http://localhost:8080`; `rustybin profiles list` shows them). The bridge handles one
request at a time; see [Connect an agent](agent-mcp.md#connect-an-agent) for what
that means for confirmations.

## Check that it worked

Ask the agent: "Call `get_instance` on rustybin and tell me what I may do." The first
call names the instance and the caller's rights:

```hurl
{{#include ../examples/agent/connect.hurl:connect_check}}
```

Then open the console's **AI agents** view: the client is listed by the name it
sends, with its credential, its calls and its ceiling. The same list from a script:

```hurl
{{#include ../examples/agent/connect.hurl:connect_seen}}
```

`rustybin agents list` shows it from the command line, and `rustybin agents tail`
follows its calls as they happen.

## Rights to give

An agent gets the rights of its credential, and you can give it less with a
**ceiling** (in the console's AI agents view, with `rustybin agents rights`, or a
rule of `rustybin.yaml`; see [Managing agents](agent-mcp.md#managing-agents)):

| Ceiling | For | Tools an admin credential sees |
|---|---|---|
| `read-only` | Exploring: what is mocked, what the deploy checks say, sample requests | 60 |
| `traffic` | Debugging a gateway: the above plus captured traffic | 66 |
| `manage` | Building mocks: reads plus creating, editing and deploying mocks, AI-built examples, the portal | 69 |
| `full` | Building the whole platform: settings, git, agents, saved tests, checkpoints, the harness | 125 |

The counts are for an admin credential on an instance without self-service mocks;
`tools/list` shows only what the agent may call. Start with `read-only` while you
try a client, and move to `full` (an admin credential) when the agent builds for
you. Destructive and outward-facing tools ask you before they act either way.

Rights follow the credential, not the client's name: two clients on the same token
share the tightest limits set on either. To give two agents different rights, give
them different credentials (OAuth sign-in does this for you).

A ceiling is also how you fit the server into a client that takes fewer tools (see
[Tool limits](#tool-limits)): `manage` and below fit in one `tools/list` page. Here
an agent goes from two pages to one:

```hurl
{{#include ../examples/agent/connect.hurl:connect_ceiling}}
```

For a tighter fit, an **allow** list names the tools the agent sees, and a **deny**
list removes some:

```bash
rustybin agents list
rustybin agents rights <id> --ceiling manage --deny delete_mock,apply_config
```

## Tool limits

With an admin credential the server has 132 tools, sent 100 to a `tools/list` page
with `nextCursor` for the rest. Clients differ in how many tools they take:

| Client | Reads every `tools/list` page | Its own limit |
|---|---|---|
| Claude Code | Not documented | None; tool search loads tools when needed |
| Codex CLI | Yes | None documented; `enabled_tools` / `disabled_tools` filter |
| Cursor | **No: the first 100 only** | No number documented |
| OpenCode | Yes | None documented; tools are reached through Code Mode by default |
| VS Code (Copilot) | Yes (the local harness) | 128 tools per chat request, every server together |
| Windsurf (Devin Desktop) | Not documented | Not documented (Cascade had 100 in total) |
| Gemini CLI | **No: the first 100 only** | None documented; `includeTools` / `excludeTools` filter |
| Zed | **No: the first 100 only** | None documented; agent profiles filter |

A client that reads only the first page misses the last tools of the list: for an
admin, everything from `end_agent_sessions` on: sign-in sessions, checkpoints,
saved tests, gateway scenarios, users and roles. Turning tools off in the client does not help
there: the client never saw the second page. **Add `?page_size=200` to the MCP URL**
(`https://<host>/_rustybin/mcp?page_size=200`, at most 1000): the server then sends
every tool on one page. OAuth sign-in works with that URL too (the query is not part
of the resource a token is for). The snippets above for Cursor, Gemini CLI and Zed
already carry it; it does no harm on the other clients.

To keep a client's list short instead (a smaller context, or a client with a
per-request limit), narrow the agent on the instance, then reconnect the client.
The `manage` ceiling (69 tools), for building mocks, fits in the default page of 100.
Full rights without the 19 harness tools no longer do (113 tools since 0.5.0), so a
client that reads one page and needs more than `manage` takes `?page_size=200`. To
drop the harness tools anyway, for a smaller context:

```bash
rustybin agents list    # the client's agent id, after it connected once
rustybin agents rights <id> --deny harness_status,validate_scenario,list_scenarios,get_scenario,create_run,create_cluster,list_cluster_nodes,get_leader,export_upstreams,start_run,list_patterns,preview_scenario,save_scenario,update_run,list_runs,get_run,stop_run,get_run_report,harness_stop
```

Give such a client its own credential (OAuth sign-in does), since rights follow the
credential: the same deny list would otherwise apply to every client on that token.
The [stdio bridge](#clients-that-only-speak-stdio) passes the pages through as they
are, so it does not change this.

A client that reads every page but takes fewer tools at once (VS Code's 128 per
request, with other servers connected) can turn tools off on its side instead.

## Build your test platform with your agent

The server ships [prompts](agent-mcp.md#prompts-and-resources) for the common jobs.
Clients that support MCP prompts offer them as commands (Claude Code:
`/mcp__rustybin__build_instance`; OpenCode: `/rustybin:build_instance`; VS Code:
`/rustybin.build_instance`; Gemini CLI: `/build_instance`); in the
others, ask in plain words and the agent picks the tools. A typical session, from
empty instance to a platform your team can use:

**1. Look around.** "What is on this Rustybin? Run the diagnostics and tell me
what to fix first." The agent calls `get_instance`, `diagnose_instance` and
`list_mocks`.

**2. Mock your APIs** (`mock_an_api`, `build_instance`). "Mock `openapi/orders.yaml`
as `orders`, check every operation and give me a curl for each." With no spec:
"Describe a parcels API with shipments and tracking events, show me the spec, then
create it." Spec generation and AI examples use [your own AI key](ai-assist.md),
which you set in the console or the CLI, never through the agent.
`build_instance` takes a whole brief: "an orders API and a parcels API for the
checkout team, published for the mobile developers".

**3. Secure them** (`secure_a_mock`). "Secure `orders` with JWT and tell me what
token my gateway must send. Issue me one with `orders:read` and try a call."

**4. Stub the AI providers.** "Which mock LLM paths does this instance serve for
Anthropic and OpenAI? Give me the base URLs to put in my gateway's AI routes." The
[mock LLM](ai/index.md) is always on; the agent finds the paths with
`find_endpoints` and checks them with `try_request`.

**5. Prove the gateway** (`debug_gateway_call`). "I'm sending a request through the
gateway now: tell me which headers arrived and whether the JWT was valid." Or a
guided [gateway scenario](gateway-scenarios.md): "List the gateway scenarios, start
the retry one against `https://gateway.local` and check it once I've configured the
route."

**6. Keep what passed as tests.** "Save the last three calls to `/mock/orders` as
[saved tests](saved-tests.md) with status and body assertions, then run them."
Before a risky change: "Save a [checkpoint](checkpoints.md) called before-v2."

**7. Load test** (`load_test_gateway`, on a [harness](harness.md) node). "Ramp to
200 requests per second against `https://gateway.local/orders` for two minutes,
dry run at 5 first, and summarise the latency and the 429s."

**8. Publish for your team** (`publish_portal`). "Publish `orders` and `parcels` to
the [dev portal](portal.md) with a summary each, turn the portal on and show me what
visitors see."

**9. Back it up** (`backup_to_git`). "Export the instance as `rustybin.yaml` and push
it to `git@github.com:team/mocks.git`." The git token or deploy key is set by you;
the push asks you first.

Other prompts: `test_mcp_server` runs the [inspector's checks](mcp/inspector.md)
against any MCP server, and `onboard_agent` proposes rights for another agent. The
walkthrough on the management MCP page shows [every call
`build_instance` makes](agent-mcp.md#build-an-instance-with-an-agent).

## Troubleshooting

**The client says it failed to connect, or `401`.** The credential is missing,
expired or revoked; the JSON body's `reason` says which (`missing_token`, `expired`,
`revoked`, `invalid_token`). A sign-in token lasts 12 hours: sign in again, or
switch to OAuth or the admin token. Check that the environment variable is set in
the shell (or the app) that starts the client, and that the URL ends in
`/_rustybin/mcp`, not `/mcp`. A client with a header set may not fall back to
OAuth after a `401` (Claude Code does not): remove the header to sign in through the
browser.

**A tool is missing, or a call says "the operator limited this agent".** The
agent's ceiling, allow list or deny list leaves it out (see the agent in **AI
agents**, or `rustybin agents get <id>`). Remember that rights follow the
credential: a limit set on another client with the same token applies here too. A
tool error saying the credential lacks a right (for example "this tool changes the
whole instance") means the token itself is not admin: a `console` scope JWT, an
OAuth grant of read only, or no admin token on an open control plane.

**Every call says the agent was revoked, or answers `403`.** The agent was
revoked, or its session was ended, in the AI agents view. **Restore** it there (or `rustybin agents restore
<id>`), then reconnect the client.

**Tools are missing that the agent's rights allow.** The client took fewer tools
than the server offers (see [Tool limits](#tool-limits)), or it read only the first
page of `tools/list`. Add `?page_size=200` to the MCP URL, narrow the agent on the
instance (a ceiling of `manage` or below fits in one page), or turn tools off in the
client. After a rights change,
reconnect the client so it lists the tools again.

**OAuth opens the wrong page, or the browser shows an error.** The sign-in is
offered only when the control plane asks for credentials (`password`, the default,
`token` or `jwt` control auth; not in shared mode or with
`RUSTYBIN_CONTROL_OAUTH=off`); on an open control plane there is nothing to sign in
to. The browser should open the console's consent page, `/ui/#/authorize/<id>`:
sign in to the console there, check the agent's name and where it will be sent back
to, and approve. Redirect URIs must be `https`, `http` on a loopback address (any
port) or an app's own scheme; the registration error names the rule. A
client that does not read the `resource_metadata` pointer of the `401` finds the
wrong metadata: update it, or use a header. On an instance whose OAuth key is
random per process (no `RUSTYBIN_CONTROL_OAUTH_KEY`, no state directory), a restart
signs agents out: sign in again. Details: [Sign in with
OAuth](agent-mcp.md#sign-in-with-oauth).

**A destructive tool fails after a wait.** The client declared elicitation but did
not show the question (some bridges and older clients). Answer the agent "yes, go
ahead" so it passes `confirm: true`; see
[Confirmations](agent-mcp.md#confirmations).

For the instance itself (it does not answer, it lost its changes after a restart),
see [Troubleshooting](troubleshooting.md); `diagnose_instance` (or `rustybin
doctor`) lists what is wrong and the fix.

## On every surface

| Surface | Where |
|---|---|
| API | `POST /_rustybin/mcp` (the server), `POST /_rustybin/login` (a token), `GET /_rustybin/agents` (who connected) |
| Console | **AI agents**: **Connect an agent** (the command and JSON with this instance's URL and credential), the agents, their rights and calls |
| CLI | `rustybin login`, `rustybin mcp-stdio`, `rustybin agents list \| rights \| tail` ([reference](cli.md#agents)) |
| Management MCP | Every tool; the prompts `build_instance`, `mock_an_api`, `secure_a_mock`, `debug_gateway_call`, `load_test_gateway`, `publish_portal`, `backup_to_git`, `test_mcp_server`, `onboard_agent` |
| `rustybin.yaml` | `agents:` rules by client name and credential kind ([Managing agents](agent-mcp.md#managing-agents)) |
