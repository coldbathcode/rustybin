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
| `/export/har.json` | HAR 1.2 archive | URLs use `http://localhost` |

The base URL defaults to `http://localhost` in every format: set the variable to
your instance or gateway after importing.

```hurl
{{#include ../examples/exports/exports.hurl:postman}}
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
