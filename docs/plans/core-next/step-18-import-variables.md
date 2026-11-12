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

## Build notes (built 2026-11-12)

- **Where it lives.** `harness::import::Variables` (checked when made: the caps, the placeholder
  names, the names that look like a credential; its `Debug` shows names only), `Vars.given` beside the
  document's own, `import_given` and `import_text_given` beside the entry points of step 13, and
  `import_body`, which reads the route's body as the bare document or the envelope.
- **The envelope.** A JSON object with `document` that is none of the formats itself: `document`
  is the document or its text (a YAML script travels as text), `format` is optional and wins over
  `?format=`, `variables` is the map. Another key is refused by name. Only JSON: a YAML body is
  always the bare document. A body is parsed for an envelope only when it starts with `{` and
  says `"document"`, so a large recording is not parsed twice.
- **A variable in the place of a Postman host.** Step 13 dropped it with the host, so a collection
  whose URLs only start with `{{baseUrl}}` already imported, without the path a base URL holds. The
  plan's first test ("nothing imports without the variable") is therefore written with variables
  in paths, queries and headers (and with an Artillery script, where a leading variable is a
  path). What changed for the host: a variable there that has a value (given, or the collection's
  own, which is new behaviour for a defined one) is read as a base URL, its host listed in `hosts`
  and its path put before the requests' paths. Without a value it is dropped as before and `notes`
  says a value may be given. It is not added to `variables`, whose meaning ("their requests are in
  `skipped`") and whose step 13 test stay as they were.
- **Every variable of a left-out request is named**, not only the first one met. Step 13 stopped
  reading a request at its first variable without a value, so the form would have needed a round
  per variable. The request is still left out, with the same reason.
- **A refusal for want of variables carries them.** When nothing imports, the `400` has
  `variables` beside `error` (`import::Refusal`), so the console asks for exactly those without
  reading the sentence.
- **Refused beyond the plan's list**: a name that looks like a credential (`token`, `apiKey`:
  step 13 never puts in a document's own variable of such a name, and a secret is never a tool
  input), a value that holds `{{` (it would read as another variable or a placeholder), a name
  that is empty, over 64 characters or holds braces or control characters, a value that is not
  text, a number or a boolean, and any variable given with a recording (HAR, captures), which has
  none. What reads as a placeholder once the values are in (`{{a}}{{a}}seq}}` with `a` = `{`) is
  left out by `fits`, as before.
- **Reserved names are exact.** `seq`, `vu`, `run`, `node`, `layer`, `rand`, `id`, `ip`
  (`scenario::PLACEHOLDERS`). A Postman path variable is `:id`, which is not a placeholder and is
  taken. A collection that writes `{{id}}` cannot have it given: the plan says so, and it is
  the commonest name a collection uses, so it is worth the owner's look.
- **A given variable the document never uses** is named in `notes` (a typo would otherwise pass
  in silence).
- **Arazzo** (the format demo next step 07 added): a workflow input is the document's variable
  (`{{PETNAME}}` for `petName`, the name `variables` shows), so it can be given. An earlier step's
  output (`steps.<id>.outputs.<name>`) is refused by name: only a run makes it, and a scenario has
  no journeys. Also changed there: a reference inside a secret-looking header no longer leaves the
  step out, since the header is removed by name whatever it holds, as in the other two formats.
- **`import_traffic`** (a recording) takes no variables: a recording has none.
- **CLI**: `--var NAME=value`, repeatable; with one, the file is sent as text in the envelope.
  A value is on the command line, as for `rustybin tests run --var`; unlike there, there is no
  `NAME=-` or `NAME=@file`, because a credential is refused here by name rather than read aside.
- **Console**: the form lists the variables the answer names, takes more rows (a base URL is not
  in `variables`), and sends the same file again with the values. The file's text and the values
  are kept in the builder's memory only.
- **Logs and the audit log**: nothing in the import logs, the route's audit entry holds no body,
  the tool's holds no arguments; the route test reads the audit ring for the planted value.
