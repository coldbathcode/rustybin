# MCP conformance

Drives the mock MCP server (`/mcp`) and the management MCP server
(`/_rustybin/mcp`) with real clients:

- the official Python MCP SDK (`mcp` 2.3, Streamable HTTP and SSE clients)
- optionally the MCP Inspector CLI (`npx @modelcontextprotocol/inspector --cli`, TypeScript SDK)

## Run

```bash
# Python 3.11+ venv with: pip install "mcp>=2.3" httpx2
PYTHON=/path/to/venv/bin/python conformance/mcp/run.sh              # SDK scripts
PYTHON=/path/to/venv/bin/python conformance/mcp/run.sh --inspector  # plus Inspector CLI (needs npm registry access)
```

`run.sh` builds the binary, starts it on ports 18400 (HTTP), 18401 (HTTPS) and
18402 (gRPC) with `RUSTYBIN_ADMIN_TOKEN=conformance-admin`, runs every script and
stops the server. Against an already running server:
`MCP_BASE=http://host:port python test_streamable_modern.py` (`test_manage.py` also reads `MANAGE_ADMIN_TOKEN`, default `conformance-admin`).

## Scripts

| Script | Covers |
|---|---|
| `test_streamable_modern.py` | 2026-07-28: `server/discover` probe, lists with `ttlMs`/`cacheScope`, every tool (Mcp-Name / Mcp-Param-* headers), progress over SSE, `_meta` logLevel logs, elicitation and sampling via multi round-trip (`input_required`), resources incl. blob and templates, prompts, completion, `subscriptions/listen`, `?page_size=` pagination |
| `test_streamable_legacy.py` | 2025-11-25, 2025-06-18, 2025-03-26: initialize + Mcp-Session-Id, ping, progress, `logging/setLevel`, server-to-client `elicitation/create` and `sampling/createMessage`, version-specific content (structuredContent, resource_link), `resources/subscribe` + GET stream updates |
| `test_sse_legacy.py` | HTTP+SSE transport (`/mcp/sse` + `/mcp/messages`) at 2024-11-05 and 2025-11-25 |
| `test_variants.py` | `/mcp/protected` (401 challenge, RFC 9728 metadata, IdP token, 403 insufficient_scope step-up), `/mcp/apikey`, `/mcp/servers/{weather,crm,devtools}` |
| `test_manage.py` | `/_rustybin/mcp`: without a token only read tools are listed and `create_mock` is refused; with `Authorization: Bearer <admin token>` discovery, role-filtered `tools/list` with destructive annotations, the three prompts, both resources, a create, deploy and `try_request` round trip (mocked body only under `untrusted_*`, control plane refused), `delete_mock` refused without confirmation then deleted with `confirm: true`, declined and accepted elicitation on 2026-07-28 (`input_required`) and on a 2025-11-25 session (`elicitation/create`) |
| `inspector_cli.sh` | Inspector CLI: tools/list, tools/call, resources/read, prompts/get, named server, API key header, legacy SSE |

Not covered: the SDK's interactive OAuth authorization-code flow against `/mcp/protected`
(the scripts fetch a client_credentials token from the built-in IdP instead).
