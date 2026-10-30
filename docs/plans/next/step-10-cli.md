# Step 10: The `rustybin` CLI for remote instances

Master plan: [master-plan.md](master-plan.md). Needs: 02 to 09. Status: **done 2026-10-30**. About 2.5 days.

## Goal

Everything the console and the API can do, from a terminal against any instance: `rustybin login`,
then nouns and verbs that mirror the control API, with tables for people and `--json` for scripts,
profiles for several instances, and a parity test so a route never lands without a command.

## Read first

- `src/main.rs` (the thin dispatcher), `src/mocks/cli.rs` (offline mock commands and its argument
  style), `src/harness/cli.rs` (`Args::parse`, `USAGE`, exit codes), `src/mcp/manage/stdio.rs`.
- `src/catalog.rs` (the route list the parity test walks), `src/openapi.rs`.
- Every control route of steps 01 to 09 and the existing ones (`/_rustybin/mocks*`, `/_rustybin/
  requests*`, `/_rustybin/keys/usage`, `/_rustybin/harness*`, `/_rustybin/config`, `/bin`).

## Design

### Shape

`rustybin <noun> <verb> [args] [--url U] [--profile P] [--token T] [--json] [--yes]`. The existing
`mock` (offline), `harness` and `mcp-stdio` commands stay; `mock` gains the remote verbs under
`mocks` (plural) so the offline and remote families are distinct and documented together.

| Noun | Verbs |
|---|---|
| `login`, `logout`, `whoami` | password sign-in (prompted, or `--password-stdin`), token stored in the profile |
| `status` | `/_rustybin/status`, version, modes, features on |
| `settings` | `get`, `set key value`, `set-secret key` (prompted or stdin), `unset`, `schema` |
| `mocks` | `list`, `get`, `create --spec file`, `describe "..."`, `update`, `deploy`, `rollback`, `enable`, `disable`, `delete`, `check`, `coverage`, `examples --apply`, `edit "instruction"`, `sample`, `export --format`, `traffic`, `data` |
| `requests` | `list`, `get`, `tail` (SSE), `clear` |
| `bins` | `create`, `get`, `tail` |
| `agents` | `list`, `get`, `rights`, `revoke`, `restore`, `sessions end` |
| `ai` | `status`, `models`, `test`, `run <task> --input file` |
| `mcp` | `servers`, `connect <url> --auth ...`, `tools`, `call <tool> --args '{}'`, `checks --junit file`, `relay`; stdio targets with `--stdio cmd` |
| `registry` | `list`, `get`, `publish file`, `validate file`, `scenario`, `browse <url>` |
| `portal` | `list`, `publish <id>`, `unpublish`, `branding`, `insights`, `open` |
| `config` | `export`, `apply --dry-run --prune`, `diff`, `schema`, `check file` (offline) |
| `git` | `status`, `init`, `commit -m`, `push`, `pull --apply`, `key` |
| `harness` | the existing commands, plus the remote ones (`runs`, `run --remote`, `report`, `clusters`) |
| `keys` | `usage` (shared mode) |
| `visits` | `list`, `end`, `pause`, `resume` (self-service) |
| `egress` | `check <url>` |
| `completion` | bash, zsh, fish |

### Implementation (`src/cli/`)

- `src/cli/mod.rs`: dispatch, global flags, output (`--json`, tables with `comfy`-free hand-rolled
  alignment as the harness report does, `--quiet`), exit codes (0, 1 for a refused or failed
  operation, 2 usage, 3 network or auth), confirmations for destructive verbs (`--yes` to skip).
- `src/cli/client.rs`: a small typed client over the control API with `reqwest` (already a
  dependency), bearer from the profile, `X-Rustybin-Session` for test calls, SSE reading for tails.
- `src/cli/profiles.rs`: `~/.config/rustybin/profiles.toml` (0600; `RUSTYBIN_CONFIG_HOME` to move
  it): `[profiles.name] url, token, expires_at`; `RUSTYBIN_URL` and `RUSTYBIN_TOKEN` override; the
  default profile is the last login.
- Shared types: the CLI uses the server's own response types where they are `Serialize` and
  `Deserialize` (`report::Report`, the settings schema, the config document), so a server change
  that breaks the CLI breaks the build.