- **Tests**: written with the code, except the CLI test, which failed first (`unknown flag
  --var`). Each protection was then removed once and its test seen to fail: the placeholder
  names, the count cap, the size cap, the credential names, the `{{` in a value, the recording
  refusal, the given value winning, the step output refusal, and the `Debug` that shows names
  only. Not removed to watch a test fail: the removal of a secret-looking header (step 08's
  protection, covered by its own tests and exercised again here with a given value).
- **Not built**: variables in a YAML envelope; values read from stdin or a file on the CLI; a
  Postman environment file read as the variables (its `values` list would map directly, and
  would be the natural next step).
- **Left as step 13 built it, and met while writing the example**: a Postman path variable
  (`:order`) is one only where the URL has its `path` list, as the app exports it. In a URL
  given as one string, `/orders/:order` is a plain path and imports as written.
- **Checks**: `cargo fmt`, clippy with `-D warnings`, the whole suite (one run failed on
  `scenarios::tests::a_gateway_without_the_feature_fails_with_the_expectation_named`, a timing
  test of another module that passed alone and in the next whole run), the dash check, the
  includes check, `mdbook build`, the documentation examples (86 files) and the console check
  (extended: the builder's file field asks for a collection's variables and imports again). The
  Rust 1.88 and cargo-feature builds were left to the merge.

## For the merge

**Changelog** (under "Unreleased"):

New

- **Harness: variables given to a scenario import.** A collection keeps its base URL and ids in
  an environment the exported file does not carry, so it could import as nothing. The caller now
  gives the values: `POST /_rustybin/harness/import` takes an envelope `{document, format,
  variables}` as well as the bare document, `rustybin harness import <file> --var NAME=value`
  (repeatable), `variables` on the `import_scenario` tool, and a form under the console
  builder's file field that asks for the variables an import named and imports again. A given
  value wins over the document's own; at most 64, 1 KiB each. A name that is a harness
  placeholder (`seq`, `vu`, `run`, `node`, `layer`, `rand`, `id`, `ip`) or looks like a
  credential is refused, credentials are removed after the values are put in, and a value is
  never logged, in an audit entry or in an error. For an Arazzo document a workflow input can be
  given, an earlier step's output cannot. Nothing in `rustybin.yaml`: an import is a conversion.

Changed

- A scenario import names every variable a left-out request needs in `variables`, not only the
  first one it meets, and when nothing imports for want of variables the `400` carries the same
  `variables` list beside `error`.
- A Postman collection's variable in the place of a URL's host (`{{baseUrl}}/orders`) that has a
  value, the collection's own included, is read as a base URL: its host is listed in `hosts` and
  its path leads the requests. Without a value it is dropped with the host as before, and `notes`
  says so.
- The harness import of an Arazzo document no longer leaves a step out for a reference inside a
  secret-looking header: the header is removed by name, as in a collection or a script.

Fixed

- Nothing.

**CLAUDE.md**, a paragraph after core next step 13's (or in the same list of rules):

Step 18 (import variables; `docs/plans/core-next/step-18-import-variables.md`): values for a
document's variables given with a harness import (`import::Variables`, the envelope read by
`import::import_body`, `--var`, `variables` of `import_scenario`, the form in
`ui/views/harness.js`). Rules it left: a given value goes in only where the document's own would
(`Vars::value`), so credentials are removed after it by the same code and the result still passes
`import::fits`; a variable is refused by name when it is a harness placeholder
(`scenario::PLACEHOLDERS`) or looks like a credential, and a value never holds `{{`; a value is
never in an error, a note, `left_out`, a log line or an audit entry (`Variables`' `Debug` shows
names only); a refusal that names missing variables carries them as `variables`
(`import::Refusal`); an Arazzo step's output is never given.

**`docs/src/roadmap.md`**: take "variables given to an import by the caller" out of the list under
consideration. A candidate this step leaves: a Postman environment file read as the variables.

**Parity tables**: none changed. `import_scenario` keeps its permission, ceiling and place in
`TOOLS`; the route, its permission and its catalogue row are the same (the description and the
OpenAPI request body grew); the CLI command gained a flag, so `docs/src/cli.md` was regenerated.
