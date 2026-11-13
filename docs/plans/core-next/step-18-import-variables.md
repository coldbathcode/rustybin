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

## Follow-ups (built after 0.7.0)

Two follow-ups the owner asked for after the release, built together on 2026-11-13.

### A. A placeholder name may be given a value

- **What changed.** `Variables::from_value` no longer refuses a name of
  `scenario::PLACEHOLDERS`. A given `id` (or `seq`, `vu`, `run`, `node`, `layer`, `rand`,
  `ip`) is put in where `{{id}}` stands, like any other given variable, and the answer's
  `notes` gains `variables: a harness placeholder name was given a fixed value: id. ...`
  (`import::fixed_placeholders`, names only), so the person knows the imported requests
  carry a constant where the harness would generate a value for each request.
- **What did not.** Without a given value the document's `{{id}}` is treated as before:
  it is the document's variable, never left for the harness to read, so its request is
  left out and `id` is named in `variables` (`Vars::resolve`; the step 13 test
  `postman_variables_are_resolved_or_the_request_is_left_out` is untouched). A value the
  document defines itself for such a name (a collection's `variable` list, a script's
  `config.variables`) also keeps its treatment, which was and is: put in like any other
  variable of the document, with no note (`Vars::define` never looked at the placeholder
  names; only the caller's were refused). A value still never holds `{{`, and what reads
  as a placeholder once the values are in is still left out by `fits`.
- **The note is written when the value is used.** A placeholder name that is given and
  that the document never uses gets the note every unused variable gets, not this one.
- **The console** already listed every name of `variables`, `id` included, so the field was
  there; what stopped it was the instance's refusal. The hint under the form now says what
  giving such a name means.

### B. A Postman environment file as the variables

- **Where it lives.** `Variables::with_environment(variables, environment)`: the given map
  first, then the environment's entries that the map does not name. `check_variable` is
  the one check both kinds of variable meet (`from_value` turns a failure into its refusal,
  the environment into a note). The envelope's fourth key is `environment` (the document or
  its text); the tool's property has the same name; the CLI's flag is `--environment FILE`;
  the console's form has a file field (`#harness-import-env`).
- **Refused or skipped.** A variable typed one by one is refused as before. An
  environment's entry that fails a check is skipped and named in `notes`, never its value,
  and the import goes on: a file is somebody's whole setup, real ones hold `token` and
  `apiKey` entries of the default type, and refusing the file for them would make it
  useless. Skipped by name: an entry of `type: "secret"`, one whose name looks like a
  credential (`keys::looks_secret`), one with no value (null or empty, which is how an
  unset entry is exported), one whose value is no variable's (over 1 KiB, holding `{{`,
  not a scalar). Skipped and counted, not named: an entry whose name no variable could have
  (empty, over 64 characters, braces, control characters), since such a name is not safe to
  repeat. Passed over in silence: `enabled: false` (and `disabled: true`, the collection's
  spelling), as the app does.
- **Refused whole (`400`, nothing of the file quoted).** Text over 256 KiB (checked before
  it is parsed, `MAX_ENVIRONMENT_BYTES`), not JSON, no `values` list, more than 1000
  entries (`MAX_ENVIRONMENT_ENTRIES`), an entry that is no object with a text `key`
  (named by its index), and more than 64 variables once the file's are counted with the
  given ones. A recording with an environment is refused as one with variables is.
- **A placeholder name in the file** (`id`) is taken like one given by hand, with the note
  of follow-up A: the caller handed the file over, which is the caller giving the value.
- **An unused entry of the file is not reported.** The "given and not used" note is for a
  variable typed by hand (a typo); a file's entries the collection never uses are the
  normal case.
- **The console** reads the file in the browser only to fill the fields the form lists
  (never from a secret, disabled or credential-named entry) and sends the file's text as
  `environment` with the import, so the instance applies the rules and names what it did
  not take. The fields win, being `variables`. The form is now also offered when the only
  thing missing is a host's variable (the `hosts:` note), which is exactly the case an
  environment file answers.

### Where this differs from the brief, and why

