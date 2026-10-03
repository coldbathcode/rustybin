# A2A agents

Rustybin hosts seven mock [Agent2Agent (A2A)](https://github.com/a2aproject/A2A)
agents for agent gateway demos: discovery through Agent Cards, routing per agent,
authentication, streaming, long-running tasks, multi-turn conversations and push
notifications. Each agent speaks both protocol generations over both bindings, and
the official `a2a-sdk` Python client is driven against every agent in
`conformance/a2a/`.

## Agents

| Agent | Path | Behaviour |
|---|---|---|
| Echo (default) | `/a2a/echo` (also `/a2a`) | Echoes every part (text, data, files) as a completed task with an `echo` artifact; replies with a direct message instead when the text starts with `msg:` or `metadata.reply` is `"message"` |
| Weather | `/a2a/weather` | A deterministic fake three-day forecast as a JSON data artifact plus a text summary ("weather in Paris" or data `{"city": "Paris"}`) |
| Travel planner | `/a2a/travel-planner` | Long-running: SUBMITTED, WORKING with status updates, artifact chunks (`append`, `lastChunk`), then COMPLETED with Markdown, JSON, a file URL and inline file bytes; `metadata.stepDelayMs` (0 to 5000) sets the pace |
| Approval | `/a2a/approval` | Multi-turn: stops in INPUT_REQUIRED and continues when a follow-up message with the same `taskId` says "approve" or "deny" |
| Flaky | `/a2a/flaky` | Every task ends FAILED unless the text contains "succeed" |
| Secure | `/a2a/secure` | Needs a bearer token from the built-in identity provider: without one the task stops in AUTH_REQUIRED; resend with the same `taskId` and `Authorization` to continue; has an authenticated extended card |
| Reject | `/a2a/reject` | Every task ends REJECTED |

## Protocol versions and bindings

| | v1.0 | v0.3 |
|---|---|---|
| Selected by | `A2A-Version: 1.0` header | no `A2A-Version` header (or `0.3`) |
| JSON-RPC (`POST /a2a/{agent}`) | `SendMessage`, `SendStreamingMessage`, `GetTask`, `ListTasks`, `CancelTask`, `SubscribeToTask`, `Create/Get/List/DeleteTaskPushNotificationConfig`, `GetExtendedAgentCard` | `message/send`, `message/stream`, `tasks/get`, `tasks/cancel`, `tasks/resubscribe`, `tasks/pushNotificationConfig/set/get/list/delete`, `agent/getAuthenticatedExtendedCard` |
| HTTP+JSON (`/a2a/{agent}/v1/...`) | ProtoJSON of the v1.0 proto (`TASK_STATE_COMPLETED`, `ROLE_USER`), errors as `google.rpc.Status` | ProtoJSON of the v0.3 proto |

The HTTP+JSON paths, below `/a2a/{agent}/v1`: `POST message:send`,
`POST message:stream` (SSE), `GET tasks`, `GET tasks/{id}`, `POST tasks/{id}:cancel`,
`POST tasks/{id}:subscribe` (SSE), `tasks/{id}/pushNotificationConfigs[/{configId}]`,
`GET extendedAgentCard` and `GET card`.

A method of one generation with the other generation's version is refused with
`VersionNotSupportedError` (`-32009`), like the reference SDK does.

## Discovery

| Route | Card |
|---|---|
| `GET /.well-known/agent-card.json` | The default (echo) agent, v1.0 fields plus the v0.3 fields so both client generations can read it; an extension lists every demo agent |
| `GET /.well-known/agent.json` | Legacy v0.3 card of the default agent (`url` + `preferredTransport`) |
| `GET /a2a` | A directory of the agents (card URLs, JSON-RPC and REST endpoints, `requiresAuth`) |
| `GET /a2a/{agent}`, `GET /a2a/{agent}/.well-known/agent-card.json` | The agent's card |
| `GET /a2a/{agent}/.well-known/agent.json` | The agent's legacy v0.3 card |

Card URLs are derived from the `Host` header (and `X-Forwarded-Proto` /
`X-Forwarded-Host` with `RUSTYBIN_TRUST_FORWARD=true`), so cards fetched through a
gateway point at the gateway. Cards carry `Cache-Control` and `ETag`.

```hurl
{{#include ../../examples/a2a/agents.hurl:directory}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:cards}}
```

## Sending messages

```hurl
{{#include ../../examples/a2a/agents.hurl:jsonrpc_v1}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:jsonrpc_v03}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:version_mismatch}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:direct_message}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:rest}}
```

## Task lifecycle

Tasks move through SUBMITTED, WORKING and then a terminal state (COMPLETED,
FAILED, CANCELED, REJECTED) or an interrupted state (INPUT_REQUIRED,
AUTH_REQUIRED) that a follow-up message with the same `taskId` resumes. A blocking
send waits for the task to settle (at most 100 seconds); `returnImmediately: true`
in the send configuration returns the task right away. Running tasks can be
canceled; canceling a finished task is `TaskNotCancelableError`, an unknown task
`TaskNotFoundError`.

Tasks are scoped to the session that created them ([`X-Rustybin-Session`, else the
client IP](../concepts/sessions.md)) and to their agent, so clients never see each
other's tasks. The store is bounded (5000 tasks, 1000 per session, idle TTL one hour;
2000, 50 and 15 minutes in public mode).

