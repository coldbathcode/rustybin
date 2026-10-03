# Request ids, CORS and the control plane

## Request ids

Every response carries `X-Request-Id`. When the request has one (for example set by
the gateway), it is kept and echoed back; otherwise a UUID is generated. The id is
also visible to the route (in `/echo` headers), stored with the
[inspector](inspector.md) entry, logged with the request, and used by the mock LLM
as `X-Rustybin-Request-Id` (the key of [`/ai/requests/{id}`](../ai/gateway-features.md#request-inspection)).

```hurl
{{#include ../../examples/http/echo.hurl:request_id}}
```

```hurl
{{#include ../../examples/concepts/control.hurl:request_id_generated}}
```

## Unknown routes

Unknown paths get a JSON 404 that names the method and path, with a hint:

```hurl
{{#include ../../examples/concepts/control.hurl:not_found}}
```

## CORS

By default Rustybin answers CORS for any origin (`RUSTYBIN_CORS_ORIGINS=*`), so
browser demos and the web console work without a gateway. A preflight (an
`OPTIONS` request with `Access-Control-Request-Method`) is answered directly,
mirroring the requested method and headers, with `Access-Control-Max-Age: 600`. A
plain `OPTIONS` request without that header reaches the route. Responses expose
`X-Request-Id`, `X-Rustybin-Fault`, `Content-Length`, `Mcp-Session-Id`,
`MCP-Protocol-Version` and `WWW-Authenticate` to browser code.

```hurl
{{#include ../../examples/concepts/control.hurl:cors_preflight}}
```

To demonstrate a gateway's own CORS plugin, turn Rustybin's off with
`RUSTYBIN_CORS_ORIGINS=off`, or restrict it with a comma-separated origin list.

## Control plane

Routes under `/_rustybin/` describe and manage the instance. They are never
captured by the inspector and ignore the fault injection headers.

| Route | Purpose |
|---|---|
| `GET /_rustybin/config` | Effective configuration, without secrets |
| `GET /_rustybin/version` | Name, version, MSRV and build profile |
| `GET, DELETE /_rustybin/requests`, `GET /_rustybin/requests/{id}`, `GET /_rustybin/requests/stream` | The [request inspector](inspector.md) |
| `GET /_rustybin/catalog` | The route catalogue as JSON, for the [web console](../console.md) <!-- TODO(console): verify against ui/ after merge --> |

```hurl
{{#include ../../examples/concepts/control.hurl:config}}
```

```hurl
{{#include ../../examples/concepts/control.hurl:version}}
```

Related instance endpoints outside the control plane: `GET /` (landing page, always
200, use it for platform liveness checks), [`/health`](../reference/reliability.md#health-toggle)
(a demo toggle) and [`/identity`](../reference/identity.md).
