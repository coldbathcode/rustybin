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
