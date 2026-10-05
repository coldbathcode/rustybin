# gRPC

The gRPC listener (default port 50051, `RUSTYBIN_GRPC_PORT`, plaintext h2c) serves,
on one port:

- `rustybin.echo.v1.EchoService` ([`proto/echo.proto`](https://github.com/oliwynn/rustybin/blob/main/proto/echo.proto)),
  all four call types plus a failure RPC;
- `grpc.health.v1.Health`, following the HTTP [`/health` toggle](reliability.md#health-toggle)
  (`SERVING` / `NOT_SERVING` for `""` and `rustybin.echo.v1.EchoService`);
- server reflection `grpc.reflection.v1` and `grpc.reflection.v1alpha`, so tools work
  without the `.proto` file;
- gRPC-Web (`application/grpc-web`, `application/grpc-web-text`) over HTTP/1.1 and
  HTTP/2, with permissive CORS for browser clients.

| RPC | Behaviour |
|---|---|
| `Echo` (unary) | Returns `message`, the request `metadata` (headers), `instance_id`, and the deadline (`grpc_timeout`, `deadline_ms`) when the client set one |
| `ServerStream` | `count` responses (default 3, max 100), 50 ms apart, with `index` |
| `ClientStream` | Reads up to 1000 messages (else `RESOURCE_EXHAUSTED`) and returns them joined with spaces, `index` = number received |
| `BidiStream` | Echoes each message as it arrives (max 1000) |
| `Fail` | Fails with `code` (0 to 16; 0 returns a normal response), `message`, rich error details (`google.rpc.ErrorInfo` with `reason`, domain `rustybin` and `metadata`; `google.rpc.RetryInfo` when `retry_delay_ms` is set) and the trailer `x-rustybin-fail: true` |

Use `Fail` for gRPC-to-HTTP status mapping and retry policy demos.

## With grpcurl

`$GRPC` is the listener's `host:port`, for example `localhost:50051`:

```bash
{{#include ../../examples/protocols/grpc.sh:list}}
```

```bash
{{#include ../../examples/protocols/grpc.sh:unary}}
```

```bash
{{#include ../../examples/protocols/grpc.sh:with_proto}}
```

```bash
{{#include ../../examples/protocols/grpc.sh:server_stream}}
```

```bash
{{#include ../../examples/protocols/grpc.sh:client_stream}}
```

```bash
{{#include ../../examples/protocols/grpc.sh:fail}}
```

```bash
{{#include ../../examples/protocols/grpc.sh:health}}
```

## gRPC-Web

gRPC-Web is served on the same port, so a browser (or a gateway's gRPC-Web
translation) can call it over HTTP/1.1. The body is a length-prefixed protobuf
frame; the response ends with a trailer frame carrying `grpc-status`.

```hurl
{{#include ../../examples/protocols/grpc_web.hurl:grpc_web}}
```

```hurl
{{#include ../../examples/protocols/grpc_web.hurl:grpc_web_cors}}
```

## On the HTTP and HTTPS ports

With `RUSTYBIN_GRPC_ON_HTTP=true` the HTTP and HTTPS listeners serve the same gRPC
services (EchoService, health, reflection, gRPC-Web) for requests with
`content-type: application/grpc*`; everything else is the usual HTTP surface. A
TLS-terminating proxy or platform edge can then forward gRPC on port 443 to the
HTTP listener as cleartext HTTP/2 (h2c), next to the web traffic. The HTTPS listener
offers HTTP/2 through ALPN in this mode. The separate gRPC port keeps working.

`$HTTP_ADDR` and `$HTTPS_ADDR` are the listeners' `host:port`:

```bash
{{#include ../../examples/protocols/grpc_on_http.sh:h2c}}
```

```bash
{{#include ../../examples/protocols/grpc_on_http.sh:tls}}
```

```bash
{{#include ../../examples/protocols/grpc_on_http.sh:health}}
```

gRPC requests on the HTTP ports skip the HTTP middleware (inspector, fault headers,
CORS of the HTTP routes); plan limits apply to `EchoService` as on the gRPC port, and
`rustybin_protocol_requests_total{protocol="grpc"}` counts them.

## Behind a gateway

Route HTTP/2 traffic for `/rustybin.echo.v1.EchoService/*` to port 50051 with an
h2c (cleartext HTTP/2) upstream. `Echo` returns the metadata it received, so
headers added by the gateway (authentication, tracing, consumer identity) are
visible in the response; `instance_id` shows which replica answered.
