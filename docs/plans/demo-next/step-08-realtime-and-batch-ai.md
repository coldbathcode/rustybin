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

## Build notes (2026-11-12)

Built whole, with the recommendation of section 6: the OpenAI-shaped realtime protocol, and batch
in the OpenAI and Anthropic shapes. Where the build differs from the plan above:

- **There is no `rustybin ai requests` command and no AI request tool**: `/ai/*` is the data
  plane, and neither surface had a reader of the request log before this step. The log is read
  over HTTP (`GET /ai/requests`) and in the console; the CLI and the management MCP reach this
  step through its settings only. The plan's rows for those two surfaces assumed commands that
  were never there.
- **The request log gained `kind` and `detail`** (`AiRecord`): `request`, `realtime` or `batch`.
  A realtime session is one record under the upgrade's request id, written again after each
  response (`RequestStore::replace`), so its `usage` is the session's total and equals the sum
  of the `response.done` usages. A batch is one record written when it is created.
- **Realtime pacing**: the query (`ttft_ms`, `tokens_per_second`), the headers of the other AI
  paths on the upgrade, or `session.update` with a `rustybin` object.
- **Realtime faults**: `?fail=drop_after:N` and also `?fail=error_after:N` (an `error` event
  after the Nth server event, the session goes on). `ai_layer` does not know these two forms
  and leaves them to the session; a fail kind it knows refuses the upgrade there.
- **A cancel** is seen while a response is paced (the deltas wait on the socket): with
  `tokens_per_second=0` a response is sent before a cancel can arrive, and the cancel is then
  answered with `response_cancel_not_active`, as the provider does.
- **Usage**: input is the engine's prompt tokens plus ten tokens a second of committed input
  audio; output is the engine's completion tokens plus two tokens for each 100 ms audio delta.
- **Batch headers**: the fail rate and kind of a batch's lines are `X-Rustybin-Batch-Fail-Rate`
  and `X-Rustybin-Batch-Fail`, since `X-Rustybin-Fail` already fails the create request itself.
  `X-Rustybin-Batch-Outcome: expired` makes the `expired` state reachable.
- **Batch results are made when the batch is created** and shown as the clock allows, so the
  states cost nothing while a batch waits. The clock is the limiter's (`Limiter::clock`, the
  injectable `limits::Clock`); the phases are also a pure function (`Batch::at`), tested without
  waiting.
- **Only `/v1/chat/completions` batches** in the OpenAI shape (another endpoint is a `400`).
  Validation problems end the batch `failed` after its validating share; a missing file or a
  wrong endpoint is refused at once.
- **Bounds beyond the plan's**: 256 callers and 256 MiB of files for the instance (64 and 32 MiB
  in public mode), 1000 requests a batch (100 in public mode). A file is at most 8 MiB and never
  more than the instance's body limit, which is 1 MiB by default.
- **A message batch's `results_url` is a path**: the mock does not know the address a gateway
  serves it at.
- **The caps are settings** of the group `ai`: `ai.realtime.max_sessions`,
  `ai.realtime.session_minutes`, `ai.batch.max_files`, `ai.batch.max_batches`,
  `ai.batch.seconds` (`ai::specs`).
- The tests were written with the code: `src/ai/realtime.rs` (the plan's three plus audio, the
  G.711 encoders, a cancel and the cap, against a real WebSocket server) and `src/ai/batch.rs`
  (the plan's two plus the phases, the Anthropic shape and the instance's bounds on a manual
  clock); `docs/examples/ai/batch.hurl` and `docs/examples/ai/realtime.sh` (websocat).

### For the merge

`src/ai/realtime.rs` and `src/ai/batch.rs`, `AiRecord.kind` and `detail` with
`RequestStore::replace` and `AiCtx::record_detail`, three fields of `AiShared` (`settings`,
`realtime`, `batches`), `ai::specs` in `settings::specs`, `ui/views/ai/realtime.js` and two cards
in `ui/views/ai.js`.
