# Behaviour: modes, rules, tools, structured output

Every provider front end turns its native request into the same internal form
(system prompt, messages with content parts, tools, tool choice, response format,
limits), generates a reply, and renders it back in the native shape. So the rules
below apply to all providers.

How a reply is chosen, in order:

1. **Echo mode**: the rendered prompt, exactly as received.
2. The last messages carry **tool results**: a final answer quoting them.
3. Tools are supplied and the tool choice is not `none`: a **tool call** when a tool
   is forced, or when the last user message mentions a tool.
4. **Structured output** requested: JSON valid against the schema.
5. Otherwise **text** from the mode: `canned`, `scripted` or `random`.

Then stop sequences and the token limit are applied.

## Modes

The mode comes from the `X-Rustybin-Mode` header, else from a segment of the model
name (split on `-`, `_`, `:`, `/`, `.`, `@`): `rustybin-echo`, `gpt-4o:scripted`,
`random`. The default is `canned`. The response reports it in `X-Rustybin-Mode`.

| Mode | Reply |
|---|---|
| `canned` (also `default`) | A keyword table on the last user message (whole words): greetings (`hello`, `hi`, `hey`, `greetings`, `howdy`), code (`code`, `python`, `function`, `program`, `script`), JSON (`json`, `data`), long text (`long`, `essay`, `explain`, `article`), else a default paragraph |
| `echo` | The prompt the upstream received, one line per part: `system: ...`, `tools: ...`, `user: ...`, `assistant: [tool_call name] {...}`, `tool (name): ...`; images, audio and files appear as placeholders |
| `scripted` (also `script`) | The demo rules below |
| `random` | Sentences from a gateway-themed vocabulary, seeded by a hash of the model and prompt: identical requests get identical text, `n > 1` choices differ |

Echo mode is the tool for prompt decorator, template and guard demos: what the
gateway prepended, appended or rewrote is right there in the reply.

```hurl
{{#include ../../examples/ai/behaviour.hurl:mode_echo}}
```

```hurl
{{#include ../../examples/ai/behaviour.hurl:mode_model_name}}
```

```hurl
{{#include ../../examples/ai/behaviour.hurl:canned}}
```

```hurl
{{#include ../../examples/ai/behaviour.hurl:random}}
```

## Scripted rules

Checked in order on the last user message (whole words or phrases,
case-insensitive):

| Trigger | Reply |
|---|---|
| `lorem N` | N lorem ipsum words (default 50, at most 4000; 500 in public mode) |
| `ssn`, `social security`, `credit card`, `pii`, `customer record` | A fake customer record: SSN `123-45-6789`, card `4111 1111 1111 1111`, email, phone, address (response sanitiser demos) |
| `toxic`, `jailbreak`, `unsafe`, `harmful` | A clearly labelled simulated unsafe answer an output guardrail should block |
| `secret`, `secrets`, `api key`, `password`, `credentials` | Fake cloud keys and tokens (secret redaction demos) |
| `refuse`, `refusal` | "I'm sorry, but I can't help with that request." |
| `json` | A bare JSON object |
| `markdown`, `table` | Markdown with a table and links |
| `url`, `urls`, `link`, `links` | Text with allowed and suspicious URLs (URL filter demos) |
| `echo` | The rendered prompt, like echo mode |
| anything else | The canned reply |

```hurl
{{#include ../../examples/ai/behaviour.hurl:scripted_pii}}
```

```hurl
{{#include ../../examples/ai/behaviour.hurl:scripted_lorem}}
```

## Tokens and limits

Token counts are deterministic and shared by every provider: a run of letters or
digits costs one token per four characters (at least one), every other
non-whitespace character costs one, whitespace attaches to the next piece. Images
cost 85 tokens; prompt tokens count the rendered prompt plus the tool definitions.
The same count drives the usage fields, the rate-limit headers, streaming (one
token per delta, so `completion_tokens` equals the number of text deltas) and the
output limit (`max_tokens`, `max_completion_tokens`, `max_output_tokens`,
`maxOutputTokens`, `maxTokens`, `num_predict`), which ends the reply with the
provider's "length" finish reason (`length`, `max_tokens`, `MAX_TOKENS`,
`incomplete`). Stop sequences cut the text and report the sequence where the
provider does.

```hurl
{{#include ../../examples/ai/behaviour.hurl:max_tokens_stop}}
```

## Tool calls

When tools are supplied and the tool choice is not `none`, the reply is a tool call
if:

- a specific tool is forced (OpenAI `{"type": "function", ...}`, Anthropic
  `{"type": "tool"}`, Gemini `ANY` with a single `allowedFunctionNames` entry), or any tool is
  required (OpenAI `required`, Anthropic `any`, Gemini `ANY`), in which case the best
  matching tool or else the first one is called; or
- the last user message mentions a tool: its full name (`get weather` for
  `get_weather` or `getWeather`), a significant part of the name, or a significant
  word of its description (whole words; common words such as "get", "data" or
  "tool" do not count). The best scoring tool wins.

One tool call is returned per reply. Its arguments are generated from the tool's
JSON schema: required and optional properties, enums (the first value), nested
objects and arrays, `$ref` into `$defs` / `definitions`, `anyOf` / `oneOf` /
`allOf`, string formats and bounds, number bounds, `const` and `default`. String
values are chosen from the property name (`email`, `city`, `date`, ...) and the
user text, so `location` becomes "Paris" for "What is the weather in Paris?".

```hurl
{{#include ../../examples/ai/behaviour.hurl:tool_call}}
```

After the client sends the tool result back, the model answers by quoting it:

```hurl
{{#include ../../examples/ai/behaviour.hurl:tool_result}}
```

```hurl
{{#include ../../examples/ai/behaviour.hurl:tool_anthropic}}
```

Streaming tool calls use each provider's native deltas (OpenAI `tool_calls` deltas,
Anthropic `input_json_delta`, Responses `response.function_call_arguments.delta`).

## Structured output

A JSON value valid against the requested schema (best effort) is returned for:
OpenAI `response_format` (`json_schema`, or `json_object` for any JSON object),
Responses `text.format`, Gemini `responseSchema` / `responseJsonSchema` (with
`responseMimeType: application/json`; Gemini's upper-case type names work),
Anthropic `output_format` or a forced tool, and Ollama `format`.

```hurl
{{#include ../../examples/ai/behaviour.hurl:structured_openai}}
```

```hurl
{{#include ../../examples/ai/behaviour.hurl:structured_gemini}}
```
