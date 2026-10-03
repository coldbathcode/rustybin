# Request inspector

The inspector records recent requests so you can see what actually reached the
upstream after the gateway rewrote, enriched or stripped it. It captures every
request except the control plane (`/_rustybin/*`), the web console (`/ui/*`), the
landing page (`/`) and `/favicon.ico`.

Each entry has: an inspector `id`, the `request_id` (`X-Request-Id`), timestamp,
method, URI, path, query, HTTP version, every header, client IP, the
`X-Rustybin-Session` value, the body (up to 64 KiB; UTF-8 as `body`, anything else
as `body_base64`, with `body_size` and `body_truncated`), the response status and the
time to response headers in `latency_ms`.

Entries live in a ring buffer of `RUSTYBIN_INSPECTOR_CAPACITY` entries (default 500,
at most 10000); the oldest entry is evicted first.

## Tag, list, fetch

Tag your traffic with `X-Rustybin-Session` (1 to 128 characters of `A-Z a-z 0-9 . _ : -`)
to find it again among everyone else's:

```hurl
{{#include ../../examples/concepts/inspector.hurl:tagged_request}}
```

`GET /_rustybin/requests` lists entries newest first. Filters: `?session=`,
`?path_prefix=`, `?limit=` (default 100, capped at the capacity).

```hurl
{{#include ../../examples/concepts/inspector.hurl:list}}
```

```hurl
{{#include ../../examples/concepts/inspector.hurl:get_one}}
```

```hurl
{{#include ../../examples/concepts/inspector.hurl:filter_prefix}}
```

## Live feed

`GET /_rustybin/requests/stream` is a Server-Sent Events feed with one `request`
event per captured request (the event id is the inspector id, the data the full
entry as JSON). It accepts the same filters as the list and sends keep-alive
comments while idle:

```bash
{{#include ../../examples/concepts/live_feeds.sh:inspector_feed}}
```

The feed never ends on its own (the example stops reading after two seconds); the
[web console](../console.md) uses it for its live traffic view.

## Clearing

`DELETE /_rustybin/requests?session=<id>` removes one session's entries and needs
no credentials. Without `?session=` it clears everything, which is an
instance-global mutation guarded by the [admin token](sessions.md#admin-token):

```hurl
{{#include ../../examples/concepts/inspector.hurl:clear_session}}
```

```hurl
{{#include ../../examples/concepts/inspector.hurl:clear_all_needs_admin}}
```

## Public mode

With `RUSTYBIN_PUBLIC_MODE=true` only requests carrying `X-Rustybin-Session` are
captured, the list and the live feed require `?session=` (400 otherwise) and only
return that session's entries, and `GET /_rustybin/requests/{id}` only answers when
`?session=` names the session the entry belongs to.

## Related

- The [AI request inspector](../ai/gateway-features.md#request-inspection)
  (`/ai/requests/{id}`) shows what the mock LLM made of a request (normalised prompt,
  token counts, redacted credentials) and links to the inspector entry.
- A [request bin](../reference/request-bin.md) gives a webhook sender or a logging
  plugin its own URL and history.
