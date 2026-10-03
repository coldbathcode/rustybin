# Guardrails and content safety

AI gateways often call an external safety service before forwarding a prompt or
after receiving a completion. Rustybin mocks such services with deterministic,
keyword-based detectors behind wire-compatible facades, so a guardrail plugin can
be configured against a fake service and the outcome is predictable.

| Route | Imitates |
|---|---|
| `POST /guardrails/azure/contentsafety/text:analyze` | Azure AI Content Safety text analysis |
| `POST /guardrails/azure/contentsafety/text:shieldPrompt` | Azure AI Content Safety Prompt Shields |
| `POST /guardrails/bedrock/guardrail/{id}/version/{version}/apply` | AWS Bedrock ApplyGuardrail |
| `POST /guardrails/check` | A generic check returning everything at once |
| `POST /guardrails/pii/redact` | A generic PII redaction service |

Any `api-version`, subscription key or (for Bedrock) signature is accepted; nothing
is checked.

## What is detected

Matching is case-insensitive and on whole words.

| Detector | Terms (severity on Azure's 0 to 7 scale) |
|---|---|
| Hate | hateful 2, bigot 3, racist 4, subhuman 6 |
| Self-harm | self-harm 4, hurt myself 4, suicide 6, kill myself 6 |
| Sexual | explicit 2, nsfw 4, porn 6 |
| Violence | fight 2, weapon 4, kill 4, bomb 6, murder 6 |
| Prompt injection / jailbreak | "ignore (all) previous instructions", "ignore the above", "ignore your instructions", "disregard previous/your instructions", "forget your instructions", "you are now DAN", "do anything now", "developer mode", "jailbreak", "pretend you have no restrictions", "reveal/print your system prompt", "bypass your safety" |
| Blocklist `demo-blocklist` | badword, blockedterm, forbidden phrase |
| PII | email addresses, phone numbers, US SSNs, credit card numbers (Luhn checked), IPv4 and IPv6 addresses |

To produce such content from the model side, use the mock LLM's
[scripted mode](behaviour.md#scripted-rules) (`customer record`, `toxic`,
`secrets`, ...).

## Generic check and redaction

`/guardrails/check` takes `{"text": ...}` and returns `flagged`, `action` (`block` when flagged, else `redact` when PII was
found, else `allow`), per-category severities and matches, `prompt_injection`, blocklist
matches, PII spans with offsets and `redacted_text`.

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:guardrail_check}}
```

`/guardrails/pii/redact` takes `{"text", "mode": "label" | "mask", "types": [...]}`
(types `EMAIL`, `PHONE`, `SSN`, `CREDIT_CARD`, `IP_ADDRESS`) and returns
`redacted_text`, `count` and `entities` with offsets. A `text/plain` body returns
just the redacted text (with `?mode=` and `?types=` as query parameters).

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:pii_redact}}
```

## Azure AI Content Safety

`text:analyze` takes `text`, `categories` (`Hate`, `SelfHarm`, `Sexual`,
`Violence`), `blocklistNames`, `haltOnBlocklistHit` and `outputType`
(`FourSeverityLevels` reports 0, 2, 4, 6; `EightSeverityLevels` 0 to 7) and
returns `categoriesAnalysis` and `blocklistsMatch` (every requested blocklist name
contains the demo items). `text:shieldPrompt` takes `userPrompt` and `documents`
and returns `userPromptAnalysis.attackDetected` and
`documentsAnalysis[].attackDetected`.

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:azure_content_safety}}
```

## AWS Bedrock ApplyGuardrail

The body is `{"source": "INPUT" | "OUTPUT", "content": [{"text": {"text": ...}}]}`.
Harmful keywords, blocklist words and (for `INPUT`) prompt attacks block the
content: `action: GUARDRAIL_INTERVENED` with a canned output message. PII alone is
anonymized in the outputs (`{EMAIL}`, `{PHONE}`, `{US_SOCIAL_SECURITY_NUMBER}`,
`{CREDIT_DEBIT_CARD_NUMBER}`, `{IP_ADDRESS}`); guardrail ids containing `block-pii`
block PII instead. Assessments include `contentPolicy`, `wordPolicy`,
`sensitiveInformationPolicy` and `invocationMetrics`.

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:bedrock_guardrail}}
```
