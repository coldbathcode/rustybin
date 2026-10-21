# Plan: OAuth sign-in to the management MCP server

Status: **building.** Planned 2026-10-20. Keep this status line and section 9 ("As built") current.

## 1. Goal

An agent connects to `/_rustybin/mcp` without the user pasting a token:

```bash
claude mcp add --transport http rustybin https://<host>/_rustybin/mcp
```

The first call answers `401` with an RFC 9728 `resource_metadata` pointer; the client discovers the
authorization server, registers itself, opens the browser on a consent page in the Rustybin console, and
gets a short-lived token scoped to what the person who approved may do. The bearer header keeps working.

Decisions (2026-10-20):

- **Built-in authorization server**, with `RUSTYBIN_CONTROL_OAUTH_ISSUER` to point the metadata at an
  external issuer (the platform, later) instead.
- **Self-service visitors keep the header** (`X-Rustybin-Mock-Token`): the consent page is for people who
  already hold control-plane credentials.

## 2. When it is on

Only where the control plane asks for credentials: `RUSTYBIN_CONTROL_AUTH=token` or `jwt`, not in shared
mode (keys authenticate there), and not with `RUSTYBIN_CONTROL_OAUTH=off`. On an open control plane no
`401` ever starts the flow, so nothing is published.

## 3. Endpoints

| Route | Auth | |
|---|---|---|
| `GET /.well-known/oauth-protected-resource/_rustybin/mcp` | none | RFC 9728 metadata: `resource` (this instance's `/_rustybin/mcp` URL), `authorization_servers` (`<base>/_rustybin/oauth`, or the external issuer), scopes `admin`, `console`, `inspector` |
| `GET /.well-known/oauth-authorization-server/_rustybin/oauth` and `/.well-known/openid-configuration/_rustybin/oauth` | none | RFC 8414 metadata of the built-in server (path-inserted, as the MCP spec has clients look it up) |
| `POST /_rustybin/oauth/register` | none | RFC 7591 dynamic client registration; public clients only (`none`) |
| `GET /_rustybin/oauth/authorize` | none | Checks the request and redirects the browser to the console's consent page `/ui/#/authorize/<id>` |
| `GET /_rustybin/oauth/requests/{id}` | control | The pending request for the consent page: client name, redirect URI, scopes requested and grantable |
| `POST /_rustybin/oauth/requests/{id}` | control | `{"decision": "approve" \| "deny", "scope": ...}`: answers `{"redirect_to": ...}` with a code or `access_denied` |
| `POST /_rustybin/oauth/token` | none | `authorization_code` (PKCE S256 required) and `refresh_token` |

`register`, `authorize` and `token` are exempt from control auth; the two `requests` routes are protected
like the rest of the control plane, so the console approves with the credential it already holds (admin
token or control JWT). Any scope may approve; the grant is capped by the approver's own grant.

The `401` from `/_rustybin/mcp` gains `resource_metadata="<url>"` in its `WWW-Authenticate` challenge
(with an external issuer too).

## 4. Clients, codes and tokens

- **Client ids are stateless**: the registration (redirect URIs, name) signed with the instance key, so
  nothing is stored, nothing grows, and registrations survive restarts when the key does. At most 3
  redirect URIs of 512 characters, a name of 80. Redirect URIs: `https`, `http` on a loopback host (any
  port at authorization time, RFC 8252 7.3), or a private-use scheme (`cursor://...`); never
  `javascript`, `data`, `file` and the like.
- **Pending requests** (10 minutes) and **authorization codes** (2 minutes, single use) live in bounded
  stores (1,000 each; 200 in public mode).
- **Tokens** are HS256 JWTs signed with the instance key: access tokens for 1 hour with
  `aud` = the `/_rustybin/mcp` URL (RFC 8707), refresh tokens for 30 days. Refresh issues a new pair; tokens
  cannot be revoked one by one (rotating the key revokes all).
- **The key**: `RUSTYBIN_CONTROL_OAUTH_KEY` (a secret of 32 characters or more), else derived from
  `RUSTYBIN_ADMIN_TOKEN` (rotating the admin token signs everyone out), else random per process (tokens
  end with a restart; several replicas need the setting).
- **Where tokens work**: `/_rustybin/mcp` only. Every other control-plane route, and the data plane's
  admin guard, refuse them. A token with the `admin` scope is admin on the management server, exactly like
  the admin token there.

## 5. Console

A consent view at `/ui/#/authorize/<id>` (not in the navigation): the sign-in screen first when the
console holds no token, then the client's name, where the browser will be sent (with a warning when it
is not a loopback address), a choice between full access, read only and traffic only (limited to what
the console's own credential allows), Approve and Deny.

## 6. External issuer

`RUSTYBIN_CONTROL_OAUTH_ISSUER=https://...`: the metadata names that issuer, the built-in endpoints answer
`404`, and the tokens accepted are the control JWTs accepted today (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`,
`aud` = `RUSTYBIN_CONTROL_JWT_AUDIENCE`). Useful once the platform runs an authorization server.

## 7. Phases

| Phase | Scope | Done when |
|---|---|---|
| 1. Server | Config, metadata, registration, authorize, requests, token, the `/_rustybin/mcp` challenge and token check, audit lines, catalogue and OpenAPI | Unit tests through the full app: the whole flow, PKCE, redirect URI rules, scope capping, token refused off `/_rustybin/mcp`, external issuer, off modes |
| 2. Console | The consent view | Browser check: sign in, approve, deny, scope choice (`conformance/ui/`) |
| 3. Conformance and docs | The Python MCP SDK's OAuth client against `/_rustybin/mcp`; docs page section, Hurl examples, README, changelog, roadmap | `conformance/mcp/run.sh`, `docs/examples/run.sh`, `check_includes.py` pass |

## 8. Not in this version

- Client ID metadata documents (the server would fetch a URL: never, as elsewhere in Rustybin).
- Revoking single tokens, a list of signed-in agents, token introspection.
- Sign-in for self-service visitors and shared-mode key holders.

## 9. As built

Nothing built yet.
