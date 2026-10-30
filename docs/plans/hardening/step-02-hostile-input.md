# Step 02: Hostile specs and queries: memory and CPU budgets

## Goal

No single anonymous request may abort the process or hold a tokio worker for more than a moment.
On a self-service instance `POST /_rustybin/mocks/import` parses a visitor's spec and runs the deploy
checks inline, so the first two findings take one request each.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| critical | `src/mocks/spec.rs:776-783` | server variable expansion runs `url.replace` once per `{name}` occurrence and rescans inserted text: a default of `{a}{a}` doubles the URL per pass, a few hundred bytes of spec abort the process on allocation failure | deduplicate names, substitute in one left-to-right pass that never rescans, cap the result (2 KiB) with a warning |
| critical | `src/mocks/checks.rs:249-251` | the sample path repeats a 128 KB example once per duplicate `{a}` placeholder (accepted at `spec.rs:575`): gigabytes at compile time | refuse repeated placeholders and more than 32 per path, cap the sample path length |
| high | `src/mocks/schema.rs:51,54,474-487,600-613`, `src/mocks/handlers.rs:571-578` | generation caps nodes (5000) but not bytes: nested `minItems: 20` around `minLength: 65536` strings gives about 310 MB per GET, copied three times (pretty print, `bytes.clone()` for an 8 KB preview); deploy checks render it per status at compile | a total output byte budget in `Gen` (1 MiB); hand the capture only the preview bytes |
| high | `src/mocks/schema.rs:1356-1362` | `uniqueItems` is O(n^2) via `items[..i].contains(a)` and outside `MAX_CHECKS`: 1 MB of numbers, about 10^10 comparisons | charge the budget per comparison or hash canonical serialisations; skip past a few thousand items |
| high | `src/graphql.rs:1078-1086` | `count_aliases` re-walks every fragment spread with no memo before async-graphql's limits run: under 1 KB of fragments spreading the next ten times blocks a worker for minutes, a few requests freeze `GET /` and readiness | memoise per fragment (name to count, saturating multiply) or cap visited selections (10k) |
| high | `src/grpc.rs:154-172,291` | `ClientStream` keeps 1000 messages of up to 4 MiB each and joins them; no `max_decoding_message_size`, and the HTTP body limit does not apply to gRPC | track bytes, `resource_exhausted` past `config.body_limit`; set decoding and encoding limits on `EchoServiceServer` |
| medium | `src/mocks/compile.rs:196` | parse, compile and `checks::run` run inline in async handlers on every preview, create and settings edit; visitor recompiles are not rate-limited | `spawn_blocking` behind a semaphore (2 to 4 permits, 503 when full); rate-limit visitor PATCHes |
| medium | `src/mocks/schema.rs:866-882` | one global regex cache of 512, cleared when full, compiled under its mutex: 513 rotating near-limit patterns serialise every tenant's regex use | compile outside the lock, LRU eviction, precompile a revision's patterns into `Compiled` |
| low | `src/webhooks.rs:222,227,235` | `now - ts` overflows on `webhook-timestamp: -9223372036854775808` (debug panic, wrong age in release) | `saturating_sub` |

The GraphQL WebSocket path skips `count_aliases`; the complexity limit (500) bounds it, so only
note it, unless the memoised walk is cheap enough to call there too.

## Tests

One test per row with the hostile input: each must finish under a time budget (for example 200 ms
in debug) and allocate under a ceiling, or answer 400 or 422 with a clear message. A test that
compile runs off the runtime (a stalled compile does not delay `GET /`).

## Done when

Every hostile input above answers quickly with an error or a bounded result, and `GET /` stays
responsive while ten hostile compiles run.
