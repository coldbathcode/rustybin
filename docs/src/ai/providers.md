# Providers and paths

Point a client SDK or a gateway's upstream at these base URLs (`HOST` is your
Rustybin, for example `localhost:8080`):

| Provider | Base URL | Routes below the base URL |
|---|---|---|
| OpenAI | `http://HOST/ai/openai/v1` (legacy alias `http://HOST/ai/v1`) | `chat/completions`, `completions`, `embeddings`, `models`, `models/{model}`, `moderations`, `images/generations`, `audio/transcriptions`, `responses` |
| Azure OpenAI | `http://HOST/ai/azure` (the SDK's `azure_endpoint`) | `openai/deployments/{deployment}/chat/completions`, `.../completions`, `.../embeddings`, all with `?api-version=` |
| Anthropic | `http://HOST/ai/anthropic` | `v1/messages`, `v1/messages/count_tokens`, `v1/models`, `v1/models/{model}` |
| Gemini | `http://HOST/ai/gemini` (`http_options.base_url`, API version `v1beta`) | `v1beta/models`, `v1beta/models/{model}` and `{model}:generateContent`, `:streamGenerateContent` (`?alt=sse` for SSE, else a streamed JSON array), `:countTokens`, `:embedContent`, `:batchEmbedContents` |
| AWS Bedrock Runtime | `http://HOST/ai/bedrock` | `model/{modelId}/converse`, `/converse-stream`, `/invoke`, `/invoke-with-response-stream` |
| Ollama | `http://HOST/ai/ollama` | `api/chat`, `api/generate`, `api/tags`, `api/embed`, `api/embeddings` |
| Cohere | `http://HOST/ai/cohere` | `v2/rerank`, `v2/embed` (plus the generic `http://HOST/ai/v1/rerank`) |

Any model name is accepted for generation: the name is echoed back, and a name
segment can select a [mode](behaviour.md#modes). The `models` endpoints list a
few well-known names (`gpt-4o`, `claude-sonnet-4-5`, `gemini-2.5-flash`,
`llama3.2`, ...) plus `rustybin-*` mode models; retrieving an unlisted model by id
answers the provider's native 404.

For example, with the Python SDKs:

```python
OpenAI(base_url="http://localhost:8080/ai/openai/v1", api_key="anything")
AzureOpenAI(azure_endpoint="http://localhost:8080/ai/azure", api_key="anything", api_version="2024-10-21")
anthropic.Anthropic(base_url="http://localhost:8080/ai/anthropic", api_key="anything")
genai.Client(api_key="anything", http_options=types.HttpOptions(base_url="http://localhost:8080/ai/gemini", api_version="v1beta"))
```

Credentials are not checked unless you ask for it, see
[credential checks](gateway-features.md#credential-checks).

Every provider response (all `/ai/*` routes except `/ai/requests`) carries `X-Rustybin-Request-Id`, `X-Rustybin-Provider`,
`X-Rustybin-Instance`, `X-Rustybin-Credential` and, for generation endpoints,
`X-Rustybin-Model`, `X-Rustybin-Mode` and success rate-limit headers, see
[Faults, latency, credentials, inspection](gateway-features.md).

## OpenAI

Chat Completions supports `n` (up to 8, 2 in public mode), `max_tokens` /
`max_completion_tokens`, `stop`, content parts (text, images, audio, files),
`tools` / `tool_choice` (one tool call per reply), `response_format`
(`json_object`, `json_schema`), and streaming with `stream_options.include_usage`.
Rate-limit headers: `x-ratelimit-limit-requests`, `x-ratelimit-limit-tokens`,
`x-ratelimit-remaining-*`, `x-ratelimit-reset-*`.

```hurl
{{#include ../../examples/ai/openai.hurl:chat}}
```

```hurl
{{#include ../../examples/ai/openai.hurl:chat_stream}}
```

```hurl
{{#include ../../examples/ai/openai.hurl:legacy_alias}}
```

Legacy completions support `stream`, `echo`, `suffix` and `n`:

```hurl
{{#include ../../examples/ai/openai.hurl:completions}}
```

The Responses API returns `output` items (a `message` with `output_text`, or
`function_call` items); `text.format` gives structured output; streaming emits the
native event sequence (`response.created`, `response.in_progress`,
`response.output_item.added`, `response.content_part.added`,
`response.output_text.delta`..., `response.completed`, or `response.incomplete`
when `max_output_tokens` cut the text).

```hurl
{{#include ../../examples/ai/openai.hurl:responses}}
```

```hurl
{{#include ../../examples/ai/openai.hurl:responses_stream}}
```

Models, moderations (keyword categories with deterministic scores), image
generation (a tiny valid PNG as `b64_json`, or a URL to `/image/png`) and audio
transcription (multipart; `json`, `text`, `srt`, `vtt` or `verbose_json`; the audio
is not decoded: the transcript is a fixed sentence and the duration is estimated
from the file size):

```hurl
{{#include ../../examples/ai/openai.hurl:models}}
```

```hurl
{{#include ../../examples/ai/openai.hurl:moderations}}
```

```hurl
{{#include ../../examples/ai/openai.hurl:images}}
```

```hurl
{{#include ../../examples/ai/openai.hurl:transcription}}
```

## Azure OpenAI

The same engine and payloads as OpenAI, with the Azure specifics: `api-version` is
required (`400` without it), the deployment name is the model, the credential is
the `api-key` header, and responses carry `prompt_filter_results` and per-choice
`content_filter_results`.

```hurl
{{#include ../../examples/ai/providers.hurl:azure}}
```

## Anthropic

Messages with `system` as a string or text blocks (with `cache_control`), content
blocks (text, image, document, `tool_use`, `tool_result`), `tools` with
`tool_choice` (`auto`, `any`, `tool`, `none`), `output_format` JSON schema,
`stop_sequences`, and the native SSE sequence (`message_start`,
`content_block_start`, `ping`, `content_block_delta` with `text_delta` /
`input_json_delta`, `content_block_stop`, `message_delta`, `message_stop`).
`max_tokens` is required, like the real API. Responses carry `request-id` and
`anthropic-ratelimit-*` headers.

```hurl
{{#include ../../examples/ai/providers.hurl:anthropic}}
```

```hurl
{{#include ../../examples/ai/providers.hurl:anthropic_stream}}
```

**Prompt caching** is simulated: the prefix up to the last `cache_control` block is
hashed; the first request reports it as `cache_creation_input_tokens`, repeats
within five minutes from the same session and model as `cache_read_input_tokens`.

```hurl
{{#include ../../examples/ai/providers.hurl:anthropic_cache}}
```

```hurl
{{#include ../../examples/ai/providers.hurl:anthropic_errors}}
```

## Gemini

`systemInstruction`, `contents`, `tools[].functionDeclarations` (replies with
`functionCall` parts, accepts `functionResponse` parts), `toolConfig` (`AUTO`,
`ANY`, `NONE`, `allowedFunctionNames`), `generationConfig` (`maxOutputTokens`,
`stopSequences`, `candidateCount`, `responseMimeType` with `responseSchema` or
`responseJsonSchema`) and `usageMetadata`. Credentials: `x-goog-api-key` or
`?key=`.

```hurl
{{#include ../../examples/ai/providers.hurl:gemini}}
```

```hurl
{{#include ../../examples/ai/providers.hurl:gemini_stream}}
```

```hurl
{{#include ../../examples/ai/providers.hurl:gemini_models}}
```

## AWS Bedrock

Converse and ConverseStream (`application/vnd.amazon.eventstream` binary frames
with CRC32 checksums: `messageStart`, `contentBlockStart`, `contentBlockDelta`,
`contentBlockStop`, `messageStop`, `metadata`), InvokeModel with the model family's
native body (Anthropic `anthropic_version` Messages body, Titan `inputText` text or
embeddings, Llama-style `prompt`), and InvokeModelWithResponseStream (`chunk`
events with base64 JSON). Responses carry `x-amzn-requestid`.

```hurl
{{#include ../../examples/ai/providers.hurl:bedrock}}
```

```hurl
{{#include ../../examples/ai/providers.hurl:bedrock_stream}}
```

```hurl
{{#include ../../examples/ai/providers.hurl:bedrock_invoke}}
```

## Ollama

`chat` and `generate` stream newline-delimited JSON by default (like Ollama; send
`"stream": false` for one object), with `tools` and `format` (JSON or a schema).
`tags` lists local models; `embed` returns 768 dimensions by default; `embeddings`
is the legacy single-prompt form.

```hurl
{{#include ../../examples/ai/providers.hurl:ollama}}
```

## Cohere

`v2/rerank` and `v2/embed` (`float`, `int8`, `uint8`, `base64` embedding types), and
the generic `/ai/v1/rerank` alias for Jina or Voyage style clients: see
[Embeddings and rerank](embeddings.md).
