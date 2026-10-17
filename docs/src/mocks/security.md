# Mocked security

A mock can enforce authentication in front of its operations, so a gateway's auth
plugins (key injection, token exchange, OAuth client credentials, mTLS to the
upstream) have something real to satisfy. Security is **off** by default, even
when the spec declares `securitySchemes`; switch it on per mock:

```json
{"security": {"enabled": true}}
```

in the create body, a `PATCH` (then deploy), [`rustybin.yaml`](config.md#reference)
or the console's **Security** tab. Failures are answered before request validation
and response selection, as `application/problem+json`, and captured with their
reason (`auth` in the capture, outcome `unauthorized` or `forbidden`):

| Result | Status |
|---|---|
| No credentials, or wrong ones | `401` with a `WWW-Authenticate` challenge (`Basic realm="mock"` or `Bearer realm="mock"`) when the scheme has one |
| A valid token without a required scope | `403`, `detail: "insufficient scope (needs ...)"` |

## Spec mode

`security.mode: spec` (the default) honours the spec: each operation's `security`
(or the document's top-level one) is a list of alternatives, any one of which is
enough, and each alternative may combine several schemes, all of which are needed.
`security: []` on an operation makes it open.

| Scheme in `components.securitySchemes` | Accepted credentials |
|---|---|
| `apiKey` (`in: header`, `query` or `cookie`) | One of `security.api_key.keys` (default `demo-key`) under the scheme's own name |
| `http` `basic` | A user and password from `security.basic.users` (default `demo` / `demo`) |
| `http` `bearer` | A token from `security.bearer.tokens` (default `demo-token`), or a JWT from the built-in [identity provider](../reference/oidc.md) |
| `oauth2`, `openIdConnect` | A JWT from the built-in identity provider whose `scope` (or `scp`) claim holds every scope the requirement lists; a token from `security.bearer.tokens` counts as having every scope |
| `mutualTLS` | A client certificate from the [demo CA](../reference/auth.md#mtls): on the HTTPS listener, or forwarded by a gateway in the `RUSTYBIN_MTLS_IN_HEADER` header |

JWTs are verified with the identity provider's RS256 key (signature and expiry);
`security.jwt.issuer` and `security.jwt.audience` add `iss` and `aud` checks. Keys,
passwords and tokens are compared in constant time. Schemes of other types are not
enforced (a spec warning names them).

The built-in sample declares `apiKey` (header `X-API-Key`) or `bearer` on its write
operations:

```hurl
{{#include ../../examples/mocks/security.hurl:petstore_on}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:petstore_401}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:petstore_credentials}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:petstore_capture}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:petstore_off}}
```

## OAuth 2.0 and OpenID Connect with the built-in IdP

Tokens come from the instance's own identity provider: `POST /oauth/token` with the
demo client `rustybin` / `secret` and the `client_credentials` grant (or any other
grant it supports) returns an RS256 JWT carrying the requested `scope`. When
security is on, the served spec (`/mock/{name}/_rustybin/openapi.json`) points the
`oauth2` flows' `tokenUrl`, `refreshUrl` and `authorizationUrl`, and an
`openIdConnect` scheme's `openIdConnectUrl`, at that identity provider, so a gateway
or API client that imports the spec can really obtain a token.

This mock combines an `oauth2` scheme with scopes, an API key in the query, an
open health check and a `mutualTLS` operation:

```hurl
{{#include ../../examples/mocks/security.hurl:create_secure}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:served_flows}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:oauth_flow}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:alternatives}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:mutual_tls}}
```

## Fixed modes

For specs that declare no security, or to demo one scheme regardless of the spec,
pick a fixed mode: it protects **every** operation (the spec's requirements, and
`security: []`, are ignored) except the ones marked `public`.

| `security.mode` | Accepted credentials |
|---|---|
| `api_key` | One of `api_key.keys` in the header `api_key.header` (default `x-api-key`) |
| `basic` | A user from `basic.users` |
| `bearer` | A token from `bearer.tokens`, or a JWT from the built-in identity provider |
| `jwt` | Only a JWT from the built-in identity provider (with the `jwt.issuer` / `jwt.audience` checks when set) |

```hurl
{{#include ../../examples/mocks/security.hurl:fixed_basic}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:fixed_api_key}}
```

```hurl
{{#include ../../examples/mocks/security.hurl:fixed_jwt}}
```

## Demo credentials

| Setting | Default | Limit |
|---|---|---|
| `security.api_key.keys` | `["demo-key"]` | 50 keys, 1 to 512 characters |
| `security.api_key.header` (fixed `api_key` mode) | `x-api-key` | a valid header name |
| `security.basic.users` | `{"demo": "demo"}` | 50 users |
| `security.bearer.tokens` | `["demo-token"]` | 50 tokens, 1 to 512 characters |
| `security.jwt.issuer`, `security.jwt.audience` | unset (not checked) | |

Setting `security` replaces the whole section, so list every credential you want
to keep. The mock's details (`GET /_rustybin/mocks/{name}`, the console's Security
tab) show them under `credentials`, with a hint per scheme and the token endpoint;
basic passwords are not shown there. In `rustybin.yaml`, write secrets as
`${ENV_VAR}` references ([environment variables](config.md#environment-variables)).
The deploy report's sample requests and the console's "Try it" already carry the
demo credentials.

## Public operations

`"public": true` in an operation's settings switches mocked security off for that
operation in every mode, for health checks and other open endpoints:

```hurl
{{#include ../../examples/mocks/security.hurl:public_operation}}
```
