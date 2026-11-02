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

## Build notes (2026-11-02)

The checklist against the code:

| Item | Found | Done here |
|---|---|---|
| loaded catalogue | `GET`, `POST /_rustybin/assist/models` (`src/assist/api.rs`), the CLI and the `list_assist_models` / `load_assist_models` tools | nothing |
| job pickers | Management, AI had the provider's models (Use, Fast) and a free-text field per job | every job field now offers the provider's models (a `datalist` filled from the catalogue, curated and loaded), and still takes any id (a compatible endpoint's models are unknown until loaded) |
| scoped cache | `Assist::fingerprint` (kind, base URL, key hash), `LOADED_TTL` 12 h, a reload within 5 s answered from the last | nothing |
| validation | `ignored` per job in the API (`jobs[].ignored`), the warning and the badge in the jobs table | nothing |
| docs | `docs/src/ai-assist.md`, Models and jobs: the 12 h list, a key or URL change, ignored choices | the console's picker and how to refresh, in the console paragraph |

The UI conformance checks that the explain job's field lists the loaded models.
