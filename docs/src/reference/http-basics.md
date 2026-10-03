# Echo, status codes and response shaping

## Echo

`/echo`, `/echo/{*path}`, `/anything` and `/anything/{*path}` accept any method and
return what arrived: `method`, `url`, `path`, `path_info` (the path segments),
`query_string`, `query_params` (repeated keys become arrays), every header (each
name maps to a list of values), `host`, `port`, `scheme`, `remote_ip`, the `body`
and `timestamp_unix_ms`.

This is the workhorse for transformation demos: point the gateway route at `/echo`
and show the headers it added, the query parameters it rewrote or the body it
transformed.

```hurl
{{#include ../../examples/http/echo.hurl:echo_get}}
```

```hurl
{{#include ../../examples/http/echo.hurl:echo_post}}
```

The `body` object reports `present`, `included`, `body` (the text), `bytes`,
`truncated` and `utf8`. Binary bodies are not included (`reason: "binary"`); text
bodies above `RUSTYBIN_BODY_LIMIT` are truncated, although such requests are
rejected with `413` before they reach the route anyway.

`scheme`, `host`, `port` and `remote_ip` describe the client's view: the HTTPS
listener reports `https`, and with `RUSTYBIN_TRUST_FORWARD=true` the
`X-Forwarded-*`, `Forwarded` and `Fly-Client-IP` headers are honoured.

### JSON or XML

Plain data endpoints (echo, status, delay, cache, the auth checks, info and random
values, orchestration and more) negotiate the representation from `Accept`:
`application/json` (the default, also for browsers and `*/*`), `application/xml`
or `text/xml`, with q-values. They never answer `406` and always send
`Vary: Accept`. The XML form has a `<response>` root and one element per key.

```hurl
{{#include ../../examples/http/echo.hurl:echo_xml}}
```

## Status codes

`/status/{code}` answers any method with the status you ask for (200 to 599).

```hurl
{{#include ../../examples/http/status.hurl:status_single}}
```

```hurl
{{#include ../../examples/http/status.hurl:status_any_method}}
```

204, 205 and 304 have no body. Some statuses carry the header a client expects:

| Status | Extra header |
|---|---|
| 301, 302, 303, 307, 308 | `Location: /echo` |
| 401 | `WWW-Authenticate: Basic realm="rustybin"` |
| 407 | `Proxy-Authenticate: Basic realm="rustybin"` |
| 429, 503 | `Retry-After: 1` |

```hurl
{{#include ../../examples/http/status.hurl:status_extra_headers}}
```

A comma-separated list picks at random: `/status/200,500` uniformly, or with
weights, `/status/200:0.9,500:0.1` (weights are relative, at most 20 codes). The
body then has `chosen_from`.

```hurl
{{#include ../../examples/http/status.hurl:status_weighted}}
```

1xx codes are refused with `400` (an informational status cannot be a final
response):

```hurl
{{#include ../../examples/http/status.hurl:status_invalid}}
```

```hurl
{{#include ../../examples/http/status.hurl:status_xml}}
```

## Delays

`/delay/{duration}` (any method) waits, then echoes the request with `delay_ms`,
`requested_ms` and `max_delay_ms`. The duration is milliseconds (`1500`), `250ms`
or seconds (`1.5s`), at most 60 s (10 s in public mode); `?jitter=true` varies it
by up to 20 percent either way. For delays on any other route use the
[`X-Rustybin-Delay` header](../concepts/fault-injection.md).

```hurl
{{#include ../../examples/http/shaping.hurl:delay}}
```

```hurl
{{#include ../../examples/http/shaping.hurl:delay_units}}
```

```hurl
{{#include ../../examples/http/shaping.hurl:delay_too_long}}
```

## Response headers

`GET /response-headers?Name=value&...` sets each query parameter as a response
header (repeat a key for several values; at most 50) and returns them as JSON. Use it
to show a gateway stripping, rewriting or adding response headers, or caching by
`Cache-Control`.

```hurl
{{#include ../../examples/http/shaping.hurl:response_headers}}
```

Hop-by-hop and framing headers (`Connection`, `Content-Length`,
`Transfer-Encoding`, `Keep-Alive`, `Proxy-Connection`, `TE`, `Trailer`, `Upgrade`,
`Host`, `HTTP2-Settings`) are refused, so the endpoint cannot be used to corrupt a
response:

```hurl
{{#include ../../examples/http/shaping.hurl:response_headers_forbidden}}
```

## Caching

`GET /cache/{ttl}` returns `Cache-Control: public, max-age=<ttl>`, a strong `ETag`
and a `Last-Modified` (the server start time), and answers `304 Not Modified` to a
matching `If-None-Match` (weak comparison, lists and `*` supported) or to an
`If-Modified-Since` not older than `Last-Modified`. The body is stable for the
lifetime of the process, which makes it a good target for proxy cache demos.

```hurl
{{#include ../../examples/http/shaping.hurl:cache}}
```

```hurl
{{#include ../../examples/http/shaping.hurl:cache_ims}}
```
