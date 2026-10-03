# Sessions, public mode and the admin token

## Sessions

Several features keep per-client state. The client is identified by a **session
key**: the `X-Rustybin-Session` header when it is present and valid (1 to 128
characters of `A-Z a-z 0-9 . _ : -`), otherwise the client IP (`ip:<address>`,
which honours proxy headers only with `RUSTYBIN_TRUST_FORWARD=true`).

| Feature | Scoped by session |
|---|---|
| [Flaky endpoints](../reference/reliability.md) | Counters per session and route: two demo attendees never share a circuit breaker |
| [Request bins](../reference/request-bin.md) | `GET /bin` lists the caller's bins; public mode caps bins per session |
| [A2A tasks](../a2a/index.md) | Tasks are only visible to the session (and agent) that created them |
| [Mock LLM](../ai/gateway-features.md) | Anthropic prompt cache hits; in public mode the AI request records |
| [Request inspector](inspector.md) | `?session=` filter; mandatory in public mode |

Gateways often run several workers or sit behind NAT, so for demos send an explicit
session header (or have the gateway add one per consumer) rather than relying on
the client IP.

MCP sessions are a different thing: `Mcp-Session-Id` is the MCP protocol's own
session (see [MCP](../mcp/index.md)).

## Admin token

Instance-global mutations affect every user of an instance. When
`RUSTYBIN_ADMIN_TOKEN` is set they require the token, as
`Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>` (compared in
constant time; `401` otherwise):

| Mutation | Endpoint |
|---|---|
| Health toggles | `POST /health/healthy`, `POST /health/unhealthy`, `POST /health/toggle` |
| Reset every client's flaky counters | `POST /flaky/reset?scope=all` |
| Clear every captured request | `DELETE /_rustybin/requests` without `?session=` |

Without a token they are open, except in public mode where they are refused with
`403`. Per-session operations (`POST /flaky/reset`, `DELETE
/_rustybin/requests?session=...`) and deleting a bin (the random bin id acts as
the capability) never need the token.

```hurl
{{#include ../../examples/reliability/health.hurl:unhealthy}}
```

The token is never returned by any endpoint; `/_rustybin/config` and `/identity`
only report `admin_token_configured`.

## Public mode

`RUSTYBIN_PUBLIC_MODE=true` hardens an instance that strangers can reach (a shared
demo URL, a conference booth):

- **Inspector**: only requests carrying `X-Rustybin-Session` are captured, and the
  inspector API only returns the entries of the `?session=` you ask for.
- **Mock LLM records**: `/ai/requests` only lists the caller's own session, and an
  IP-derived session cannot be claimed through `?session=`.
- **Global mutations**: refused (`403`) unless `RUSTYBIN_ADMIN_TOKEN` is set.
- **A2A push**: `RUSTYBIN_A2A_PUSH_ALLOW_ALL` is ignored.
- **Lower caps** on delays (10 s), payload sizes, stream lengths and lifetimes,
  batch sizes, WebSocket limits and every bounded store; see the table in
  [Configuration](../configuration.md#limits-that-are-not-configurable).
- The landing page footer shows "public mode".

Everything else works the same, so the demos you rehearse locally behave the same
on the public instance.
