# Collection exports and API description

Every catalogue example (see [Route catalogue](concepts/catalogue.md)) is available as
a ready-made collection for the usual API clients, grouped by category. WebSocket
endpoints and examples that never end (live feeds) are left out, because plain
HTTP clients cannot run them. Some examples contain placeholders such as
`<access_token>` or `<from-step-1>` where a value from an earlier response belongs.

| Route | Format | Base URL setting |
|---|---|---|
| `/export/postman.json` | Postman collection v2.1 (download) | collection variable `base_url` |
| `/export/insomnia.json` | Insomnia export v4 (download) | environment variable `base_url` |
| `/export/bruno.json` | Bruno collection (download) | `base_url` |
| `/export/curl.sh` | Bash script of curl commands | `BASE_URL=... bash rustybin-curl.sh` |
| `/export/requests.http` | VS Code REST Client / JetBrains HTTP Client | `@base_url` at the top |
| `/export/requests.hurl` | [Hurl](https://hurl.dev) file (asserts only `HTTP *`) | `hurl --variable base_url=...` |
| `/export/k6.js` | [k6](https://k6.io) script (one iteration, checks that each request got a response) | `BASE_URL=... k6 run rustybin-k6.js` |
| `/export/har.json` | HAR 1.2 archive | absolute URLs on the base URL |

The base URL written into every format is the origin the export was downloaded
from: the scheme of the listener (`https` on the HTTPS port), the `Host` header,
and, with `RUSTYBIN_TRUST_FORWARD=true`, the `Forwarded` / `X-Forwarded-Proto` /
`X-Forwarded-Host` / `X-Forwarded-Port` headers of your proxy. Download an export
through your gateway and it already points at the gateway.

```hurl
{{#include ../examples/exports/exports.hurl:postman}}
```

To target another URL, pass `?base_url=` (an absolute `http` or `https` URL, with an
optional path prefix such as `https://gateway.example.com/rustybin`; no credentials,
query or fragment). Anything else is a `400` with a JSON error. You can still change
the variable after importing.

```hurl
{{#include ../examples/exports/exports.hurl:base_url_override}}
```

```hurl
{{#include ../examples/exports/exports.hurl:other_exports}}
```

## OpenAPI

`/openapi.json` and `/openapi.yaml` describe the HTTP API (OpenAPI 3.0.3), and
`/docs` renders it as an interactive reference (the Scalar viewer, loaded from a CDN, so
`/docs` needs internet access unlike the [web console](console.md)). Import the spec into a gateway to
generate routes, or into a client generator. The landing page `/` lists every
endpoint with a runnable example.

```hurl
{{#include ../examples/exports/exports.hurl:openapi}}
```

```hurl
{{#include ../examples/exports/exports.hurl:api_reference}}
```

```hurl
{{#include ../examples/exports/exports.hurl:landing}}
```

The examples in this documentation (`docs/examples/`) are a more thorough,
assertion-checked alternative to the generated Hurl export.
