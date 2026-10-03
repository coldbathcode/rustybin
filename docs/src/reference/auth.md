# Authentication

These endpoints really check credentials, so they work both as an upstream that a
gateway must authenticate to (credential injection) and as a target behind a
gateway that authenticates the client (then the gateway forwards or strips the
credential and you can show the difference). Successful checks return
`{"authenticated": true, "auth_type": ..., ...}`; failures return `401` with
`{"authenticated": false, "error": ...}`. All of them accept any method and
negotiate JSON or XML.

## Basic auth

| Route | Expected credentials |
|---|---|
| `/auth/basic-auth` | user `basic`, password `password` |
| `/auth/basic-auth/{username}/{password}` | the ones in the path |

Failures carry `WWW-Authenticate: Basic realm="rustybin", charset="UTF-8"`, so a
browser shows its login dialog. Comparisons are constant time.

```hurl
{{#include ../../examples/auth/basic_apikey.hurl:basic}}
```

```hurl
{{#include ../../examples/auth/basic_apikey.hurl:basic_custom}}
```

```hurl
{{#include ../../examples/auth/basic_apikey.hurl:basic_fail}}
```

## API keys

| Route | Expected header |
|---|---|
| `/auth/api-key` | `apikey: my-key` |
| `/auth/api-key/{header_name}/{key_value}` | the header and value in the path (header name case-insensitive) |

```hurl
{{#include ../../examples/auth/basic_apikey.hurl:apikey}}
```

```hurl
{{#include ../../examples/auth/basic_apikey.hurl:apikey_custom}}
```

## JWT

| Route | Behaviour |
|---|---|
| `/auth/jwt` | Validates the Bearer token: signature, `exp` (required), `nbf`, with 30 s leeway, plus `?iss=` and `?aud=` when given |
| `/auth/jwt/decode` | Decodes the Bearer token **without any validation**: "what did the gateway forward?" |
| `/auth/jwt/exchange` | Validates like `/auth/jwt`, then returns a new HS256 token with the same claims and a new `iss` (`rustybin`), `iat`, `jti` and one hour `exp` |

The algorithm comes from the token header and only two are accepted:

- `HS256` with the public demo secret `rustybin-demo-secret-do-not-use-in-production`
  (configure it in a gateway to mint or validate tokens Rustybin accepts);
- `RS256` with the key of the built-in identity provider, published at
  `/oauth/jwks` (`kid` `rustybin-rs256-key`). The key pair is generated at
  startup, so RS256 tokens do not survive a restart.

`alg: none` and every other algorithm are rejected. Failures carry an RFC 6750
challenge: `WWW-Authenticate: Bearer realm="rustybin", error="invalid_token",
error_description="..."`.

A ready-made HS256 demo token (`sub` 1234567890, `name` John Doe, `iss` and `aud`
`rustybin`, `scope` "openid profile", valid until 2100):

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:jwt_hs256}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:jwt_wrong_audience}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:jwt_rs256}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:jwt_decode}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:jwt_none_rejected}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:jwt_exchange}}
```

## HMAC signatures

`/auth/hmac` (user `alice`, secret `secret`) and `/auth/hmac/{username}/{secret}`
validate request signatures in the common gateway hmac-auth format and in the
draft-cavage HTTP Signatures format:

```text
Authorization: hmac username="alice", algorithm="hmac-sha256", headers="date request-line", signature="<base64>"
Authorization: Signature keyId="alice", algorithm="hmac-sha256", headers="date (request-target)", signature="<base64>"
```

- The credentials can also come in `Proxy-Authorization`.
- Algorithms: `hmac-sha1`, `hmac-sha256`, `hmac-sha384`, `hmac-sha512`.
- The signing string has one `name: value` line per header listed in `headers`
  (default `date`; repeated headers joined with `, `), joined by `\n`. Two pseudo
  headers exist: `request-line` (`GET /path?query HTTP/1.1`) and `(request-target)`
  (`(request-target): get /path?query`).
- `Date` or `X-Date` is required and must be within `?clock_skew=` seconds of the
  server clock (default 300, at most 604800; `0` disables the check, handy for fixed
  examples).
- When `digest` is signed, the `Digest: SHA-256=<base64>` (or `SHA-512=`) header is
  checked against the body.
- The response shows the signing string the server computed, which is the fastest
  way to debug a gateway's signing plugin.

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:hmac}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:hmac_digest}}
```

```hurl
{{#include ../../examples/auth/jwt_hmac.hurl:hmac_fail}}
```

## mTLS

`/auth/mtls` authenticates a client certificate issued by the demo CA (see
[the demo PKI](../configuration.md#proxies-and-tls)). It looks in two places:

1. **TLS mode**: the certificate the client presented to Rustybin's own HTTPS
   listener (verified during the handshake).
2. **Header mode**: when `RUSTYBIN_MTLS_IN_HEADER` names a header, a gateway that
   terminated mTLS can forward the client certificate there (URL-encoded PEM, raw
   PEM with spaces instead of newlines, or base64 DER). Rustybin verifies the
   signature chain to the demo CA and the validity period.

The response has the subject (`client_dn`), issuer (`client_ca`) and, in `claims`,
serial, validity, SHA-256 fingerprint and `source` (`tls` or `header`).

| Route | Purpose |
|---|---|
| `GET /auth/mtls/get-client-cert` | A client certificate and key issued by the demo CA (`cert_pem`, `key_pem`) |
| `GET /auth/mtls/get-ca-cert` | The demo CA certificate (`ca_cert_pem`), to verify the HTTPS listener or configure a gateway's trust store |

```hurl
{{#include ../../examples/auth/mtls.hurl:get_client_cert}}
```

```hurl
{{#include ../../examples/auth/mtls.hurl:get_ca}}
```

Header mode (the documentation examples run with
`RUSTYBIN_MTLS_IN_HEADER=X-Client-Cert`; Hurl's `urlEncode` filter URL-encodes the
PEM):

```hurl
{{#include ../../examples/auth/mtls.hurl:header_mode}}
```

```hurl
{{#include ../../examples/auth/mtls.hurl:header_missing}}
```

TLS mode with curl (`$HTTP` and `$HTTPS` are the HTTP and HTTPS base URLs, for
example `http://localhost:8080` and `https://localhost:8443`):

```bash
{{#include ../../examples/auth/mtls_tls.sh:download}}
```

```bash
{{#include ../../examples/auth/mtls_tls.sh:tls_mode}}
```

```bash
{{#include ../../examples/auth/mtls_tls.sh:tls_no_cert}}
```

Behind a platform that terminates TLS itself (Fly.io's edge, most load balancers),
the HTTPS listener is not reachable from clients: use header mode.

## OAuth 2.0 and OpenID Connect

Tokens for `/auth/jwt`, the MCP and A2A demos and your gateway's OIDC plugin come
from the built-in identity provider: see [OAuth 2.0 / OIDC provider](oidc.md).
