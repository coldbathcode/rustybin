# Orchestration pipeline

Four `POST` steps of a fake payment flow, for gateway request chaining, workflow
and API composition demos: each step needs something the previous one produced.
The module keeps no state, so steps can be called independently.

| Step | Requires | Produces |
|---|---|---|
| `POST /orchestration/step/1` (authenticate) | a non-empty `X-Api-Key` (else `401`) | `correlation_id`, merchant details, `permissions` |
| `POST /orchestration/step/2` (enrich) | `X-Correlation-Id` | `enrichment.risk_score` (0 to 99), risk level, card details, velocity check |
| `POST /orchestration/step/3` (validate) | `X-Correlation-Id` | `validation.result`: `approved`, or `declined` when a rule fails |
| `POST /orchestration/step/4` (process) | `X-Correlation-Id` and `X-Validation-Result: approved` (else `403`) | a completed `transaction` |
| `GET /orchestration/status` | | Machine-readable documentation of the pipeline |

Validation rules: risk score below 70, amount at most 50000, merchant active,
`card_payment` permitted, velocity check. The risk score is a stable hash of the
correlation id, merchant, amount and card BIN, so the same inputs always get the
same score and roughly 30 percent of flows are declined. Step 3 uses the
`risk_score` from its body when given (what an orchestration layer passes along
from step 2), else computes it (`risk_score_source` says which). Errors, including
malformed JSON, go through JSON / XML content negotiation.

The chained flow, the way a gateway's orchestration feature would run it:

```hurl
{{#include ../../examples/protocols/orchestration.hurl:pipeline}}
```

An approved and a declined validation, with what step 4 does with each:

```hurl
{{#include ../../examples/protocols/orchestration.hurl:approved}}
```

```hurl
{{#include ../../examples/protocols/orchestration.hurl:declined}}
```

```hurl
{{#include ../../examples/protocols/orchestration.hurl:missing_header}}
```
