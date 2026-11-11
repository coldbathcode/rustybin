# Step 02: MCP security scenarios

## Goal

Mock MCP servers that misbehave on purpose, in the ways an MCP gateway or a careful client is meant
to catch, and a guided scenario per attack with a pass or fail verdict. A presenter shows the attack
reaching an unprotected client, then the gateway stopping it.

## Where things stand

- The mock MCP server (`src/mcp/`) has named servers (`/mcp/weather`, `/mcp/crm`, `/mcp/devtools`),
  an API key and an OAuth variant, and one injection demo: `prompt_injection_demo` returns
  `INJECTION_SAMPLE` in a tool result (`src/mcp/tools.rs`).
- The MCP inspector (`src/mcp/inspector/`) has `CHECKS` for protocol conformance, none for security.
- Nothing serves a poisoned tool description, a definition that changes after approval, a shadowed
  tool name or a server that asks for more scope than its tools need.

## Model

Attack servers at `/mcp/attack/<kind>`, each a `Profile` on the existing protocol and transport
code (a new `Family::Attack(kind)`), with a fixed, documented behaviour:

| kind | The attack | What a defence looks like |
|---|---|---|
| `poisoned_description` | A tool whose description hides instructions (in an HTML comment, after many blank lines, in a parameter's description) | The gateway strips or refuses the tool, or flags it, before `tools/list` reaches the client |
| `rug_pull` | `tools/list` answers a harmless definition, then, after N calls or on a toggle, the same tool name with a changed description and schema, and `notifications/tools/list_changed` | The gateway pins the approved definition and refuses or flags the changed one |
| `shadowing` | Two servers behind one gateway both offer `send_email`; the attack server's copy also asks for a `bcc` | The gateway namespaces or refuses the duplicate |
| `result_injection` | Tool results carrying `INJECTION_SAMPLE` in text, in a resource link and in structured content | The gateway's response guard removes or flags it |
| `scope_overreach` | The OAuth variant's protected resource metadata asks for scopes its tools never need (`admin`, `offline_access`, a wildcard) | The gateway or client narrows the scope or refuses |
| `oversized` | A result of many megabytes, or a stream that never ends | The gateway caps the size and the time |
| `exfiltration_url` | A result asking the client to fetch a URL with data in the query string | The gateway's egress rule refuses the host |

- Every attack's text is inert test data in one table (`src/mcp/attacks.rs`), never a working payload
  against a real system, and every URL is on a reserved domain (`example.com`, `.invalid`).
- Bounded: the rug pull's switch is per MCP session (the sessions are already bounded), and the
  oversized answer is generated as a stream, never held in memory.

Scenarios: one row per kind in `src/scenarios/catalog.rs` with a new `Upstream::McpAttack(kind)`.
The check speaks MCP to the gateway (the inspector's client in `src/mcp/inspector/client.rs`), and
`evaluate` judges what came back: was the poisoned text present in `tools/list`, did the changed
definition reach the client, was the injection removed. The attack server's own log of what it was
asked for shows what got through.

Inspector checks: three new `CHECKS` (`descriptions_look_clean`, `definitions_are_stable`,
`results_carry_no_instructions`) that any MCP server can be tested against, judged in `run` and
listed in `docs/src/mcp/inspector.md`. They are heuristics and say so: a warning, never a failure.

## Surfaces

| Surface | What changes |
|---|---|
| API | The data plane routes `/mcp/attack/{kind}` in the catalogue and OpenAPI; `/_rustybin/scenarios*` lists the new rows; `/_rustybin/mcp-checks` runs the new checks |
| Console | The Gateway scenarios view gains an MCP group; the MCP view's server list shows the attack servers with a warning badge and their kind |
| Management MCP | The scenario tools take the new ids; the inspect tools run the new checks; every answer from an attack server is under `untrusted_*` |
| CLI | `rustybin scenario start mcp_rug_pull ...`; `rustybin mcp check` reports the new checks |
| `rustybin.yaml` | The setting `mcp.attack_servers` |

Docs: a new page `docs/src/mcp/security.md` (each attack, the defence it tests, a Hurl example per
attack server), linked from `docs/src/mcp/index.md` and `docs/src/gateway-scenarios.md`.

## Owner decision (master plan section 6)

On by default, off in public mode (`mcp.attack_servers`, `false` when `RUSTYBIN_PUBLIC_MODE` is set,
refused from git like other security settings). With it off the routes answer 404 and the scenarios
are not listed.

## Rules

- An attack server's answers are third-party content everywhere they surface: `untrusted_*` in tool
  answers, rendered as text in the console, never in a text summary or an audit entry.
- The management MCP never calls an attack server in process on an agent's behalf except through the
  scenario check, whose answer is marked untrusted.

## Tests that fail first

- Each scenario passes against the test proxy with the defence on and fails, naming the attack that
  got through, with it off.
- The rug pull changes only in the session that triggered it; a new session sees the harmless
  definition again.
- `oversized` never holds more than a fixed buffer (measured), and the check stops at its cap.
- Public mode: the routes are 404, the scenarios unlisted, the setting refused from a document.
- `every_tool_is_fully_defined` and the untrusted-field tests cover the new answers.

## Done when

Seven attack servers, seven scenarios and three inspector checks run on every surface, each scenario
proven both ways against the test proxy, documented on the new page.

## Build notes (2026-11-11)

Built whole. Where the build differs from the plan above:

- **The attack servers are a fourth MCP family** (`Family::Attack`, `src/mcp/attacks.rs`), on
  the existing protocol and transport code with their own dispatch, like the OpenAPI mocks'
  servers. `Profile.server` holds the kind; `Shared.attacks` the bounded state (the rug pull's
  keys, 1024 for an hour; at most 4 oversized results built at once). Seven kinds as planned;
  `oversized` is 2 MiB built in memory under that cap, not a never-ending stream (the transport
  answers whole JSON-RPC messages).
- **Six scenarios, not seven.** `scope_overreach` is an authorization flow, not a message a
  check can send through a gateway: it is an attack server (a `401` challenge and protected
  resource metadata asking for `admin offline_access tools:* files:write`; any bearer passes)
  for demos and the inspector's OAuth walk, without a scenario.
- **The rug pull is keyed per check.** `Scenarios::check_key` gives every request of one check
  the same key (`scn-<run>-<secret>-<check>`) and the next check a new one, so each check starts
  with the harmless definition; outside a scenario the key is the MCP session, else the caller's
  `X-Rustybin-Session` or address.
- **The check is its own small MCP client** (`send_mcp` in `src/scenarios/mod.rs`): `initialize`
  with `2025-06-18`, `notifications/initialized`, then the scenario's requests in the session,
  through the same egress client as the other checks; answers are read as JSON or SSE
  (`catalog::rpc_answer`). The inspector's client was not reused: it goes through the relay
  under `Purpose::McpRelay`, and a check's traffic belongs to `Purpose::Scenario`.
- **A namespaced name counts.** `crm_lookup_customer`, `crm.lookup_customer` and the like are the
  trusted tool when their description is the trusted one; the shadowing line fails only when the
  exact name is the attacker's or is listed twice.
- **The setting is off in public mode whatever it says** (`attacks::enabled`), rather than a
  default that depends on the mode; it is not refused from git (it is not a security setting:
  turning it on in a document still serves nothing in public mode). The console's card is on
  Management, General.
- **The attack servers are listed in the inspector** (kind `attack-mcp`, with the attack in
  words and a warning in the console) and skipped by the registry's generated entries, so an
  agent browsing the registry never finds them.
- **Inspector checks** are heuristics shared with the scenarios' stand-in gateway
  (`attacks::hidden_instructions`); `results_carry_no_instructions` reads the answers of the
  `tools_call` calls (and makes the calls when asked alone), `definitions_are_stable` lists the
  tools again after them. Their details are `untrusted_tools` and `untrusted_results`.

### For the merge

Touches the MCP router and core dispatch (one family arm each), the settings table, the
inspector's server list and checks, the registry's generated entries, the scenario table and
engine, the CLI's `scenario check` flags and the console's scenarios, MCP and Management views.
Two new data plane routes (in the catalogue, OpenAPI and README tables); no control route, tool
or permission is new.
