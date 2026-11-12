# Realtime API

AI gateways front realtime sessions too: one WebSocket, JSON events both ways, text
and audio. The mock LLM speaks the OpenAI-shaped realtime protocol at

```
GET /ai/openai/v1/realtime?model=<model>      (WebSocket upgrade)
```

with the same deterministic answers as the [other AI paths](behaviour.md): the
modes, the scripted rules and the tool calls come from the same engine.

## A session

A session starts with `session.created`. Send events as JSON text frames:

```bash
{{#include ../../examples/ai/realtime.sh:session}}
```

| Client event | Does | Answered with |
|---|---|---|
| `session.update` | Sets `instructions`, `tools`, `tool_choice`, `modalities` (`["text"]` or `["audio", "text"]`), `voice`, `input_audio_format`, `output_audio_format`, `max_response_output_tokens` | `session.updated` |
| `conversation.item.create` | Adds a `message` (content `input_text` or `input_audio`) or a `function_call_output` | `conversation.item.created` |
| `input_audio_buffer.append` | Adds base64 audio to the buffer | nothing |
| `input_audio_buffer.commit` | Makes the buffer a user message | `input_audio_buffer.committed`, `conversation.item.created` |
| `input_audio_buffer.clear` | Empties the buffer | `input_audio_buffer.cleared` |
| `response.create` | Asks for a response to the conversation | The response events below |
| `response.cancel` | Stops the response being sent | `response.done` with `status: cancelled` |

A response is `response.created`, `response.output_item.added`,
`conversation.item.created`, `response.content_part.added`, then the deltas:
`response.text.delta` for a text session, `response.audio_transcript.delta` and
`response.audio.delta` for an audio one. Then the matching `.done` events,
`response.output_item.done`, `response.done` with the `usage`, and
`rate_limits.updated`. Every server event has an `event_id`.

An event the session does not know, or a frame that is not a JSON object, is answered
with an `error` event (`invalid_request_error`) and the session goes on.

## Tools

Tools given in `session.update` are called as on the chat paths: when the user's
text is about the tool, the response is a `function_call` item whose arguments are
valid for the tool's schema.

```bash
{{#include ../../examples/ai/realtime.sh:tool}}
```

Send the result back as a `function_call_output` item with the call's `call_id`, then
`response.create` again for the text answer.

## Audio

**Audio is never decoded or kept.** Input audio is counted: its bytes give a duration
in the session's `input_audio_format` (`pcm16` at 24 kHz, `g711_ulaw` or `g711_alaw`
at 8 kHz), and the duration gives tokens (ten a second), reported as
`usage.input_token_details.audio_tokens`.

Output audio is valid frames in the session's `output_audio_format`: 100 ms of
silence a delta, or a 440 Hz tone with `?audio=tone`. The transcript deltas carry the
text the engine answered. A gateway that meters, records or transforms audio has real
frames to work on, and nothing that sounds like speech.

## Pacing and faults

| | |
|---|---|
| `?ttft_ms=`, `?tokens_per_second=` | The pace of a response's deltas. `X-Rustybin-TTFT-Ms` and `X-Rustybin-Tokens-Per-Second` on the upgrade do the same, and so does `session.update` with `"rustybin": {"ttft_ms": 200, "tokens_per_second": 20}` |
| `?fail=<kind>` (or `X-Rustybin-Fail`) | Refuses the upgrade with the provider's error, as on every AI path: `429`, `503`, `auth` |
| `?fail=drop_after:N` | Drops the connection after the Nth server event, without a close frame |
| `?fail=error_after:N` | Sends an `error` event (`server_error`) after the Nth server event; the session goes on |

```bash
{{#include ../../examples/ai/realtime.sh:refused}}
```

```bash
{{#include ../../examples/ai/realtime.sh:drop}}
```

Credentials are checked on the upgrade as on the other OpenAI paths
(`Authorization: Bearer`, enforced with `X-Rustybin-Require-Auth` or
`RUSTYBIN_AI_REQUIRE_AUTH`), and the answer carries `X-Rustybin-Credential`.

## What the mock saw

A session is one record of the [request log](gateway-features.md#request-inspection),
`kind: realtime`, under the upgrade's `X-Request-Id`: the totals of its responses as
`usage`, and `detail.events`, each event's direction, type, size and text (the text
of a delta, of a message, of an error), at most 200. Never audio.

```bash
{{#include ../../examples/ai/realtime.sh:log}}
```

## Limits

| | |
|---|---|
| Sessions open at once | 64 (the `ai.realtime.max_sessions` setting, 1 to 256); one more is refused with the provider's `429` |
| A session | 30 minutes (`ai.realtime.session_minutes`), and closed after 5 minutes without a client event |
| A client event | 512 KiB |
| A conversation | 200 items; the oldest go first |

## On every surface

| Surface | Where |
|---|---|
| API | `GET /ai/openai/v1/realtime` (WebSocket); the session in `GET /ai/requests` and `/ai/requests/{id}` |
| Console | **AI playground**: **Realtime session** (connect, say something, the reply and the events as a timeline) and the **Request log**, where a realtime session shows its events |
| CLI and management MCP | Nothing to configure: this is the data plane. The caps are settings (`rustybin settings set ai.realtime.max_sessions 16`, `update_settings`) |
| `rustybin.yaml` | `settings:` with `ai.realtime.max_sessions` and `ai.realtime.session_minutes` |
