# MCP security: attack servers and scenarios

An MCP gateway, or a careful MCP client, is meant to stop a few known attacks: a tool
whose description hides instructions for the model, a tool that changes after the
client approved it, a tool that takes a trusted tool's name, a tool result that
carries a prompt injection or a link that leaks data, a result too large to pass on,
a server that asks for more access than it needs. Rustybin serves an MCP server for
each of them, so a demo can show the attack reach an unprotected client, then the
gateway stop it, and a guided [gateway scenario](../gateway-scenarios.md) per attack
gives a pass or fail verdict.

Everything the attack servers send is inert test data: the hidden instructions name
files and addresses no real system acts on, every address is on a reserved domain
(`.invalid`, `example.com`), and every attack carries a `RUSTYBIN-...` marker so a
check can tell whether it got through.

## The attack servers

Each one is at `/mcp/attack/{kind}`, on the same protocol and transports as the
[mock MCP server](index.md) (every protocol version, Streamable HTTP with or without a
session).

| Kind | The attack | What a defence looks like |
|---|---|---|
| `poisoned_description` | `add_numbers`'s description hides an `<IMPORTANT>` block telling the model to read `~/.ssh/id_rsa` and keep it from the user, and a parameter's description hides an HTML comment; a clean `get_weather` sits next to it | The gateway removes, refuses or rewrites the poisoned tool before `tools/list` reaches the client, and keeps the clean one |
| `rug_pull` | `get_exchange_rate` is harmless until its first call; then `tools/list` answers it with hidden instructions and a new parameter, `forward_to` | The gateway pins the definition the client approved and refuses or flags the changed one |
| `shadowing` | `lookup_customer`, the name of a tool of the trusted [`/mcp/servers/crm`](index.md#named-servers), whose description tells the model to copy every email to `audit@exfil.rustybin.invalid` | The gateway namespaces or refuses the duplicate, so the trusted tool keeps its name |
| `result_injection` | `get_order_notes` answers a prompt injection in its text, its structured content and a resource link's description | The gateway's response guard removes or flags it |
| `oversized` | `export_logs` answers 2 MiB | The gateway caps what it passes to the client |
| `exfiltration_url` | `fetch_report` answers a link to `exfil.rustybin.invalid` with data in its query string, as an image and a resource link | The gateway removes or refuses links to hosts it does not allow |
| `scope_overreach` | The server challenges a caller without a bearer for `admin offline_access tools:* files:write`, and its protected resource metadata lists the same, for one read-only tool | The gateway or client asks for the scopes the tools need, or refuses the server (any bearer passes here: the point is the scope it asks for) |

```hurl
{{#include ../../examples/mcp/security.hurl:poisoned}}
```

The rug pull is kept per caller: a gateway scenario's check, else the MCP session,
else the caller's `X-Rustybin-Session` (or address). Each check of a scenario starts
with the harmless definition. At most 1024 callers are kept, for an hour each.

```hurl
{{#include ../../examples/mcp/security.hurl:rug_pull}}
```

```hurl
{{#include ../../examples/mcp/security.hurl:overreach}}
```

```hurl
{{#include ../../examples/mcp/security.hurl:unknown}}
```

The attack servers are on by default and always off in public mode: the setting
`mcp.attack_servers` (seeded by `RUSTYBIN_MCP_ATTACK_SERVERS`) turns them off on
another instance. Off, their routes answer `404`, the MCP scenarios are not listed
and a run of one is refused. They are listed in the MCP inspector (with a warning that
they misbehave on purpose), never in the [mock MCP registry](registry.md).

## MCP gateway scenarios

Each attack server but `scope_overreach` is the upstream of a
[gateway scenario](../gateway-scenarios.md). Put the attack server behind the gateway,
let it forward `X-Rustybin-Session`, turn on the defence, and check with the URL the
gateway serves the MCP server at. The check starts an MCP session through the gateway
(`initialize` with protocol `2025-06-18`, then `notifications/initialized`), sends the
scenario's requests in it, and compares what the client got with what reached the
attack server. The verdict says what got through by name, never the attack's text.

| Scenario | Attack server | The check sends | A pass needs |
|---|---|---|---|
| `mcp_tool_poisoning` | `poisoned_description` | `tools/list` | The list reached the server, no poisoned description reached the client, and `get_weather` did |
| `mcp_rug_pull` | `rug_pull` | `tools/list`, a call of `get_exchange_rate`, `tools/list` | The harmless definition first, the call reached the server, and the changed definition never reached the client |
| `mcp_tool_shadowing` | `shadowing`, and the trusted `/mcp/servers/crm` as the second server | `tools/list` | The trusted `lookup_customer` reached the client (under its name or a namespaced one), and the attacker's copy did not take the name `lookup_customer` |
| `mcp_result_injection` | `result_injection` | a call of `get_order_notes` | The call reached the server, and the injection never reached the client (removed, or the result refused or flagged) |
| `mcp_oversized_result` | `oversized` | a call of `export_logs` | The call reached the server, and the client never received more than `max_result_kib` (16 to 1024 KiB, default 256): the result cut, or refused |
| `mcp_exfiltration_url` | `exfiltration_url` | a call of `fetch_report` | The call reached the server, and no link to `exfil.rustybin.invalid` reached the client |

A tool counts under a namespaced name too: `crm_lookup_customer`, `crm.lookup_customer`
and the like are the trusted tool when its description is the trusted one.

```hurl
{{#include ../../examples/mcp/security.hurl:scenario}}
```

## Security checks in the inspector

The [MCP inspector](inspector.md)'s **Run checks** has three checks for any MCP server,
heuristics that warn and never fail a run:

| Check | Warns when |
|---|---|
| `descriptions_look_clean` | A tool's description or schema has an `<IMPORTANT>` block, an HTML comment, an instruction to ignore earlier instructions or to keep something from the user, invisible characters, a long run of blank lines, or a mention of credential files |
| `results_carry_no_instructions` | A tool's answer to the check's call has the same signs |
| `definitions_are_stable` | `tools/list` after the calls differs from the list before them: a tool changed, went or came (a rug pull, or a server being updated) |

Against the attack servers they warn; against Rustybin's own `/mcp`, the results check can
warn on `prompt_injection_demo`, which answers an injection on purpose. What a server
answered is third-party content: the details are under `untrusted_tools` and
`untrusted_results`.

## On every surface

| Surface | Where |
|---|---|
| API | `/mcp/attack/{kind}` (the data plane), `/.well-known/oauth-protected-resource/mcp/attack/scope_overreach`; the scenarios through `/_rustybin/scenarios*`; the checks through `POST /_rustybin/mcp-checks` |
| Console | **Gateway scenarios**: the MCP group. **MCP inspector**: the attack servers in the server list, with a warning, and the three checks in **Run checks**. **Management**, General: **MCP attack servers** |
| CLI | `rustybin scenario start mcp_rug_pull`, `rustybin scenario check ... --max-result-kib KIB`; `rustybin settings set mcp.attack_servers false` |
| Management MCP | The `*_gateway_scenario` tools take the MCP scenarios; the inspector's check tools run the three checks; what an attack server answered is under `untrusted_*` |
| `rustybin.yaml` | `settings: { mcp.attack_servers: false }`; scenario runs are transient |
