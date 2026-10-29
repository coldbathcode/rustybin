# Step 10: The `rustybin` CLI for remote instances

Master plan: [master-plan.md](master-plan.md). Needs: 02 to 09. Status: **not started**. About 2.5 days.

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

(filled in when built)
