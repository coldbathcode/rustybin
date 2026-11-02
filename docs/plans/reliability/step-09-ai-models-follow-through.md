# Step 09: AI model selection follow-through

## Goal

Check that hardening step 01 delivered section 8.3 row 6 (the loaded catalogue, job pickers in the
console, a discovery cache scoped to the endpoint and account, validation of unsupported choices)
and complete only what is missing. Document how discovery refreshes and how an unsupported
selection is handled.

## Checklist

| Item | Where it should be |
|---|---|
| loaded catalogue | `GET`, `POST /_rustybin/assist/models` |
| job pickers | Management, AI, Models and jobs (a model list to pick from per job, not only free text) |
| scoped cache | the fingerprint (kind, base URL, key hash) and the 12 h TTL |
| validation | an ignored job model flagged in the API and the console |
| docs | refresh and unsupported selections in `docs/src/ai-assist.md` |
