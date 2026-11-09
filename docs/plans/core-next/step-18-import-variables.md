# Step 18: Variables given to a scenario import

## Goal

An import leaves out every request that uses a variable its document does not define, and names the
variable. Collections usually keep their base URL and ids in an environment the document does not
carry, so a real collection can import as nothing. The caller gives the values.

## Model

- `variables`: a map of name to value given with the import. A given value wins over the
  document's own.
- At most 64 variables, 1 KiB each; names in the document's own syntax.
- The harness placeholders (`{{id}}`, `{{seq}}` and the rest of the engine's set) are reserved: a
  variable of that name is refused, so an import can never change what a placeholder means.
- Credentials are still removed after substitution: a value that lands in a secret-looking header
  or query parameter goes the way the document's own would. A variable's value is never logged, in
  an audit entry or in `left_out`.
- A value that makes a URL point somewhere new changes nothing about safety: the result is a
  scenario, validated and allowlisted like a hand-written one (`import::fits`).
- The answer keeps naming what is still undefined in `variables`.

## Surfaces

- API: `POST /_rustybin/harness/import` accepts an envelope `{document, format, variables}` as well
  as the bare document it takes today.
- Console: after an import that left requests out for variables, a small form asks for them and
  imports again.
- Management MCP: `import_scenario` gains `variables` (keys ending in `_key` or `_token` are refused
  by `every_tool_is_fully_defined`, so the property is a map and its description says credentials
  are removed).
- CLI: `rustybin harness import <file> --var name=value` (repeatable; `--var` is not a global flag).
- `rustybin.yaml`: nothing, an import is a conversion.

## Tests that fail first

- A collection whose every URL starts with `{{baseUrl}}`: nothing imports without the variable,
  everything with it.
- A given value overrides the document's.
- `--var seq=1` is refused by name.
- A variable placed in an `Authorization` header is removed, and its value is in no answer or log.
- Caps: the 65th variable and a value over 1 KiB are refused.

## Done when

A real collection with an environment imports whole on every surface, and
`docs/src/harness.md#importing-a-postman-collection-or-an-artillery-script` has a tested example.
