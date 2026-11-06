# Step 02: The signed image under the demo's limits

## Goal

Run the image the demo will run, as the compose file runs it: read-only root, a tmpfs, no
capabilities, 512 MiB, one CPU, arm64, the free plan. Measure what a console load and a hostile
visitor cost, and fix what does not hold.

## Model

- A visitor's revision is bounded in what it keeps: `mcp::VISITOR_TOOL_NODES` for all its tools
  together (`schema::inline_shared`), `checks::KEPT_SAMPLE_BYTES` for the report's generated bodies.
- All visitor mocks share a budget, `RUSTYBIN_MOCKS_SELF_SERVICE_MAX_MB` (192): `Compiled.weight`
  estimates each revision from its document, `MockStore::check_memory` asks on a create and an edit.
  Saved mocks coming back at startup are not asked.

## Surfaces

One setting, in the environment like the other self-service caps. No new route or tool. API, console,
MCP and CLI all reach it through the store. `rustybin.yaml`: none, self-service caps are not in a
document. `SampleRequest.body_omitted` is new in a report's answer.

## Tests

`a_visitors_tools_expand_a_bounded_number_of_schema_nodes`, `a_report_keeps_a_bounded_amount_of_generated_bodies`,
`visitors_share_a_memory_budget_however_few_mocks_they_hold`. Each was seen to fail with its bound
removed, the edit path on its own too.

## Build notes (built 2026-11-06)

- **Boots with two settings**: the image of `8d720e7` (signature verified) exits fatal as the compose
  file stands, and runs at 8 MiB with `RUSTYBIN_CONTROL_AUTH=open` and `RUSTYBIN_STATE_DIR`. Nothing
  fails on the read-only root; a tmpfs on `/tmp` and `/app/certs` as a volume are enough.
- **One anonymous request ended it**: a counting allocator on `Compiled::build` and `mcp_tools()`:

  | Document | Size | Compile kept | Tools kept | After the fix, all of it |
  |---|---|---|---|---|
  | 195 `POST`s naming one 4,500 property schema, both ways | 160 KiB | 160 MiB | 485 MiB | 17.5 MiB |
  | The same, `GET`s, response only | 143 KiB | 3.6 MiB | 243 MiB | 12.9 MiB |
  | Nested `$ref`s, 16 levels, two per level | 48 KiB | 114 MiB | 97 MiB | 19.6 MiB |
  | One schema of 9,500 properties, inline | 249 KiB | 10 MiB | 8 MiB | 16.2 MiB |
  | YAML aliases | 1 KiB | refused by the parser | | |

  `INLINE_NODES` bounded one schema, so a `$ref` named by every operation multiplied. `mcp_detail`
  asks for the tools on the create itself, so the cost was paid at once.
- **The cap on mocks was not a cap on memory**: 102 padded mocks of 250 KiB ended the container, half
  the 200 allowed, and a mock keeps up to six revisions. A restart with those 102 came back at 320 MiB:
  no crash loop.
- **`weight` errs high on purpose**: a safety budget under-counting is the failure that matters. The
  constants in `compile.rs` come from the table above; the budget is about a third of the container.
- **Stubs cost what the budget saved**: past the budget each remaining property got its own "not
  expanded" schema, so a wide object stops at one note instead.
- **Replayed on an image of this code, at the demo's limits**: three of the 160 KiB documents hold 82
  MiB; filling with the worst shapes is refused at 200 MiB; 600 bodies of 1 MB through the inspector
  reach 213; 75 seconds of 16 workers importing and creating on a full instance reach 322, the rest
  answered `503`. Restart with eight worst-case mocks: 7 seconds on one CPU, 79 MiB.
- **The console fits the free plan**: 34 control calls over a walk of 15 views, no `429`. Clicking
  every 120 ms gets 9 of 60 refused, which a person does not do; `/ui/*` is exempt.
- **Tried and left**: the document cost itself stays about 40 to 100 times its size. Sharing the
  parsed operation's schemas with the root would cut that, and the budget makes it unnecessary now.
