# Step 08: Realtime and batch AI APIs

## Goal

AI gateways now front two more kinds of provider API: realtime sessions over WebSocket (voice and
text, events both ways) and batch jobs (upload a file of requests, poll, download results). The AI
mocks gain both, with the same deterministic answers, faults and inspection as the request and
response APIs.

## Where things stand

- `src/ai/` imitates seven providers' request and response APIs at stable paths with streaming (SSE,
  NDJSON, the AWS event stream), deterministic tokens, native faults and the request log.
- `src/websocket.rs` serves WebSocket demo routes; nothing speaks a realtime AI protocol.
- No batch or files API is imitated.

## Model

### Part A: realtime

- `GET /ai/openai/v1/realtime` (WebSocket upgrade, `?model=`), the OpenAI-shaped realtime protocol:
  client events `session.update`, `input_audio_buffer.append | commit | clear`,
  `conversation.item.create`, `response.create`, `response.cancel`; server events `session.created`,
  `session.updated`, `input_audio_buffer.committed`, `conversation.item.created`, `response.created`,
  `response.output_item.added`, `response.text.delta` and `response.audio.delta`,
  `response.audio_transcript.delta`, `response.done` with usage, `rate_limits.updated`, `error`.
- Answers come from the existing engine (`src/ai/engine.rs`: modes, scripted rules, tool calls with
  arguments from the session's tool schemas), paced by the existing headers' equivalents in
  `session.update` (`X-Rustybin-TTFT-Ms` and tokens per second as query parameters too).
- Audio: input audio is accepted and counted (bytes to duration to tokens), never decoded or kept;
  output audio is valid PCM16 or G.711 frames of silence or a tone, the transcript carries the text.
- Faults: a fail kind on the upgrade (`?fail=rate_limit`) answers the provider's refusal; mid-session
  `error` events and a dropped connection at a chosen point (`?fail=drop_after:N`).
- Credential checks as for the other paths; the request log records each session's events
  (normalised, bounded, no audio).
- Bounded: sessions capped (64, 30 minutes, a message size cap), counted in metrics by
  `metrics::open_ws`.

### Part B: batch

- Files: `POST /ai/openai/v1/files` (purpose `batch`, JSONL), `GET .../files/{id}`,
  `GET .../files/{id}/content`, `DELETE`. Batches: `POST /ai/openai/v1/batches`, `GET .../batches`,
  `GET .../batches/{id}`, `POST .../batches/{id}/cancel`. Anthropic shape:
  `POST /ai/anthropic/v1/messages/batches`, `GET .../batches/{id}`, `.../results`, `.../cancel`.
- A batch moves through the provider's states (`validating`, `in_progress`, `finalizing`,
  `completed`, `failed`, `expired`, `cancelling`, `cancelled`) on a compressed clock
  (`X-Rustybin-Batch-Seconds`, default 10 seconds for the whole batch) on the injectable
  `limits::Clock`; results are the engine's deterministic answers per line; a fail rate makes chosen
  lines fail with native errors; a malformed line fails validation with the provider's error.
- Bounded: per session 20 files of 8 MiB and 10 batches, an hour, then evicted.

## Surfaces

| Surface | What changes |
|---|---|
| API | The new data plane routes in the catalogue (`.websocket()` for realtime) and OpenAPI; `/ai/requests` shows realtime sessions and batch jobs |
| Console | The AI view's request log renders a realtime session's event timeline and a batch's lines; a Try panel opens a text realtime session |
| Management MCP | Nothing new to configure (data plane); the AI request tools return the new kinds under `untrusted_*` |
| CLI | `rustybin ai requests` shows the new kinds |
| `rustybin.yaml` | The `ai.realtime.*` and `ai.batch.*` caps as settings |

Docs: `docs/src/ai/realtime.md` and `docs/src/ai/batch.md`; Hurl examples for batch (Hurl has no
WebSocket support: realtime is shown with a small script under `docs/examples/ai/` run by
`docs/examples/run.sh`).

## Owner decision (master plan section 6)

OpenAI-shaped realtime first; batch for the OpenAI and Anthropic shapes. Other providers' realtime
APIs when asked.

## Rules

- Audio is never decoded or stored; a session's log holds event types, sizes and text only.
- Batch results are deterministic for the same input file, so a gateway's caching of batch results
  can be shown.

## Tests that fail first

- A realtime session: `session.created`, a `response.create`, text deltas in order, `response.done`
  with usage equal to the request log's.
- A tool schema in `session.update` gives a function call with valid arguments.
- `drop_after:3` closes after the third server event.
- A batch of three lines completes in the compressed time; one line with a fail rate of 1 fails with
  the native error; cancel moves through `cancelling` to `cancelled`.
- Caps: the 65th session and the 11th batch are refused with the provider's error.

## Done when

Both APIs are served, faulted and inspected like the rest of the AI mocks, documented with tested
examples.