```hurl
{{#include ../../examples/a2a/agents.hurl:multi_turn}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:failure_states}}
```

Streaming (`SendStreamingMessage`, `message:stream`) sends the task first, then
status and artifact updates, and ends when the task reaches a terminal or
interrupted state (or after 10 minutes):

```hurl
{{#include ../../examples/a2a/agents.hurl:streaming}}
```

## Authentication

The `secure` agent accepts RS256 access tokens from the built-in
[identity provider](../reference/oidc.md); its card declares the scheme and its
extended card (`GetExtendedAgentCard`, `GET .../v1/extendedAgentCard`) lists an
extra skill and requires the token.

```hurl
{{#include ../../examples/a2a/agents.hurl:secure}}
```

## Push notifications

A push notification config (in the send `configuration` or created separately)
makes Rustybin POST every task update to a URL, with `Content-Type:
application/a2a+json`, `X-A2A-Notification-Token` when a `token` was set and an
`Authorization` header built from the config's `authentication`.

Because push URLs come from clients, they are an SSRF risk, so they are checked
when the config is created:

- this server's own sink, `/a2a/webhook-sink/{id}` (a bare path or a URL on the
  same host the request used), is always accepted and delivered in-process;
- hosts listed in `RUSTYBIN_A2A_PUSH_ALLOWLIST` (`host` or `host:port`) are
  delivered over HTTP(S);
- `RUSTYBIN_A2A_PUSH_ALLOW_ALL=true` accepts any http(s) URL, including loopback
  and private addresses, except in public mode;
- anything else is rejected with an invalid params error.

The built-in sink records what arrives: `GET /a2a/webhook-sink/{id}` lists the
notifications (headers and body), `DELETE` clears them, and anyone can `POST` to it
(ids are 1 to 64 characters of `A-Z a-z 0-9 . _ -`). It is bounded (500 sinks, 100 in
public mode, 50 notifications each, one hour).

```hurl
{{#include ../../examples/a2a/agents.hurl:push}}
```

```hurl
{{#include ../../examples/a2a/agents.hurl:push_ssrf}}
```

## Conformance

```bash
PYTHON=/path/to/venv/bin/python conformance/a2a/run.sh
```

checks every agent with the official SDK client over v1.0 and v0.3, JSON-RPC and
HTTP+JSON: card resolution, blocking and streaming sends, get, cancel, artifacts,
multi-turn, auth-required, the extended card and push notification CRUD and
delivery.
