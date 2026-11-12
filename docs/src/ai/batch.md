# Batch APIs

Batch jobs are the other shape AI gateways now front: upload a file of requests,
poll, download the results. The mock LLM serves the OpenAI-shaped files and batches
API and the Anthropic-shaped message batches API, with the engine's deterministic
answers, on a clock compressed from a day to seconds.

| OpenAI shape | |
|---|---|
| `POST /ai/openai/v1/files` | Upload an input file (multipart: `purpose=batch`, `file` as JSONL) |
| `GET /ai/openai/v1/files`, `GET`, `DELETE /ai/openai/v1/files/{id}` | Your files |
| `GET /ai/openai/v1/files/{id}/content` | A file's content: your input, or a batch's output or error file |
| `POST /ai/openai/v1/batches` | Start a batch from an input file |
| `GET /ai/openai/v1/batches`, `GET /ai/openai/v1/batches/{id}` | Your batches |
| `POST /ai/openai/v1/batches/{id}/cancel` | Cancel one |

| Anthropic shape | |
|---|---|
| `POST /ai/anthropic/v1/messages/batches` | Start a batch (`requests`, each a `custom_id` and `params`) |
| `GET /ai/anthropic/v1/messages/batches`, `GET .../batches/{id}` | Your batches |
| `GET .../batches/{id}/results` | The results as JSONL, once it ended |
| `POST .../batches/{id}/cancel` | Cancel one |

## A batch

The input file has one request a line:

```json
{{#include ../../examples/ai/batch-requests.jsonl}}
```

```hurl
{{#include ../../examples/ai/batch.hurl:upload}}
```

```hurl
{{#include ../../examples/ai/batch.hurl:create}}
```

A batch moves through the provider's states: `validating`, `in_progress`,
`finalizing`, `completed`. Poll it:

```hurl
{{#include ../../examples/ai/batch.hurl:poll}}
```

```hurl
{{#include ../../examples/ai/batch.hurl:output}}
```

Each line of the output file is `{id, custom_id, response: {status_code, request_id,
body}, error}`, the body a chat completion from the [engine](behaviour.md): a model
name or `X-Rustybin-Mode` on the batch request picks the mode for its lines.

**The results are deterministic for the same input file**, ids included: two batches
of one file have the same output, byte for byte. A gateway that caches batch results
can be shown to do so.

Only `/v1/chat/completions` lines are run; another `endpoint` is refused.

## The clock

`X-Rustybin-Batch-Seconds` on the create request is the whole life of the batch
(default 10, the `ai.batch.seconds` setting; at most 3600; `0` is done at once):

| Share of the life | State | `request_counts` |
|---|---|---|
| The first tenth | `validating` | nothing done |
| Up to eight tenths | `in_progress` | lines done evenly |
| The rest | `finalizing` | every line done |
| After it | `completed` | `output_file_id` and `error_file_id` are set |

The timestamps of a batch (`in_progress_at`, `finalizing_at`, `completed_at`) follow
the same clock.

## Lines that fail, and files that do not validate

`X-Rustybin-Batch-Fail-Rate` (a share from 0 to 1, or a percent) makes lines fail
with the provider's error; `X-Rustybin-Batch-Fail` picks which (`500` by default,
`429`, `503`, `529`, `timeout`). The lines are chosen by their `custom_id`, so the same
file fails the same lines every time. Failed lines are in the error file:

```hurl
{{#include ../../examples/ai/batch.hurl:failing}}
```

A file with a line that is not JSON, has no `custom_id` (or one used twice), names
another URL than the batch's endpoint or holds a request the chat API would refuse
fails validation: the batch ends `failed` after its validating share, with the
provider's `errors` list naming each line. An empty file fails the same way.

`X-Rustybin-Batch-Outcome: expired` lets a batch run out of time: `in_progress` until
its life is over, then `expired` with half its lines done and in the output file.

## Cancel

```hurl
{{#include ../../examples/ai/batch.hurl:cancel}}
```

A cancelled batch is `cancelling` for a tenth of its life, then `cancelled`; the
lines done before the cancel are in its output file. A batch that already ended
stays as it ended.

## The Anthropic shape

```hurl
{{#include ../../examples/ai/batch.hurl:anthropic}}
```

A message batch is `in_progress` for its life, then `ended` (`canceling` in between
when cancelled). Its `request_counts` are `processing`, `succeeded`, `errored`,
`canceled` and `expired`. The results are one line a request: `succeeded` with the
message, `errored` with the provider's error (a request whose `params` are wrong is
its own errored result, as above; so is a line chosen by the fail rate), `canceled`
or `expired` for what was not done. The batch headers above apply. Asking for the
results before the batch ended is a `400`.

## What the mock saw

A batch is one record of the [request log](gateway-features.md#request-inspection),
`kind: batch`, under the create request's `X-Request-Id`: the tokens of all its lines
as `usage`, and `detail.lines` with each line's `custom_id`, outcome, finish reason,
tokens and the start of its text (at most 200 lines).

```hurl
{{#include ../../examples/ai/batch.hurl:log}}
```

```hurl
{{#include ../../examples/ai/batch.hurl:cleanup}}
```

## Limits

Files and batches belong to their caller: the session (`X-Rustybin-Session`, else the
client address), and the API key in shared mode. Another caller gets `404`.

| | |
|---|---|
| Files a caller | 20 (`ai.batch.max_files`), 8 MiB each and never more than the instance's body limit (`RUSTYBIN_BODY_LIMIT`, 1 MiB by default) |
| Batches a caller | 10 (`ai.batch.max_batches`); one more is refused with the provider's rate limit error |
| Requests a batch | 1000 (100 on a public instance) |
| Kept | An hour after the caller's last use |
| The instance | 256 callers and 256 MiB of files (64 and 32 MiB on a public instance) |

## On every surface

| Surface | Where |
|---|---|
| API | The routes above; a batch in `GET /ai/requests` and `/ai/requests/{id}` |
| Console | **AI playground**, **Request log**: a batch shows its lines |
| CLI and management MCP | Nothing to configure: this is the data plane. The caps are settings (`rustybin settings set ai.batch.seconds 30`, `update_settings`) |
| `rustybin.yaml` | `settings:` with `ai.batch.max_files`, `ai.batch.max_batches` and `ai.batch.seconds` |
