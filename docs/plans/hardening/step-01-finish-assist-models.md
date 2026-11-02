# Step 01: Finish the assist model catalogue

## Goal

The uncommitted change adds `GET` and `POST /_rustybin/assist/models` (the provider's model list
merged with the curated table) and one `assist.job.<task>` setting per AI task, with the CLI verbs
`ai models [--load] | jobs | use` and the MCP tools `list_assist_models` and `load_assist_models`.
Its server core is sound (no lock across the outbound await, egress under `Purpose::Assist`, a
2 MiB body cap, admin-only load). Finish it so CI passes and the five-surface rule holds.

## Fixes

| Sev | Where | Problem | Fix |
|---|---|---|---|
| ci | `src/assist/catalog.rs` | `cargo fmt --check` fails | `cargo fmt` |
| ci | `src/assist/tests.rs:1095` | clippy `clone_on_copy` | pass `list` |
| ci | `src/settings.rs:313` | `assist.job.*` name group `assist_jobs`, which `groups()` lacks (`the_production_specs_are_consistent` fails) | add `Group { id: "assist_jobs", title: "AI models per job" }` after `assist` |
| ci | `README.md` | endpoint table stale | `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints` |
| high | `ui/views/manage.js:349` | no console surface: the job settings and the model list are invisible in the console, though `catalog.rs:10` says the AI tab shows them | a "Models and jobs" card on the AI tab: load button, model table, the `assist_jobs` group through `settingsForm`; a `conformance/ui` check |
| medium | `src/assist/mod.rs` `status()` | a job model survives a provider switch (`claude-haiku-5-5` sent to OpenAI) | ignore and flag in `jobs_json` a job model the provider's curated or loaded list does not hold |
| medium | `src/assist/mod.rs` `models_json` | the loaded list is keyed by provider kind only: after a base URL or key change, the old list (fine-tuned ids naming another org) is shown to Read callers | store a fingerprint (kind, base URL, SHA-256 of the key) in `Loaded`, serve only while it matches, add a TTL |
| medium | `docs/src/ai-assist.md`, `docs/examples/assist/assist.hurl`, `docs/src/configuration.md` | no docs, no Hurl example, no settings rows; the "On every surface" table is stale | add them |
| low | `src/assist/api.rs`, `src/mcp/manage/assist.rs` | a keyless Compatible endpoint that fails answers 503 `assist_disabled` | typed error from `load_models` (disabled vs provider), 502 for the second |
| low | `src/assist/provider.rs` `get_json` | an oversized body or a stream error reads as "not JSON" | report "larger than 2 MiB" or the transport error |
| low | `src/assist/mod.rs` specs | `assist.provider` as a `Choice` now refuses the aliases `claude`, `google`, `openai-compatible` (`rustybin.yaml` files that use them fail) | normalise through `Kind::parse` before the choice check (owner decision, master plan section 6) |
| low | `src/cli/commands.rs` `"use"` | `--fast` silently ignored with `--job` | usage error on the combination |
| low | `POST /_rustybin/assist/models` | no single-flight: concurrent loads each call out | skip a load under a few seconds old |

## Tests

The 502 mapping, a load refused by egress, an oversized body, the MCP `load_assist_models` confirm
path, `ai use` (job, fast, both), a provider switch with a job model set, the fingerprint (a key
change hides the old list).

## Done when

CI is green on the change, the console shows and edits the job models, and the docs page has its
table and tested example.

## Build notes (2026-11-02)

- Every finding is fixed; none was closed as not reproducible.
- Provider aliases (master plan section 6): accepted again. A setting can carry a
  `normalize` function (`settings::Spec::normalize`), applied before validation, so
  `claude`, `google` and `openai-compatible` are stored as `anthropic`, `gemini` and
  `compatible` from the API, the console, `rustybin.yaml` and the CLI alike.
- A job model is checked against the provider's curated table and its loaded list.
  The mock takes any id (it answers every model), and so does a compatible endpoint
  until its list is loaded (it has no curated table). An ignored choice shows in
  `Status.ignored`, in each job's `ignored` field and as a warning in the console.
- The loaded list's fingerprint is SHA-256 over the provider, the base URL and the
  key's SHA-256; the TTL is 12 hours (`assist::LOADED_TTL`) on the state's clock.
  Single flight is a `tokio::sync::Mutex` around the load plus a 5 second debounce
  (`LOAD_DEBOUNCE`): concurrent loads make one outbound call.
- `provider::get_json` reads through `read_limited`, which says "larger than 2 MiB"
  or "cut off: <transport error>" instead of truncating.
- Console: the "Models and jobs" card on Management, AI (`#assist-catalog`,
  `#assist-provider-models`, `#assist-jobs`, the `assist_jobs` settings form); the
  conformance run loads the list, saves a job model and clears it.
