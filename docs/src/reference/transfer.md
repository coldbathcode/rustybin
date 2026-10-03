# Transfer, compression, ranges and images

## Compression

`/gzip`, `/deflate`, `/brotli` and `/zstd` return a small JSON echo (`gzipped`,
`deflated`, `brotli` or `zstd` set to `true`, plus `method`, `headers` and `origin`)
compressed with that encoding and labelled with `Content-Encoding` (`gzip`,
`deflate` in the zlib format, `br`, `zstd`) and `Vary: Accept-Encoding`. They do so
**whatever `Accept-Encoding` says**, so you can show a gateway decompressing for a
client that cannot, or passing the encoding through.

```hurl
{{#include ../../examples/http/transfer.hurl:gzip}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:brotli}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:deflate_zstd}}
```

`GET /encoding/utf8` is an HTML page of multi-script UTF-8 text (Latin with
diacritics, Greek, Cyrillic, Hebrew, Arabic, CJK, Thai, math symbols, emoji) for
charset and transcoding tests.

```hurl
{{#include ../../examples/http/transfer.hurl:utf8}}
```

## Ranges

`GET /range/{n}` returns `n` bytes (`abc...z` repeated, 1 to 102400; 10240 in public
mode) with `Accept-Ranges: bytes` and a strong `ETag: "range<n>"`. A single range
(`bytes=a-b`, `bytes=a-`, `bytes=-n`) gets `206` with `Content-Range`; an
unsatisfiable or malformed one gets `416` with `Content-Range: bytes */<n>`.
Multiple ranges and unknown units get the full `200` body (allowed by RFC 9110).
`If-Range` with a different ETag also returns the full body.

```hurl
{{#include ../../examples/http/transfer.hurl:range}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:range_suffix}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:range_if_range}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:range_416}}
```

## Data transfer

| Route | Behaviour |
|---|---|
| `GET /bytes/{n}` | `n` random bytes (`application/octet-stream`); `?seed=` makes them reproducible |
| `GET /stream-bytes/{n}` | The same, streamed in chunks of `?chunk_size=` bytes (default 10240) |
| `GET /stream/{n}` | `n` JSON lines (`url`, `args`, `headers`, `origin`, `id`), streamed |
| `GET /drip` | `?numbytes=` bytes (default 10) of `*` spread over `?duration=` seconds (default 2), after `?delay=` seconds (default 0), with status `?code=` (default 200) |
| `GET /links/{n}` | Redirects to `/links/{n}/0` |
| `GET /links/{n}/{offset}` | An HTML page with `n` links (at most 200), the current one unlinked |
| `GET /base64/{value}` | Decodes standard or URL-safe base64 (padding optional); text comes back as `text/plain`, anything else as `application/octet-stream` |

Caps (normal / public mode): `/bytes` and `/stream-bytes` 102400 / 10240 bytes,
`/stream` 100 / 20 lines, `/drip` 10240 / 1024 bytes. Counts above the cap are
clamped (like httpbin) and the response says so in `X-Rustybin-Capped: <cap>`;
`/drip` instead rejects too many bytes with `400`. `/drip` duration and delay are
capped by the instance delay limit (30 s, 10 s in public mode).

```hurl
{{#include ../../examples/http/transfer.hurl:bytes}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:bytes_capped}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:stream}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:stream_bytes}}
```

`/drip` sends its headers (with the final `Content-Length`) right away and then
the body slowly: use it for streaming and idle-timeout policies.

```hurl
{{#include ../../examples/http/transfer.hurl:drip}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:links}}
```

```hurl
{{#include ../../examples/http/transfer.hurl:base64}}
```

## Client information

| Route | Returns |
|---|---|
| `GET /ip` | `{"ipv4": ..., "ipv6": ...}` (the other family is `null`) |
| `GET /ip/v4`, `GET /ip/v6` | One family only |
| `GET /date`, `GET /date/{timezone}` | Today's date, in UTC or an IANA timezone (`/date/America/New_York`) |
| `GET /time`, `GET /time/{timezone}` | The current time as RFC 3339; unknown timezones get `404` |

The client IP honours proxy headers only with `RUSTYBIN_TRUST_FORWARD=true` (see
[Configuration](../configuration.md#proxies-and-tls)), which makes `/ip` the quickest
check of what a gateway forwards.

```hurl
{{#include ../../examples/http/info.hurl:ip}}
```

```hurl
{{#include ../../examples/http/info.hurl:time}}
```

## Random values

`/uuid` (v4), `/guuid` (a braced GUID), `/random` (a bundle), `/random/int`,
`/random/int/{lower}/{upper}` (inclusive; `lower` must be smaller than `upper`),
`/random/uint`, `/random/lorem-ipsum` and `/random/lorem-ipsum/{count}` (1 to 32
paragraphs).

```hurl
{{#include ../../examples/http/info.hurl:random}}
```

## Images

Small, valid images for content-type routing, transformation and caching tests:
`/image/png` (16x16), `/image/jpeg`, `/image/gif` and `/image/webp` (8x8),
`/image/svg`. `GET /image` picks the format from `Accept` (webp, svg, jpeg, png, gif
in that order of preference; `*/*` gives webp, no `Accept` header gives png) and answers `406` when
nothing matches.

```hurl
{{#include ../../examples/http/info.hurl:images}}
```
