# Mock LLM conformance

Scripts that drive Rustybin's mock LLM (`/ai/*`) with the official client
SDKs, to prove real clients accept the responses.

```bash
# needs: openai, anthropic, google-genai, httpx, pydantic
PYTHON=/path/to/venv/bin/python conformance/ai/run.sh
```

`run.sh` builds the binary, starts two instances (`:18300` with default
settings and `:18301` with `RUSTYBIN_AI_API_KEY=conformance-secret` for the
expected-key checks; override the base port with `RUSTYBIN_CONFORMANCE_PORT`),
runs every script and stops the servers. A script can also be run alone
against a running instance: `RUSTYBIN_URL=http://127.0.0.1:8080 python test_openai.py`
(`RUSTYBIN_AUTH_URL` is optional).

| Script | Client | Covers |
|---|---|---|
| `test_openai.py` | `openai` | chat (content parts, n, streaming with `include_usage`, stream helper), tools and tool results (streamed tool call deltas), `.parse()` structured output, `json_object`, `max_tokens`, modes, Responses API (create, stream events, function calls, parse), completions, embeddings (SDK default base64, float, dimensions), models, moderations, images, audio transcription, `AzureOpenAI`, native errors (429, context length, auth, content filter) |
| `test_anthropic.py` | `anthropic` | messages, system blocks with `cache_control` (cache creation then read), max_tokens / stop sequences, image blocks, streaming (`text_stream`, event order), tool use with `input_json_delta`, tool_result round trip, forced tool, count_tokens, models, `OverloadedError` (529), `RateLimitError`, auth errors |
| `test_gemini.py` | `google-genai` (`http_options.base_url`) | generateContent, streamGenerateContent (SSE), system instruction, MAX_TOKENS, echo mode, function calling (manual and automatic), `response_schema` with a pydantic model, countTokens, embed_content (batchEmbedContents), models, RESOURCE_EXHAUSTED |
| `test_http_providers.py` | `httpx` | Bedrock Converse with a real SigV4 header, ConverseStream and InvokeModelWithResponseStream decoded by an independent event stream parser (CRC32 checked with `zlib.crc32`), Ollama NDJSON, Cohere rerank / embed, `/ai/requests/{id}`, latency and TTFT headers |

Skipped: boto3 / the AWS SDK and the Ollama and Cohere SDKs are not in the
conformance venv; their wire formats are covered by `test_http_providers.py`
instead.
