# Redirects and cookies

## Redirects

| Route | Behaviour |
|---|---|
| `GET /redirect/{n}` | A chain of `n` relative `302` redirects (1 to 20) ending at `/echo`; the query string is carried along |
| `GET /absolute-redirect/{n}` | The same with absolute URLs on the request's own origin (1 to 10): `https` on the HTTPS listener, and the proxy's `Forwarded` / `X-Forwarded-*` scheme and host with `RUSTYBIN_TRUST_FORWARD=true` |
| `ANY /redirect-to?url=&status_code=` | One redirect to `url` with `status_code` 300, 301, 302 (default), 303, 307 or 308 |

`/redirect-to` is not an open redirect: `url` must be a path starting with a single
`/`, or an `http(s)` URL whose `host:port` equals the request's `Host`. Anything
else (other hosts, `//host`, credentials, whitespace, backslashes) is refused with
`400`, so a public instance cannot be abused for phishing redirects.

```hurl
{{#include ../../examples/http/redirects.hurl:redirect_chain}}
```

```hurl
{{#include ../../examples/http/redirects.hurl:redirect_to}}
```

```hurl
{{#include ../../examples/http/redirects.hurl:redirect_to_refused}}
```

```hurl
{{#include ../../examples/http/redirects.hurl:absolute_redirect}}
```

## Cookies

| Route | Behaviour |
|---|---|
| `GET /cookies` | The request's cookies as JSON (`{"cookies": {...}}`) |
| `GET /cookies/set?name=value&...` | Sets each pair, then `302` to `/cookies` |
| `GET /cookies/set/{name}/{value}` | Sets one cookie (`Path=/`), then `302` to `/cookies` |
| `GET /cookies/delete?name&...` | Expires the named cookies (`Max-Age=0` and a 1970 `Expires`), then `302` |

Attributes for `/cookies/set` are query parameters starting with `_`: `_path`
(default `/`), `_domain`, `_secure`, `_httponly`, `_samesite` (`Strict`, `Lax` or
`None`; `None` forces `Secure`, as browsers require) and `_maxage` (seconds). For
`/cookies/delete`, `_path` and `_domain` must match how the cookie was set. Names
must be RFC 6265 tokens (else `400`), values are percent-encoded where needed so they
can never inject attributes, and at most 20 cookies are handled per request.

```hurl
{{#include ../../examples/http/redirects.hurl:cookies_set}}
```

```hurl
{{#include ../../examples/http/redirects.hurl:cookies_samesite}}
```

```hurl
{{#include ../../examples/http/redirects.hurl:cookies_read}}
```

```hurl
{{#include ../../examples/http/redirects.hurl:cookies_delete}}
```
