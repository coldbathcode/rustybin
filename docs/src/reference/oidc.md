# OAuth 2.0 / OIDC provider

Rustybin includes a small but standards-shaped identity provider, so a gateway's
OpenID Connect, OAuth introspection or JWT plugin, an MCP client and the A2A
`secure` agent all have a real issuer to talk to without any external setup.

## Endpoints

| Route | Standard |
|---|---|
| `GET /.well-known/openid-configuration` | OpenID Connect Discovery |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 authorization server metadata |
| `GET /oauth/jwks` | The RS256 public key (JWK Set, `kid` `rustybin-rs256-key`) |
| `GET, POST /oauth/authorize` | Authorization endpoint with a demo login page |
| `POST /oauth/token` | Token endpoint |
| `GET, POST /oauth/userinfo` | OIDC UserInfo |
| `POST /oauth/introspect` | RFC 7662 token introspection |
| `POST /oauth/revoke` | RFC 7009 token revocation |
| `POST /oauth/register` | RFC 7591 dynamic client registration |

The **issuer** is derived from each request: `http://<Host>`, or `https://` on the
HTTPS listener. With `RUSTYBIN_TRUST_FORWARD=true`, `X-Forwarded-Proto`,
`X-Forwarded-Host` and `X-Forwarded-Prefix` are honoured, so the issuer matches the
public URL when Rustybin sits behind a gateway (route the gateway's `/.well-known`
and `/oauth` paths to it and forward those headers).

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:discovery}}
```

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:jwks}}
```

## Demo clients and users

| Client | Secret | Type | Grants |
|---|---|---|---|
| `rustybin` | `secret` | confidential (`client_secret_basic` or `client_secret_post`) | all five |
| `rustybin-public` | none | public, PKCE required | `authorization_code`, `refresh_token` |

Both demo clients accept any absolute `http(s)` redirect URI. Dynamically
registered clients must use one of their registered redirect URIs exactly.

| User | Password | Name | Email | Groups |
|---|---|---|---|---|
| `demo` | `demo` | Demo User | demo@rustybin.local | users |
| `alice` | `alice` | Alice Admin | alice@rustybin.local | users, admins |
| `bob` | `bob` | Bob Builder | bob@rustybin.local | users |

## Tokens

Access tokens are RS256 JWTs (`typ: at+jwt`) valid for one hour with `iss`, `sub`,
`aud`, `exp`, `iat`, `jti`, `client_id`, `scope` and, for user grants, `name`,
`email`, `preferred_username`, `groups` and `auth_time`. The audience is
`rustybin` unless the request names the target with `resource` (RFC 8707, up to
five absolute URIs). ID tokens (for user grants with the `openid` scope) are
addressed to the client and carry `nonce` when one was sent. Refresh tokens are
opaque (`rt_...`), valid 24 hours and **rotate**: each use returns a new one and
invalidates the old.

The signing key is generated at startup: tokens do not survive a restart. Scopes
are free-form (for example `orders:read` or `mcp:tools`); the defaults are `openid`
for `client_credentials` and `openid profile email` for user grants.

Token responses are sent with `Cache-Control: no-store`. Errors follow RFC 6749
(`{"error": "invalid_grant", "error_description": "..."}`), `401` with
`WWW-Authenticate` for client authentication failures.

## Grants

### Client credentials

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:client_credentials}}
```

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:bad_credentials}}
```

### Password

Handy for scripted demos (and still common in legacy gateways). It returns an ID
token and a refresh token.

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:password}}
```

### Authorization code with PKCE

`/oauth/authorize` validates the request and shows a login form (prefilled with
`login_hint`, else `demo`). Submitting it redirects (`303`) to the redirect URI with
`code` and `state`; "Deny" returns `error=access_denied`. Codes are single use and
expire after five minutes. PKCE supports `S256` and `plain`; it is mandatory for
public clients. Errors that can be sent back safely go to the redirect URI as
`error` and `error_description`; an unknown client or redirect URI gets an error
page instead.

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:authorize_page}}
```

The browser step, done with a form post:

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:login}}
```

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:exchange_code}}
```

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:code_single_use}}
```

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:pkce_required}}
```

### Refresh

A refresh may narrow the scope (never widen it) and may name a new `resource`.

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:refresh}}
```

### Token exchange (RFC 8693)

`grant_type=urn:ietf:params:oauth:grant-type:token-exchange` exchanges a
`subject_token` (access token, ID token or JWT issued by this server, RS256 or the
HS256 demo secret) for a new access token with the requested `audience` /
`resource` and an optionally narrowed `scope`. An `actor_token` adds an `act`
claim (delegation, nested when the subject was already delegated).
`requested_token_type` may be `access_token`, `jwt` or `id_token`.

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:token_exchange}}
```

## UserInfo, introspection, revocation

`/oauth/userinfo` accepts an access token from this provider (Bearer header, or an
`access_token` form field on POST) and requires the `openid` scope (`403
insufficient_scope` otherwise).

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:userinfo}}
```

`/oauth/introspect` needs a confidential client and reports `active`, `token_type`,
`scope`, `client_id`, `sub`, `aud`, `iss`, `exp`, `iat`, `jti`, `act` and
`username`; refresh tokens can be introspected too.

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:introspect}}
```

`/oauth/revoke` always answers `200` for a well-formed request; a client can only
revoke its own tokens. Revoked access tokens are rejected by introspection,
UserInfo and token exchange (but not by `/auth/jwt`, which checks only the
signature and claims, like a gateway validating JWTs locally would).

```hurl
{{#include ../../examples/oidc/discovery_and_grants.hurl:revoke}}
```

## Dynamic client registration

`POST /oauth/register` takes RFC 7591 metadata (`client_name`, `redirect_uris`
(required for `authorization_code`, at most 10), `grant_types`, `response_types`
(only `code`), `token_endpoint_auth_method`: `client_secret_basic` (default),
`client_secret_post` or `none` for a public client, and `scope`). It returns
`201` with a `dyn-...` client id (and a secret for confidential clients).
Registrations are kept for 24 hours in a bounded registry (1000 clients, 200 in
public mode). MCP clients use this to onboard themselves.

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:register}}
```

```hurl
{{#include ../../examples/oidc/authorization_code.hurl:register_public}}
```

The `conformance/oauth/oauth_flow.py` script walks through the same flows the way an
MCP client does (metadata, registration, PKCE with `resource`, refresh, UserInfo,
introspection, revocation and token exchange).
