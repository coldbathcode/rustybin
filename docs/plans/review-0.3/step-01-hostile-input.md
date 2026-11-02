# Step 01: Hostile input: schema budgets, streams, connections

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | critical | The mock LLM's schema generator pads a string to the caller's `minLength` with no cap (and recounts it each pass): one anonymous `/ai/v1/chat/completions` with `response_format` or a tool schema `minLength: 1e10` exhausts memory | `src/ai/schema.rs:369` | clamp `minLength` (1 KiB), a byte budget in `Gen` as `mocks::schema` has, one `repeat` |
| 2 | high (critical with self-service) | `inline_refs` (operation detail, AI tasks) has depth and stack limits but no breadth budget: S0..S8 with 10 refs each builds 10^8 nodes on an async worker | `src/mocks/service.rs:570` | reuse `schema::inline` with `INLINE_NODES`, or a node counter that leaves `$ref` in place |
| 3 | high | `declared_properties` and `property_schema` walk `allOf` and `$ref` to depth 8 with no node budget: 20 refs per level is 20^8 calls; reached at deploy (pins a compile permit) and in request validation and echo | `src/mocks/schema.rs:740`, `:167` | a shared budget and a visited set of `$ref`s |
| 4 | high | Rerank recomputes the query's words and embedding for every document, inline: a 500 KB query with 256 documents costs tens of seconds per request | `src/ai/cohere.rs:69`, `src/ai/embed.rs:103` | compute the query once, cap its length, score on the blocking pool |
| 5 | medium | `/ai/v1/completions` streams pace each choice with its own total: 128 choices at 20 s keep one connection for about 43 minutes | `src/ai/openai.rs:587` | one time budget for the whole stream |
| 6 | medium (verify first) | No header-read timeout or connection cap on the listeners (slowloris) | `src/server.rs:269`, `:584` | hyper builder with `header_read_timeout`, a connection semaphore |
| 7 | medium (verify first) | The inspector is bounded by entries, not bytes: 500 entries of large heads | `src/inspector.rs:44`, `:536` | cap captured head sizes, a byte budget with eviction |
| 8 | medium | Collections (Hurl, `.http`, k6) take spec path keys and summaries raw: a path key with newlines adds requests and options to a downloaded Hurl file; U+2028 ends a k6 comment | `src/mocks/collections.rs:38`, `src/mocks/spec.rs:546` | reject control characters and whitespace in path keys at parse; `one_line` also replaces U+2028, U+2029, NEL |
| 9 | medium (verify first) | The shared regex cache: 512 entries with 1 MiB size and DFA limits each | `src/mocks/schema.rs:924` | a byte cap, a lower size limit |
| 10 | medium | Saved tests' Hurl export takes `request.path` with control characters (one test becomes several requests) | `src/replay/hurl.rs:48`, `src/replay/mod.rs:183` | reject control characters and whitespace in `path` and `gateway` |
| 11 | low | `exclusiveMinimum: 1e30` overflows `as i64 + 1` (panic in debug) | `src/ai/schema.rs:296`, `:302` | saturating arithmetic |
| 12 | low | WebSocket sends block outside the idle and lifetime timers; gRPC streams have no idle limit | `src/websocket.rs:168`, `src/grpc.rs:171`, `:204` | a send timeout, a stream idle guard |
| 13 | low (verify first) | `Prefer: code=1xx` and `X-Rustybin-Mock-Status` below 200 become a hyper 500 with an error log line per request | `src/mocks/respond.rs:96` | clamp to 200..=599 |
| 14 | low | The portal catalogue cache is keyed by the Host-derived base (8 entries): rotating `Host` renders the whole catalogue per request | `src/portal/items.rs:373`, `src/portal/api.rs:264` | render once without per-request links, or cache validated hosts only |
