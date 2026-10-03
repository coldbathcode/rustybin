# Request bin

A request bin is a URL that records everything sent to it. Point a webhook sender,
a gateway's HTTP logging plugin, a mirroring policy or a callback at it, then
inspect what arrived.

| Route | Purpose |
|---|---|
| `POST /bin` | Create a bin (`201`) and get its URLs |
| `GET /bin` | List the bins owned by your session |
| `ANY /bin/{id}` and `ANY /bin/{id}/{*path}` | Capture a request (except `DELETE /bin/{id}`, which deletes the bin) |
| `GET /bin/{id}/requests` | Captured requests, newest first (`?limit=`, default 50) |
| `GET /bin/{id}/requests/{n}` | One request by sequence number |
| `GET /bin/{id}/requests/stream` | Live feed (Server-Sent Events) |

## Creating a bin

The optional JSON body configures what the bin answers: `status`, `headers`
(name to value), `body` (a string, or JSON which is serialised and labelled
`application/json`) and `delay_ms` (capped by the instance delay limit). Without a
body the bin answers `200` with `{"ok": true, "bin": ..., "id": ..., "seq": n}`.
Every capture response carries `X-Rustybin-Bin-Seq`.

The bin belongs to the creating session (`X-Rustybin-Session`, else the client IP),
which is what `GET /bin` lists; anyone who knows the id can send to it, read it or
delete it.

```hurl
{{#include ../../examples/protocols/request_bin.hurl:create}}
```

## Capturing and inspecting

Each capture records the method, the path below the bin (`path`) and the full path,
query, headers, client IP, timestamp and the body (up to 64 KiB, as text or
`body_base64`). The inspection routes are never captured themselves.

```hurl
{{#include ../../examples/protocols/request_bin.hurl:capture}}
```

```hurl
{{#include ../../examples/protocols/request_bin.hurl:inspect}}
```

```hurl
{{#include ../../examples/protocols/request_bin.hurl:list_bins}}
```

```hurl
{{#include ../../examples/protocols/request_bin.hurl:default_response}}
```

The live feed sends one `request` event per capture with the sequence number as
event id; `Last-Event-ID` replays the stored requests after that number. It ends
when the bin expires or is deleted (the example stops reading after two seconds):

```bash
{{#include ../../examples/concepts/live_feeds.sh:bin_feed}}
```

To capture a `DELETE`, send it to a sub path: `DELETE /bin/{id}` deletes the bin.

```hurl
{{#include ../../examples/protocols/request_bin.hurl:delete}}
```

## Limits

| | Normal | Public mode |
|---|---|---|
| Bins | 200 | 100 (10 per session) |
| Requests kept per bin | 100 | 50 |
| Stored bytes per bin | 1 MiB | 256 KiB |
| Lifetime | 24 hours | 1 hour |
| Captured body | 64 KiB | 64 KiB |

The oldest requests are evicted first when a bin is full.