- Help: every command has a one-line summary and examples; `rustybin help <noun>`; a test
  generates `docs/src/cli.md` from the command tree (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test
  cli_docs`) the way the README tables are generated.
- Parity test (`src/cli/tests.rs`): every catalogue route under `/_rustybin/` (except the MCP
  endpoint, the OAuth sign-in routes, the node routes of the harness and an explicit allowlist with
  a reason) has a command that calls it, checked by a table in the test.

## Tests

- Unit: argument parsing, profiles (precedence, permissions), output formatting, confirmations.
- Full app: every noun against an in-process instance in password mode (login, then one verb per
  route; the parity table drives it), tails over SSE, exit codes, `--json` shapes.
- `docs/examples/cli/*.sh` run by the examples runner against the examples instances.

## Docs

`docs/src/cli.md` (generated), a hand-written `docs/src/cli-guide.md` (profiles, scripting, CI use,
the offline commands), README (the CLI section), the changelog.

## Done when

`rustybin login` and one verb per control route work against a fresh instance; the parity test
passes; the generated reference is in the book; all checks pass; committed and pushed.

## Build notes

Built 2026-10-30. What differs from the plan and why:

- **A table, not a trait per command.** `src/cli/commands.rs` is one static table
  (`COMMANDS`: noun, verb, usage, summary, flags, the routes it calls, destructive,
  example) and one `dispatch` match; `mod.rs` holds the parser (flags are declared per
  command, so `--flag value`, `--flag=value` and a declared short flag such as `-m` parse
  without ambiguity), the help, the generated reference, the shell completion and the
  rendering for people (objects as `key: value`, lists of objects as aligned tables,
  nothing hand-rolled per command; `--json` prints the answer as it is). 158 commands
  over 24 nouns. The parity test walks every catalogue path under `/_rustybin/` and
  `/bin` against the table's routes and `UNCOVERED` (ten paths with a reason: the
  management MCP endpoint, the OAuth sign-in routes, the cluster's node-to-node routes,
  a bin's capture URL); it also checks that every route a command names exists with
  that method, that no command flag clashes with a global one, and that every command
  has an example, which the smoke test then runs against a real instance in password
  mode (a usage or credential exit code fails it; a refusal does not).
- **Profiles are JSON, not TOML** (`profiles.json`, mode 0600 through
  `settings::write_private`): no new dependency. `RUSTYBIN_CONFIG_HOME`, else
  `$XDG_CONFIG_HOME/rustybin`, else `~/.config/rustybin`. Precedence: `--url` and
  `--token`, then `RUSTYBIN_URL` (also the mocks CLI's `RUSTYBIN_SERVER`) and
  `RUSTYBIN_TOKEN` (also `RUSTYBIN_ADMIN_TOKEN`), then `--profile`, `RUSTYBIN_PROFILE`
  or the default profile, then `http://localhost:<RUSTYBIN_HTTP_PORT or 80>` (the
  server's own default, as the mocks CLI, so `docker exec` works). A profile's token
  only goes to the profile's URL. `login --token` keeps a token without a sign-in;
  `password change` keeps the fresh token the instance answers with.
- **Nouns that differ from the plan's table.** `status` is its own command and
  `instance` holds `version`, `ready`, `config`, `catalog`, `metrics`, `usage`;
  `sessions` and `password` are nouns (the plan had them under settings); `visit` (a
  visitor's own visit) sits next to `visits` (the operator's); `bins` has `list`,
  `create`, `delete`, `requests`, `get`, `tail`; `mcp` has `check` (one run, `--junit`
  writes the report), `runs`, `run`, `delete-run`, `save-connection` and friends,
  `call` and `tools` through the relay (one stateless 2026-07-28 message with the
  client's `_meta`; no relay sessions, no stdio targets, which `rustybin mcp-stdio`
  covers the other way round), `fetch`, `end-relay`; `registry publish` goes through the
  mock registry's own publish API (an `auth/none` token first); `portal branding` reads
  the `portal.*` settings; `config diff` is an apply with `dry_run` printed as lines;
  `egress show` only (there is no check route; the plan's `egress check` would have
  needed one). The harness `validate`, `run` and `report` stay the offline commands of
  `src/harness/cli.rs` unless `--remote` sends them to the instance (`main.rs` routes
  on that flag and the verb); `run --remote --wait` follows the events and prints the
  report with the report's exit code. `harness join` takes `--join-token` (`--token`
  is the credential everywhere).
- **Secrets without echo** through `libc` termios (already a dependency), with
  `--stdin` or a non-terminal stdin reading a line instead; destructive verbs ask, and
  refuse without a terminal unless `--yes`.
- **Typed answers.** The CLI reads JSON values rather than the server's types: the
  answers are shaped by the routes (and the parity test keeps the routes), and the
  rendering is generic, so there was nothing to gain from sharing structs. The plan's
  "a server change that breaks the CLI breaks the build" holds through the smoke test
  instead.
- **Docs.** `docs/src/cli.md` is generated from the table (the `cli_docs` test,
  `RUSTYBIN_UPDATE_CLI_DOCS=1`); `docs/src/cli-guide.md` is the hand-written guide;
  `docs/examples/cli/cli.sh` starts its own password-mode instance (the runner's
  password instance changes its password in other examples). README has a Command
  line section.
- **Checks.** 1075 lib tests (6 new: parsing, profiles and their mode, rendering, the
  tree against the catalogue, the generated reference, and 158 commands against a live
  instance plus the sign-in, settings, export, diff, git, tails, confirmations,
  password change, sign out everywhere, logout and profile flows), clippy, fmt, the
  docs examples, mdBook, includes and dashes. The console is unchanged, so no browser
  check.
- **For step 11:** the management MCP tools get the same parity treatment against the
  catalogue; the CLI's `UNCOVERED` list is the model.