- **`--environment`, not `--env`.** `--env` is not a global flag, but it reads as "an
  environment variable of the process" (it is that in most command lines). `--environment
  FILE` matches the API's and the tool's `environment`, and its help says "a JSON file, not
  the variables of this process".
- **An environment given as an object is bounded by the request body and by its entries**,
  not by its own byte count: it has been parsed with the envelope by then. Its text form is
  bounded before parsing. The CLI and the console send text.
- **The console form is offered for a dropped host variable too** (not asked for; without
  it a collection whose only variable is its base URL never showed the form or the file
  field).
- **Skipped entries without a value** are a rule the brief did not list.
- **More than 64 usable entries refuse the import** rather than being cut: the brief says
  the caps apply to the total, and a silent cut would leave requests out for no visible
  reason. The way out (disable what the collection does not use) is in the message.

### Not built

- Postman's "initial" versus "current" value (an export holds one `value`), and more than
  one file (an environment plus globals) in one import.
- A value built from another variable (`{{baseUrl}}/v1`) is not resolved: the entry is
  skipped and named.
- Nothing for an Artillery script's own `config.environments` (still named in `left_out`);
  a Postman environment's entries are taken for a script or an Arazzo document like any
  other given variables.

### Tests

- Changed deliberately, because they asserted the old refusal of a placeholder name:
  `import_tests::a_harness_placeholder_is_never_a_variable` is now
  `a_placeholder_name_takes_a_value_only_when_the_caller_gives_one` plus
  `a_value_never_brings_a_placeholder_in` (the second keeps the old test's other halves);
  the `seq` rows of the refusal tables in
  `harness::tests::the_import_route_takes_variables_in_an_envelope`,
  `mcp::manage::tests::import_scenario_takes_variables` and
  `import_tests::a_request_body_is_the_document_or_an_envelope` (there replaced by a
  credential's name, and the envelope's key list in the message grew); the `--var seq=`
  case of `cli::tests::harness_import_takes_variables` (now `--var apiKey=`); the Hurl
  request "a placeholder of the harness is never a variable" in
  `docs/examples/harness/api.hurl` (now a credential's name); the `seq` case of the
  console check (now `apiKey`).
- New: the environment tests at the end of `import_tests.rs`, the route, tool and CLI
  halves in the three tests above, the Hurl example `harness_import_environment` with three
  more requests, the console check's environment file, and
  `the_documented_envelope_is_the_example_collection_and_its_environment`, which holds the
  example envelope to the two example files.

## For the merge (follow-ups)

**Changelog** (under "Unreleased"):

New

- **Harness import: a Postman environment file as the variables.** `POST
  /_rustybin/harness/import` takes `environment` in its envelope (the exported environment
  or globals file, or its text), `rustybin harness import <file> --environment FILE`, the
  `import_scenario` tool takes `environment`, and the console's variables form takes the
  file and fills its fields. A given variable wins over the file's entry. A disabled entry
  is passed over; an entry marked `secret`, one named like a credential and one without a
  usable value are not taken and are named in `notes`, never their values. At most 256 KiB
  and 1000 entries; the 64 variables count the file's with the given ones.

Changed

- **Harness import: a variable named like a harness placeholder may be given a value.**
  `id`, `seq`, `vu`, `run`, `node`, `layer`, `rand` and `ip` were refused as `variables`;
  a collection that writes `{{id}}` kept losing those requests. A given value is now put
  in like any other, and `notes` names the variable, since the imported requests then carry
  a constant where the harness would generate a value. Without a given value nothing
  changed: the request is left out and the name listed in `variables`.
- The console's variables form is also offered when an import only dropped a host's
  variable.

**Upgrade note**: none needed. A caller that relied on the `400` for a placeholder name in
`variables` now gets a `200` with a note; an envelope with a key `environment` was refused
before and is read now.

**CLAUDE.md**, the step 18 paragraph, corrected:

Step 18 (import variables; `step-18-import-variables.md`): values for a document's variables given with a
harness import (`import::Variables`, the envelope read by `import::import_body`, `--var` and
`--environment`, `variables` and `environment` of `import_scenario`, the form in `ui/views/harness.js`).
Rules it left: a given value goes in only where the document's own would (`Vars::value`), so credentials
are removed after it by the same code and the result still passes `import::fits`; a variable is refused by
name when it looks like a credential, and a value never holds `{{`; a document's variable named like a
harness placeholder (`scenario::PLACEHOLDERS`) is the document's, never the harness's: without a value its
request is left out, and a value the caller gives is put in and named in `notes`
(`import::fixed_placeholders`); a Postman environment file is more variables
(`Variables::with_environment`): every entry meets `import::check_variable`, the one check a typed variable
meets, a failing entry is skipped and named in `notes` where a typed one is refused, an entry marked
`secret` is never taken, the file is bounded before it is parsed and a refusal quotes nothing of it; a
value is never in an error, a note, `left_out`, a log line or an audit entry (`Variables`' `Debug` shows
names only); a refusal that names missing variables carries them as `variables` (`import::Refusal`); an
Arazzo step's output is never given.

**`docs/src/roadmap.md`**: take "a Postman environment file read as the variables" out of the
candidates, if it is listed.

**Parity tables**: none changed. The route, its permission and its catalogue row are the same
(the description and the OpenAPI request body grew); `import_scenario` keeps its permission,
ceiling and place in `TOOLS` and gained a property; the CLI command gained a flag, so
`docs/src/cli.md` was regenerated.
