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
18402 (gRPC) with `RUSTYBIN_ADMIN_TOKEN=conformance-admin`, and a second instance on
18403 to 18405 with `RUSTYBIN_CONTROL_AUTH=token` and the same admin token (for
OAuth sign-in to the management server), runs every script and stops both. Against
an already running server: `MCP_BASE=http://host:port python test_streamable_modern.py`
(`test_manage.py` and `test_manage_oauth.py` also read `MANAGE_ADMIN_TOKEN`, default
`conformance-admin`; `test_manage_oauth.py` reads `MANAGE_OAUTH_BASE`, default
`http://127.0.0.1:18403`, an instance with `token` or `jwt` control auth).

## Scripts

| Script | Covers |
|---|---|
| `test_streamable_modern.py` | 2026-07-28: `server/discover` probe, lists with `ttlMs`/`cacheScope`, every tool (Mcp-Name / Mcp-Param-* headers), progress over SSE, `_meta` logLevel logs, elicitation and sampling via multi round-trip (`input_required`), resources incl. blob and templates, prompts, completion, `subscriptions/listen`, `?page_size=` pagination |
| `test_streamable_legacy.py` | 2025-11-25, 2025-06-18, 2025-03-26: initialize + Mcp-Session-Id, ping, progress, `logging/setLevel`, server-to-client `elicitation/create` and `sampling/createMessage`, version-specific content (structuredContent, resource_link), `resources/subscribe` + GET stream updates |
| `test_sse_legacy.py` | HTTP+SSE transport (`/mcp/sse` + `/mcp/messages`) at 2024-11-05 and 2025-11-25 |
| `test_variants.py` | `/mcp/protected` (401 challenge, RFC 9728 metadata, IdP token, 403 insufficient_scope step-up), `/mcp/apikey`, `/mcp/servers/{weather,crm,devtools}` |
| `test_manage.py` | `/_rustybin/mcp`: without a token only read tools are listed and `create_mock` is refused; with `Authorization: Bearer <admin token>` discovery, role-filtered `tools/list` with destructive annotations, the three prompts, both resources, a create, deploy and `try_request` round trip (mocked body only under `untrusted_*`, control plane refused), `delete_mock` refused without confirmation then deleted with `confirm: true`, declined and accepted elicitation on 2026-07-28 (`input_required`) and on a 2025-11-25 session (`elicitation/create`) |
| `test_manage_oauth.py` | OAuth sign-in to `/_rustybin/mcp` with the SDK's `OAuthClientProvider`: the 401 challenge with `resource_metadata`, protected resource and authorization server metadata, dynamic registration, the authorization URL followed to the console's consent page and approved through the consent API with the admin token (playing the user), a `console` sign-in (read tools only, refresh of an expired access token) and an `admin` sign-in (mock tools listed, an admin tool succeeds, the token refused on `/_rustybin/config`) |
| `inspector_cli.sh` | Inspector CLI: tools/list, tools/call, resources/read, prompts/get, named server, API key header, legacy SSE |

Not covered: the SDK's interactive OAuth authorization-code flow against `/mcp/protected`
(the scripts fetch a client_credentials token from the built-in IdP instead).
