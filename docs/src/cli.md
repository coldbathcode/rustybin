# CLI reference

Generated from the command tree (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`); the [CLI guide](cli-guide.md) says how to sign in, use profiles and script it, and the [mocks CLI](mocks/cli.md) covers the offline `rustybin mock` commands.

`rustybin <noun> <verb> [args] [flags]`. Every command takes the global flags; `--json` prints the instance's answer as it is, `--yes` skips a confirmation. Exit codes: `0` done, `1` refused or failed, `2` usage, `3` the network, the credentials, or a permission the credential lacks (`permission_required: <permission>`). The **Needs** column names the permission each command needs (the routes it calls declare it).

| Flag | Meaning |
|---|---|
| `--url` | The instance URL (else RUSTYBIN_URL, the profile, http://localhost:80) |
| `--profile` | The profile to use (else RUSTYBIN_PROFILE, the default profile) |
| `--token` | A control token or the admin token (else RUSTYBIN_TOKEN, the profile) |
| `--session` | The X-Rustybin-Session the calls carry |
| `--mock-token` | A self-service manage token (else RUSTYBIN_MOCK_TOKEN) |
| `--json` | Print the instance's JSON answer as it is |
| `--yes` | Skip the confirmation of a destructive verb |
| `--quiet` | No progress or confirmation lines |
| `--help` | This help |

## login

Sign in to an instance and keep its token in a profile

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin login [--url U] [--user U] [--password-stdin] [--sso [--no-browser] [--wait S]] [--profile P]` | Sign in with the console user name and password (prompted, or the first line of stdin) and keep the token in a profile; --sso signs in with the identity provider in the browser instead (a loopback address on this machine takes the answer); --token keeps a token you already have. Flags: `--user` value, `--password-stdin`, `--sso`, `--no-browser`, `--wait` value. | `POST /_rustybin/login`, `GET /_rustybin/session`, `GET /_rustybin/sso/status`, `GET /_rustybin/sso/start`, `POST /_rustybin/sso/token` | open, signed in |

```bash
rustybin login --url https://mocks.example.com --user rustybin
```

## logout

Revoke the profile's token and forget it

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin logout` | Revoke the profile's token on the instance and forget it. | `POST /_rustybin/logout` | signed in |

```bash
rustybin logout
```

## whoami

Who the current credential is

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin whoami` | The credential the calls carry: user, scopes, expiry. | `GET /_rustybin/session` | signed in |

```bash
rustybin whoami
```

## status

The instance's status

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin status` | Name, version, modes, health, the features that are on. | `GET /_rustybin/status` | `instance.read` |

```bash
rustybin status
```

## doctor

What is wrong with the instance, and the fix

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin doctor` | What is wrong with the instance and how to fix it; exits 1 when a check fails. | `GET /_rustybin/diagnostics` | `settings.read` |

```bash
rustybin doctor
```

## instance

Version, readiness, the running configuration, the catalogue, metrics, usage

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin instance version` | Version and build. | `GET /_rustybin/version` | `instance.read` |
| `rustybin instance ready` | Readiness (never authenticated). | `GET /_rustybin/ready` | open |
| `rustybin instance config` | The running configuration (no secrets). | `GET /_rustybin/config` | `instance.read` |
| `rustybin instance catalog [--category C]` | The route catalogue. Flags: `--category` value. | `GET /_rustybin/catalog` | `instance.read` |
| `rustybin instance metrics` | Prometheus metrics, as text. | `GET /_rustybin/metrics` | `instance.read` |
| `rustybin instance usage` | Plan limits and this client's usage. | `GET /_rustybin/usage` | `instance.read` |

```bash
rustybin instance version
rustybin instance ready
rustybin instance config
rustybin instance catalog --category "Mock APIs (OpenAPI)"
rustybin instance metrics
rustybin instance usage
```

## profiles

The profiles this machine keeps

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin profiles list` | The profiles and the default. |  | (none) |
| `rustybin profiles use <name>` | Make a profile the default. |  | (none) |
| `rustybin profiles remove <name>` | Forget a profile. |  | (none) |

```bash
rustybin profiles list
rustybin profiles use mocks.example.com
rustybin profiles remove old-instance
```

## settings

Instance settings: get, set, secrets, unset, the schema

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin settings get [key]` | Every setting with its value and source, or one. | `GET /_rustybin/settings` | `settings.read` |
| `rustybin settings set <key> <value>` | Save a setting (JSON values are parsed, anything else is a string). | `PATCH /_rustybin/settings` | `settings.write` |
| `rustybin settings set-secret <key> [--stdin]` | Save a secret (prompted without echo, or the first line of stdin). Flags: `--stdin`. | `PATCH /_rustybin/settings` | `settings.write` |
| `rustybin settings unset <key>` | Forget the stored value (back to the environment or the default). | `DELETE /_rustybin/settings/{key}` | `settings.write` |
| `rustybin settings schema` | Every registered setting: key, kind, default, variable. | `GET /_rustybin/settings` | `settings.read` |

```bash
rustybin settings get console.title
rustybin settings set console.title "Payments mocks"
rustybin settings set-secret assist.openai_key --stdin < key.txt
rustybin settings unset console.title
rustybin settings schema
```

## sessions

The console's sign-in sessions

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin sessions list` | The sign-in sessions since the last restart. | `GET /_rustybin/sessions` | `users.read` |
| `rustybin sessions end <id>` | End one session. | `DELETE /_rustybin/sessions/{id}` | `users.manage` |
| `rustybin sessions end-all` | Sign out everywhere (every token issued so far). | `DELETE /_rustybin/sessions` | `users.manage` |

```bash
rustybin sessions list
rustybin sessions end 3f2a
rustybin sessions end-all --yes
```

## password

Change the first admin's user name and password (0.3; rustybin passwd changes yours)

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin password change [--user U] [--stdin]` | Change your password (the first admin's also takes a new user name): the current and the new one prompted, or two lines of stdin. Flags: `--user` value, `--stdin`. | `PUT /_rustybin/password` | signed in |
| `rustybin password hash [--stdin]` | Hash a password for RUSTYBIN_CONSOLE_PASSWORD_HASH, offline: prompted, or the first line of stdin. Prints the argon2id hash and nothing else. Flags: `--stdin`. |  | (none) |

```bash
rustybin password change --stdin < passwords.txt
rustybin password hash --stdin
```

## users

Users: invite, change roles, disable, delete (the password sign-in)

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin users list` | The users: roles, status (active, invited, disabled), last sign-in. | `GET /_rustybin/users` | `users.read` |
| `rustybin users invite <name> --role R [--display-name D] [--email E] [--reset] [--sso]` | Invite a user: the answer is a link to send them (shown once), where they set their own password. --reset sends a user who exists a new link (a password reset); --sso adds a single sign-on user instead (no link). Flags: `--role` value, `--display-name` value, `--email` value, `--reset`, `--sso`, `--project` value. | `POST /_rustybin/users`, `POST /_rustybin/users/{id}/invite` | `users.manage` |
| `rustybin users show <user>` | One user (an id or a sign-in name): roles, permissions, status, the open invitation. | `GET /_rustybin/users/{id}` | `users.read` |
| `rustybin users update <user> [--role R] [--pin-role R] [--display-name D] [--email E]` | Change a user's roles (--role replaces them; repeatable), a single sign-on user's pinned roles (--pin-role), display name or email (--email "" clears it). Flags: `--role` value, `--display-name` value, `--email` value, `--pin-role` value. | `PATCH /_rustybin/users/{id}` | `users.manage` |
| `rustybin users disable <user>` | Disable a user: their sessions end and their sign-in is refused. | `PATCH /_rustybin/users/{id}` | `users.manage` |
| `rustybin users enable <user>` | Enable a disabled user again (an invited one joins through the invitation). | `PATCH /_rustybin/users/{id}` | `users.manage` |
| `rustybin users sessions <user> [--end]` | A user's sign-in sessions; --end signs them out everywhere. Flags: `--end`. | `GET /_rustybin/users/{id}/sessions`, `DELETE /_rustybin/users/{id}/sessions` | `users.read`, `users.manage` |
| `rustybin users delete <user>` | Delete a user (never the last enabled admin, never the first admin). | `DELETE /_rustybin/users/{id}` | `users.manage` |

```bash
rustybin users list
rustybin users invite alice --role editor --display-name "Alice Doe"
rustybin users show alice
rustybin users update alice --role tester
rustybin users disable alice --yes
rustybin users enable alice
rustybin users sessions alice
rustybin users delete alice --yes
```

## roles

Roles: the built-in ones and custom roles with their permissions

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin roles list` | The roles with their permissions, whether they are privileged and how many users hold them. | `GET /_rustybin/roles` | `users.read` |
| `rustybin roles show <name>` | One role: its permissions and who holds it. | `GET /_rustybin/roles/{name}` | `users.read` |
| `rustybin roles save <name> --permission P [--description D]` | Create or change a custom role (--permission repeatable, or comma separated). Flags: `--permission` value, `--description` value. | `PUT /_rustybin/roles/{name}`, `POST /_rustybin/roles` | `users.manage` |
| `rustybin roles delete <name>` | Delete a custom role nobody holds. | `DELETE /_rustybin/roles/{name}` | `users.manage` |

```bash
rustybin roles list
rustybin roles show editor
rustybin roles save catalog-reader --permission mocks.read,traffic.read --description "Reads mocks and traffic"
rustybin roles delete catalog-reader --yes
```

## projects

Projects: roles on part of the instance (mocks, saved tests, checkpoints)

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin projects list` | The projects you may see, your roles in each and what each holds. | `GET /_rustybin/projects` | signed in |
| `rustybin projects show <name>` | One project: its members (with users.read), your roles, its mocks, saved tests and checkpoints. | `GET /_rustybin/projects/{name}` | signed in |
| `rustybin projects save <name> [--description D] [--member USER=role+role]...` | Create or change a project; --member (repeatable) replaces the members. Flags: `--description` value, `--member` value, `--no-members`. | `PUT /_rustybin/projects/{name}` | `users.manage` |
| `rustybin projects delete <name> [--move-to P]` | Delete a project; one that holds resources needs --move-to (another project, or instance). Flags: `--move-to` value. | `DELETE /_rustybin/projects/{name}` | `users.manage` |

```bash
rustybin projects list
rustybin projects show payments
rustybin projects save payments --description "The payments team"
rustybin projects delete payments --move-to instance --yes
```

## me

Your own account: who you are, display name, email, sessions

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin me` | Your own account: user, display name, email, roles, permissions. | `GET /_rustybin/me` | signed in |
| `rustybin me update [--display-name D] [--email E]` | Change your display name or email (--email "" clears it). Flags: `--display-name` value, `--email` value. | `PATCH /_rustybin/me` | signed in |
| `rustybin me sessions [--end-all]` | Your sign-in sessions; --end-all signs you out everywhere (this profile too). Flags: `--end-all`. | `GET /_rustybin/me/sessions`, `DELETE /_rustybin/me/sessions` | signed in |
| `rustybin me end-session <id>` | End one of your sign-in sessions. | `DELETE /_rustybin/me/sessions/{id}` | signed in |

```bash
rustybin me
rustybin me update --display-name "Ops on call"
rustybin me sessions
rustybin me end-session 3f2a
```

## passwd

Change your own password

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin passwd [--stdin]` | Change your own password: the current and the new one prompted, or two lines of stdin. Your other sessions end; the profile keeps a fresh token. Flags: `--stdin`. | `PUT /_rustybin/me/password` | signed in |

```bash
rustybin passwd --stdin < passwords.txt
```

## sso

Single sign-on with OpenID Connect: its status, and a test against the provider

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin sso status` | Single sign-on as the sign-in screen sees it: on, ready, the button's name, whether the password sign-in works. | `GET /_rustybin/sso/status` | open |
| `rustybin sso test` | Fetch the identity provider's discovery document and keys now, and check the role map; exits 1 when the provider cannot be read. | `POST /_rustybin/sso/test` | `settings.write` |

```bash
rustybin sso status
rustybin sso test
```

## mocks

OpenAPI mocks on the instance (rustybin mock ... works offline)

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin mocks list [--all] [--project P]` | The mocks with their state and traffic. Flags: `--all`, `--project` value. | `GET /_rustybin/mocks` | `mocks.read` |
| `rustybin mocks get <name>` | One mock: settings, operations, the deploy report. | `GET /_rustybin/mocks/{name}` | `mocks.read` |
| `rustybin mocks create <name> (--spec FILE \| --spec-url URL) [--project P]` | Create a mock from an OpenAPI file (YAML or JSON), or from a URL the instance fetches itself. Flags: `--spec` value, `--spec-url` value, `--mode` value, `--disabled`, `--no-deploy`, `--strip-server-prefix`, `--project` value. | `POST /_rustybin/mocks` | `mocks.write` |
| `rustybin mocks move <name> --project P` | Move a mock to another project (instance: the instance's); needs mocks.write in both. Flags: `--project` value. | `PATCH /_rustybin/mocks/{name}` | `mocks.write` |
| `rustybin mocks import --spec FILE` | Preview a spec: title, operations, the checks, the suggested name. Flags: `--spec` value. | `POST /_rustybin/mocks/import` | `mocks.read` |
| `rustybin mocks update <name> [--spec FILE] [--mode M] [--settings JSON]` | Change a mock's spec or settings (the draft, deployed unless --no-deploy). Flags: `--spec` value, `--mode` value, `--settings` value, `--no-deploy`, `--force`, `--expect-revision` value. | `PATCH /_rustybin/mocks/{name}` | `mocks.write` |
| `rustybin mocks deploy <name> [--force] [--expect-revision N]` | Deploy the draft. Flags: `--force`, `--expect-revision` value. | `POST /_rustybin/mocks/{name}/deploy` | `mocks.write` |
| `rustybin mocks rollback <name> [--revision N]` | Go back to the previous (or a given) revision. Flags: `--revision` value. | `POST /_rustybin/mocks/{name}/rollback` | `mocks.write` |
| `rustybin mocks enable <name>` | Switch a mock on. | `POST /_rustybin/mocks/{name}/enable` | `mocks.write` |
| `rustybin mocks disable <name>` | Switch a mock off. | `POST /_rustybin/mocks/{name}/disable` | `mocks.write` |
| `rustybin mocks delete <name>` | Delete a mock. | `DELETE /_rustybin/mocks/{name}` | `mocks.delete` |
| `rustybin mocks duplicate <name> [--name NEW]` | Copy a mock. Flags: `--name` value. | `POST /_rustybin/mocks/{name}/duplicate` | `mocks.write` |
| `rustybin mocks check <name>` | The deploy report: a valid request and response per operation. | `GET /_rustybin/mocks/{name}/report` | `mocks.read` |
| `rustybin mocks coverage <name>` | Which operations lack an example. | `GET /_rustybin/mocks/{name}/coverage` | `mocks.read` |
| `rustybin mocks spec <name>` | The mock's OpenAPI document as stored. | `GET /_rustybin/mocks/{name}/spec` | `mocks.read` |
| `rustybin mocks revision <name> <revision>` | A kept revision: what changed since in the draft, its settings and spec (the whole answer with --json). Flags: `--spec`. | `GET /_rustybin/mocks/{name}/revisions/{revision}` | `mocks.read` |
| `rustybin mocks export [name]` | One mock, or all, as a rustybin.yaml with only the mocks section. | `GET /_rustybin/mocks/{name}/export`, `GET /_rustybin/mocks/export` | `mocks.read` |
| `rustybin mocks apply <file> [--prune]` | Apply a rustybin.yaml (spec files are read locally and inlined); --prune deletes API mocks it does not name. Flags: `--prune`. | `POST /_rustybin/mocks/apply` | `config.apply` |
| `rustybin mocks reload` | Reload the file-defined mocks (RUSTYBIN_CONFIG_FILE, RUSTYBIN_MOCKS_DIR). | `POST /_rustybin/mocks/reload` | `config.apply` |
| `rustybin mocks schema` | The JSON Schema of rustybin.yaml. | `GET /_rustybin/mocks/config.schema.json` | `mocks.read` |
| `rustybin mocks sample <name> <operationId>` | A valid response for an operation. Flags: `--status` value, `--seed` value, `--mode` value, `--example` value. | `GET /_rustybin/mocks/{name}/operations/{op}/sample` | `mocks.read` |
| `rustybin mocks validate <name> <operationId> --body FILE` | Validate a request or response body against the spec. Flags: `--body` value, `--kind` value, `--status` value. | `POST /_rustybin/mocks/{name}/operations/{op}/validate` | `mocks.read` |
| `rustybin mocks operation <name> <operationId>` | One operation's settings and statistics. | `GET /_rustybin/mocks/{name}/operations/{op}` | `mocks.read` |
| `rustybin mocks set-operation <name> <operationId> --settings JSON` | Set an operation's settings (respond, rules, mode, delay_ms, public). Flags: `--settings` value, `--expect-revision` value. | `PUT /_rustybin/mocks/{name}/operations/{op}` | `mocks.write` |
| `rustybin mocks test-rules <name> <operationId> [--rules FILE] [--request N]` | Try an operation's rules (or given ones) on a captured request. Flags: `--rules` value, `--request` value. | `POST /_rustybin/mocks/{name}/operations/{op}/rules/test` | `mocks.read` |
| `rustybin mocks traffic <name> [--limit N]` | The mock's captured requests. Flags: `--limit` value. | `GET /_rustybin/mocks/{name}/requests` | `traffic.read` |
| `rustybin mocks traffic-get <name> <n>` | One captured request of the mock. | `GET /_rustybin/mocks/{name}/requests/{n}` | `traffic.read` |
| `rustybin mocks traffic-tail <name>` | Follow the mock's requests as they arrive. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/mocks/{name}/requests/stream` | `traffic.read` |
| `rustybin mocks traffic-clear <name>` | Forget the mock's captured requests. | `DELETE /_rustybin/mocks/{name}/requests` | `mocks.data` |
| `rustybin mocks data <name>` | The items a stateful mock stored. | `GET /_rustybin/mocks/{name}/state` | `mocks.read` |
| `rustybin mocks data-clear <name>` | Forget the items a stateful mock stored. | `DELETE /_rustybin/mocks/{name}/state` | `mocks.data` |
| `rustybin mocks collection <name> <format>` | The mock as a postman, hurl, curl, http, k6, bruno or insomnia collection. Flags: `--base-url` value. | `GET /_rustybin/mocks/{name}/collections/{format}` | `mocks.read` |
| `rustybin mocks describe "<description>"` | An OpenAPI spec from a description (AI), printed as JSON. Flags: `--openapi-version` value, `--model` value. | `POST /_rustybin/mocks/ai/spec` | `assist.jobs` |
| `rustybin mocks examples <name> [--apply]` | Example payloads for the operations that lack one (AI). Flags: `--operations` value, `--per-response` value, `--persona` value, `--source` value, `--apply`. | `POST /_rustybin/mocks/{name}/ai/examples` | `assist.jobs` |
| `rustybin mocks edit <name> "<instruction>" [--apply]` | Edit a spec with an instruction (AI): the diff, applied with --apply. Flags: `--apply`, `--model` value. | `POST /_rustybin/mocks/{name}/ai/edit` | `assist.jobs` |

```bash
rustybin mocks list
rustybin mocks get petstore
rustybin mocks create orders --spec orders.yaml --mode generated
rustybin mocks move orders --project instance
rustybin mocks import --spec orders.yaml
rustybin mocks update orders --mode stateful
rustybin mocks deploy orders
rustybin mocks rollback orders
rustybin mocks enable orders
rustybin mocks disable orders
rustybin mocks delete orders --yes
rustybin mocks duplicate orders --name orders-v2
rustybin mocks check orders
rustybin mocks coverage orders
rustybin mocks spec orders > orders.yaml
rustybin mocks revision orders 12
rustybin mocks export orders
rustybin mocks apply rustybin.yaml --prune --yes
rustybin mocks reload
rustybin mocks schema
rustybin mocks sample orders listOrders --status 200
rustybin mocks validate orders createOrder --body order.json --kind request
rustybin mocks operation orders listOrders
rustybin mocks set-operation orders listOrders --settings '{"delay_ms": 200}'
rustybin mocks test-rules orders getOrder
rustybin mocks traffic orders --limit 20
rustybin mocks traffic-get orders 7
rustybin mocks traffic-tail orders --max 10
rustybin mocks traffic-clear orders --yes
rustybin mocks data orders
rustybin mocks data-clear orders --yes
rustybin mocks collection orders hurl > orders.hurl
rustybin mocks describe "a parcel tracking API with shipments and events"
rustybin mocks examples orders --apply
rustybin mocks edit orders "add a DELETE /orders/{id} operation" --apply
```

## visits

Self-service visits, as the operator

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin visits list` | Every self-service visit. | `GET /_rustybin/mocks/visits` | `visits.operate` |
| `rustybin visits end <id>` | End a visit (its mocks go). | `DELETE /_rustybin/mocks/visits/{id}` | `visits.operate` |
| `rustybin visits end-all` | End every visit. | `DELETE /_rustybin/mocks/visits` | `visits.operate` |
| `rustybin visits pause` | Stop accepting new visits. | `POST /_rustybin/mocks/visits/pause` | `visits.operate` |
| `rustybin visits resume` | Accept new visits again. | `POST /_rustybin/mocks/visits/resume` | `visits.operate` |

```bash
rustybin visits list
rustybin visits end 9c1e --yes
rustybin visits end-all --yes
rustybin visits pause
rustybin visits resume
```

## visit

Your own self-service visit, as a visitor

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin visit get` | Your visit (needs --mock-token or RUSTYBIN_MOCK_TOKEN). | `GET /_rustybin/mocks/visit` | `mocks.read` |
| `rustybin visit start [--email E]` | Start a visit; the manage token is shown once. Flags: `--email` value. | `POST /_rustybin/mocks/visit` | signed in |
| `rustybin visit extend` | Extend your visit. | `POST /_rustybin/mocks/visit/extend` | signed in |
| `rustybin visit rotate-token` | A new manage token for your visit. | `POST /_rustybin/mocks/visit/token` | signed in |
| `rustybin visit end` | End your visit. | `DELETE /_rustybin/mocks/visit` | signed in |

```bash
rustybin visit get --mock-token rbm_...
rustybin visit start
rustybin visit extend --mock-token rbm_...
rustybin visit rotate-token --mock-token rbm_...
rustybin visit end --mock-token rbm_... --yes
```

## requests

The request inspector

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin requests list [--limit N] [--path-prefix P]` | The captured requests. Flags: `--limit` value, `--path-prefix` value. | `GET /_rustybin/requests` | `traffic.read` |
| `rustybin requests get <id>` | One captured request. | `GET /_rustybin/requests/{id}` | `traffic.read` |
| `rustybin requests tail` | Follow the requests as they arrive. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/requests/stream` | `traffic.read` |
| `rustybin requests clear` | Forget the captured requests. | `DELETE /_rustybin/requests` | `traffic.read` |

```bash
rustybin requests list --limit 20
rustybin requests get 42
rustybin requests tail --max 5
rustybin requests clear --yes
```

## bins

Request bins

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin bins list` | Your request bins. | `GET /bin` | (none) |
| `rustybin bins create [--status N] [--body B] [--delay-ms N]` | A new bin, with the answer it gives. Flags: `--status` value, `--body` value, `--delay-ms` value. | `POST /bin` | (none) |
| `rustybin bins delete <id>` | Delete a bin. | `DELETE /bin/{id}` | (none) |
| `rustybin bins requests <id> [--limit N]` | What a bin captured. Flags: `--limit` value. | `GET /bin/{id}/requests` | (none) |
| `rustybin bins get <id> <n>` | One captured request of a bin. | `GET /bin/{id}/requests/{n}` | (none) |
| `rustybin bins tail <id>` | Follow a bin's requests. Flags: `--max` value, `--timeout` value. | `GET /bin/{id}/requests/stream` | (none) |

```bash
rustybin bins list
rustybin bins create --status 202
rustybin bins delete 0123456789abcdef01234567 --yes
rustybin bins requests 0123456789abcdef01234567
rustybin bins get 0123456789abcdef01234567 1
rustybin bins tail 0123456789abcdef01234567 --max 3
```

## checkpoint

Checkpoints: the instance's API mocks and health, saved and restored

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin checkpoint list [--from-git] [--project P]` | The checkpoints and the mocks each holds; --from-git lists the configuration repository's (after git pull). Flags: `--from-git`, `--project` value. | `GET /_rustybin/checkpoints` | `mocks.read` |
| `rustybin checkpoint show <name>` | One checkpoint. | `GET /_rustybin/checkpoints/{name}` | `mocks.read` |
| `rustybin checkpoint save <name> [--replace] [--project P]` | Save the instance's API mocks (with their data) and the health toggle; --project keeps one project's mocks only. Flags: `--replace`, `--project` value. | `POST /_rustybin/checkpoints` | `checkpoints.write` |
| `rustybin checkpoint restore <name> [--dry-run] [--from-git]` | Bring the API mocks, their data and health back (other API mocks are deleted, flaky counters reset); --dry-run says what would change, --from-git restores the configuration repository's checkpoint. Flags: `--dry-run`, `--from-git`. | `POST /_rustybin/checkpoints/{name}/restore` | `checkpoints.restore` |
| `rustybin checkpoint delete <name>` | Delete a checkpoint (the instance is not changed). | `DELETE /_rustybin/checkpoints/{name}` | `checkpoints.write` |

```bash
rustybin checkpoint list
rustybin checkpoint show demo-start
rustybin checkpoint save demo-start
rustybin checkpoint restore demo-start --yes
rustybin checkpoint delete demo-start --yes
```

## tests

Saved tests: captured requests with assertions, run here, through a gateway or in CI

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin tests list [--project P]` | The saved tests and the variables each uses. Flags: `--project` value. | `GET /_rustybin/tests` | `tests.read` |
| `rustybin tests show <name>` | One saved test. | `GET /_rustybin/tests/{name}` | `tests.read` |
| `rustybin tests save <name> --capture ID [--gateway U] [--description D] [--project P]` | Save a captured request as a test (its status asserted; credentials become {{variables}}). Flags: `--capture` value, `--gateway` value, `--description` value, `--project` value. | `POST /_rustybin/tests` | `tests.write` |
| `rustybin tests put <name> --file F` | Save or replace a test from a file (JSON or YAML: request, gateway, assert). Flags: `--file` value. | `PUT /_rustybin/tests/{name}` | `tests.write` |
| `rustybin tests delete <name>` | Delete a saved test. | `DELETE /_rustybin/tests/{name}` | `tests.write` |
| `rustybin tests run [--names a,b] [--project P] [--var NAME=value]... [--junit]` | Run saved tests (all by default); exits 1 when one fails. --junit prints JUnit XML for CI. Flags: `--names` value, `--var` value, `--junit`, `--project` value. | `POST /_rustybin/tests/run` | `tests.run` |
| `rustybin tests export` | Every saved test as one Hurl file (hurl --test --variable base_url=...). | `GET /_rustybin/tests/export` | `tests.read` |
| `rustybin tests runs [--limit N] [--cursor C]` | The history of test runs, newest first: when, the trigger, who, the counts, what changed since the previous run. Flags: `--limit` value, `--cursor` value. | `GET /_rustybin/tests/runs` | `tests.read` |
| `rustybin tests run-show <id> [--junit]` | One kept run: each test's result and failures, the change since the previous run; --junit prints its JUnit XML. Flags: `--junit`. | `GET /_rustybin/tests/runs/{id}` | `tests.read` |
| `rustybin tests runs-clear` | Forget every kept test run (the tests stay). | `DELETE /_rustybin/tests/runs` | `tests.write` |
| `rustybin tests schedules` | The saved tests on a schedule: when, what, on or off, the last result, the next time, the last notice. | `GET /_rustybin/tests/schedules` | `tests.read` |
| `rustybin tests schedule-show <name>` | One test schedule with its status and recent outcomes. | `GET /_rustybin/tests/schedules/{name}` | `tests.read` |
| `rustybin tests schedule-save <name> (--every D \| --at HH:MM) [--names a,b \| --prefix P] [--webhook U] [--notify-recovery] [--disabled]` | Save or replace a test schedule: every 5m to 24h, or at HH:MM UTC; every saved test unless --names or --prefix. Flags: `--every` value, `--at` value, `--names` value, `--prefix` value, `--webhook` value, `--notify-recovery`, `--disabled`, `--description` value. | `PUT /_rustybin/tests/schedules/{name}` | `tests.write` |
| `rustybin tests schedule-run <name>` | Run a test schedule now, as the scheduler would (kept in the history); exits 1 when it fails. | `POST /_rustybin/tests/schedules/{name}/run` | `tests.run` |
| `rustybin tests schedule-delete <name>` | Delete a test schedule (the tests and the history stay). | `DELETE /_rustybin/tests/schedules/{name}` | `tests.write` |

```bash
rustybin tests list
rustybin tests show order-1
rustybin tests save order-1 --capture 0123456789abcdef
rustybin tests put order-1 --file '{"request":{"path":"/status/200"},"assert":[{"status":200}]}'
rustybin tests delete order-1 --yes
rustybin tests run --junit
rustybin tests export
rustybin tests runs --limit 5
rustybin tests run-show 20261103T101500000Z-abc123 --junit
rustybin tests runs-clear --yes
rustybin tests schedules
rustybin tests schedule-show nightly
rustybin tests schedule-save nightly --every 1h --prefix order
rustybin tests schedule-run nightly
rustybin tests schedule-delete nightly --yes
```

## scenario

Guided gateway scenarios: prepare the upstream, check through the gateway, clean up

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin scenario list` | The gateway scenarios (retries, rate limiting, credential injection, timeouts, caching, circuit breakers, request size limits, CORS, header transformation) and your runs. | `GET /_rustybin/scenarios` | `instance.read` |
| `rustybin scenario start <scenario>` | Prepare a run: its upstream and the steps to configure on the gateway. | `POST /_rustybin/scenarios/{id}/runs` | `scenarios.run` |
| `rustybin scenario show <scenario> <run>` | A run: the setup, what it created, the last verdict. | `GET /_rustybin/scenarios/{id}/runs/{run}` | `instance.read` |
| `rustybin scenario check <scenario> <run> --gateway U [--limit N] [--half-open-ms MS] [--size-limit BYTES] [--allowed-origin O] [--refused-origin O] [--upstream-cors] [--add-request-header H] [--add-request-value V] [--remove-request-header H] [--add-response-header H]` | Send the run's test requests through the gateway and compare what the client got with what reached the upstream; exits 1 when an expectation fails. Flags: `--gateway` value, `--limit` value, `--half-open-ms` value, `--size-limit` value, `--allowed-origin` value, `--refused-origin` value, `--upstream-cors`, `--add-request-header` value, `--add-request-value` value, `--remove-request-header` value, `--add-response-header` value. | `POST /_rustybin/scenarios/{id}/runs/{run}/check` | `scenarios.run` |
| `rustybin scenario end <scenario> <run>` | End a run: remove the mock and counters it created, nothing else. | `DELETE /_rustybin/scenarios/{id}/runs/{run}` | `scenarios.run` |

```bash
rustybin scenario list
rustybin scenario start caching
rustybin scenario show caching 1a2b3c4d
rustybin scenario check caching 1a2b3c4d --gateway http://localhost:8000/catalog-api
rustybin scenario end caching 1a2b3c4d --yes
```

## agents

AI agents on the management MCP server

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin agents list [--owner U]` | Every AI agent that called the management server, the user each acts for, and the policy (yours only without agents.read). Flags: `--owner` value. | `GET /_rustybin/agents` | signed in |
| `rustybin agents get <id>` | One agent: its user, tools, last errors, sessions. | `GET /_rustybin/agents/{id}` | signed in |
| `rustybin agents rights <id> [--ceiling C] [--allow T] [--deny T] [--note N] [--reset]` | Limit an agent: a ceiling (a role or a 0.3 ceiling), tools allowed or denied, a note; --reset goes back to the rules. Your own agents only to limit them, without agents.manage. Flags: `--ceiling` value, `--allow` value, `--deny` value, `--note` value, `--reset`. | `PATCH /_rustybin/agents/{id}` | signed in |
| `rustybin agents revoke <id>` | Refuse the agent's calls, end its sessions, revoke its tokens. | `POST /_rustybin/agents/{id}/revoke` | signed in |
| `rustybin agents restore <id>` | Undo a revocation. | `POST /_rustybin/agents/{id}/restore` | signed in |
| `rustybin agents end-sessions <id>` | End the agent's MCP sessions. | `DELETE /_rustybin/agents/{id}/sessions` | signed in |
| `rustybin agents tail` | Follow the agents' calls as they happen. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/agents/events` | signed in |

```bash
rustybin agents list
rustybin agents get 1f3a...
rustybin agents rights 1f3a... --ceiling viewer --deny try_request
rustybin agents revoke 1f3a... --yes
rustybin agents restore 1f3a...
rustybin agents end-sessions 1f3a...
rustybin agents tail --max 10
```

## ai

AI assist: status, models, usage, test, run a task

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin ai status` | Whether AI assist is on, the provider, the caps. | `GET /_rustybin/assist` | `assist.use` |
| `rustybin ai models [--load]` | The configured provider's models with tier, context, price, whether each reasons and the jobs it runs; --load asks the provider for its list with the stored key first (admin). Flags: `--load`. | `GET /_rustybin/assist/models`, `POST /_rustybin/assist/models` | `assist.use`, `settings.write` |
| `rustybin ai jobs` | Which model each AI job (task) runs on, and whether a setting chose it. | `GET /_rustybin/assist/models` | `assist.use` |
| `rustybin ai use <model> [--job TASK] [--fast]` | Choose a model: the model for the capable jobs, with --fast the fast model, with --job the model of one job (empty with --job clears the choice). Flags: `--job` value, `--fast`. | `PATCH /_rustybin/settings` | `settings.write` |
| `rustybin ai usage` | Calls and tokens since start. | `GET /_rustybin/assist/usage` | `settings.read` |
| `rustybin ai test` | Call the provider once with the configured key. | `POST /_rustybin/assist/test` | `settings.write` |
| `rustybin ai run <task> [--input JSON] [--model M]` | Run an assist task (free_answer, explain, suggest, ...) and print its answer. Flags: `--input` value, `--model` value. | `POST /_rustybin/assist/run` | `assist.use` |

```bash
rustybin ai status
rustybin ai models --load
rustybin ai jobs
rustybin ai use gpt-5-mini --job explain
rustybin ai usage
rustybin ai test
rustybin ai run free_answer --input '{"question": "what does /delay do?"}'
```

## mcp

The MCP inspector: servers, saved connections, checks, calls through the relay

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin mcp servers` | This instance's MCP servers. | `GET /_rustybin/mcp-servers` | `mcp.inspect` |
| `rustybin mcp connections` | The saved connections. | `GET /_rustybin/mcp-connections` | `mcp.inspect` |
| `rustybin mcp connection <id>` | One saved connection. | `GET /_rustybin/mcp-connections/{id}` | `mcp.inspect` |
| `rustybin mcp save-connection <id> --target URL` | Save a connection (its secrets sealed on the instance). Flags: `--target` value, `--name` value, `--transport` value, `--mode` value, `--auth` value, `--secret` value. | `PUT /_rustybin/mcp-connections/{id}` | `mcp.connections` |
| `rustybin mcp delete-connection <id>` | Forget a saved connection. | `DELETE /_rustybin/mcp-connections/{id}` | `mcp.connections` |
| `rustybin mcp check <target>` | Run the checks against a server (--junit writes a report). Flags: `--transport` value, `--version` value, `--header` value, `--connection` value, `--checks` value, `--junit` value. | `POST /_rustybin/mcp-checks` | `mcp.inspect` |
| `rustybin mcp runs` | The saved check runs. | `GET /_rustybin/mcp-checks` | `mcp.inspect` |
| `rustybin mcp run <id>` | One saved check run. | `GET /_rustybin/mcp-checks/{id}` | `mcp.inspect` |
| `rustybin mcp delete-run <id>` | Forget a saved check run. | `DELETE /_rustybin/mcp-checks/{id}` | `mcp.connections` |
| `rustybin mcp call <target> <method> [--params JSON]` | Send one JSON-RPC message to a server through the instance's relay. Flags: `--params` value, `--transport` value, `--header` value, `--connection` value. | `POST /_rustybin/mcp-relay` | `mcp.inspect` |
| `rustybin mcp tools <target>` | The tools a server offers (tools/list through the relay). Flags: `--header` value, `--connection` value. | `POST /_rustybin/mcp-relay` | `mcp.inspect` |
| `rustybin mcp fetch <url>` | Fetch an OAuth discovery document through the instance. | `POST /_rustybin/mcp-relay/fetch` | `mcp.inspect` |
| `rustybin mcp end-relay <session>` | End a relay session. | `DELETE /_rustybin/mcp-relay/{session}` | `mcp.inspect` |

```bash
rustybin mcp servers
rustybin mcp connections
rustybin mcp connection acme
rustybin mcp save-connection acme --target https://mcp.acme.example/mcp --auth '{"kind": "bearer"}' --secret bearer=- < token.txt
rustybin mcp delete-connection acme --yes
rustybin mcp check http://localhost:8080/mcp --junit mcp.xml
rustybin mcp runs
rustybin mcp run 3
rustybin mcp delete-run 3 --yes
rustybin mcp call http://localhost:8080/mcp tools/call --params '{"name": "echo", "arguments": {"message": "hi"}}'
rustybin mcp tools http://localhost:8080/mcp
rustybin mcp fetch https://mcp.acme.example/.well-known/oauth-authorization-server
rustybin mcp end-relay 5b2c
```

## registry

The mock MCP registry and registry browsing

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin registry status` | The mock registry: scenario, entries, capacity. | `GET /_rustybin/registry` | `instance.read` |
| `rustybin registry publish <file>` | Publish a server.json to the mock registry (through its publish API). |  | (none) |
| `rustybin registry delete <name> [--version V]` | Delete a published entry. Flags: `--version` value. | `DELETE /_rustybin/registry/servers/{serverName}` | `registry.write` |
| `rustybin registry validate <file>` | Validate a server.json against the schema and the semantic checks. | `POST /_rustybin/registry/validate` | `instance.read` |
| `rustybin registry browse [--registry U] [--search S] [--cursor C]` | Browse a registry (the official one by default) through the instance. Flags: `--registry` value, `--search` value, `--cursor` value. | `GET /_rustybin/registry/browse` | `mcp.inspect` |

```bash
rustybin registry status
rustybin registry publish server.json
rustybin registry delete io.example/orders --yes
rustybin registry validate server.json
rustybin registry browse --search weather
```

## portal

The dev portal

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin portal status` | The portal: on or off, who may read it, how many users read closed items, each item and its access (open or closed). | `GET /_rustybin/portal` | `instance.read` |
| `rustybin portal publish <id> [--access open\|closed] [--secure]` | Publish an item: mock:NAME, mcp:SERVER, llm, a2a:AGENT, tools or registry, open or closed (a closed mock needs an API key or Basic user of its own on every call; --secure sets random ones). Flags: `--title` value, `--summary` value, `--tags` value, `--contact` value, `--order` value, `--no-credentials`, `--access` value, `--secure`. | `PUT /_rustybin/portal/items/{id}` | `portal.publish` |
| `rustybin portal unpublish <id>` | Remove an item from the portal. | `DELETE /_rustybin/portal/items/{id}` | `portal.publish` |
| `rustybin portal insights` | Views, downloads and Try it calls. | `GET /_rustybin/portal/insights` | `instance.read` |
| `rustybin portal branding` | The portal.* settings. | `GET /_rustybin/settings` | `settings.read` |
| `rustybin portal open` | The portal's URL. |  | (none) |

```bash
rustybin portal status
rustybin portal publish mock:orders --title "Orders API" --tags payments
rustybin portal unpublish mock:orders --yes
rustybin portal insights
rustybin portal branding
rustybin portal open
```

## config

The whole instance as rustybin.yaml

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin config export [--format F] [--secrets S] [--output FILE]` | The whole instance as rustybin.yaml (yaml, json, or a bundle to a file). Flags: `--format` value, `--secrets` value, `--output` value. | `GET /_rustybin/config/export` | `config.export` |
| `rustybin config apply <file> [--dry-run] [--prune]` | Apply a document; a dry run shows the changes and touches nothing. Flags: `--dry-run`, `--prune`. | `POST /_rustybin/config/apply` | `config.apply` |
| `rustybin config diff <file>` | What applying a document would change (a dry run). Flags: `--prune`. | `POST /_rustybin/config/apply` | `config.apply` |
| `rustybin config schema` | The JSON Schema of rustybin.yaml, version 2. | `GET /_rustybin/config/schema` | `instance.read` |
| `rustybin config check <file>` | Check a document offline: every section parses, every mock compiles. |  | (none) |

```bash
rustybin config export --output rustybin.yaml
rustybin config apply rustybin.yaml --dry-run
rustybin config diff rustybin.yaml
rustybin config schema
rustybin config check rustybin.yaml
```

## git

Source control

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin git status [--fetch]` | The repository, the working copy, the drift, the timeline. Flags: `--fetch`. | `GET /_rustybin/git` | `git.read` |
| `rustybin git init` | Connect the repository (clone, or start an empty working copy). | `POST /_rustybin/git/init` | `git.write` |
| `rustybin git commit -m "<message>" [--checkpoint NAME]...` | Export the instance into the working copy and commit; --checkpoint also commits a local checkpoint (repeat it, or a,b). Flags: `--message` value, `-m` value, `--checkpoint` value. | `POST /_rustybin/git/commit` | `git.write` |
| `rustybin git push` | Push the branch. | `POST /_rustybin/git/push` | `git.write` |
| `rustybin git pull [--apply] [--dry-run] [--prune]` | Fetch and fast-forward; --apply applies the repository's document. Flags: `--apply`, `--dry-run`, `--prune`. | `POST /_rustybin/git/pull` | `git.write` |
| `rustybin git resolve --with instance\|repository` | After a diverged branch: the instance's copy wins, or the repository's. Flags: `--with` value, `--message` value. | `POST /_rustybin/git/resolve` | `git.write` |
| `rustybin git key [--generate] [--replace]` | The deploy key's public half; --generate makes one. Flags: `--generate`, `--replace`. | `GET /_rustybin/git/key`, `POST /_rustybin/git/key` | `git.read`, `git.write` |
| `rustybin git history` | The last commits of the working copy. | `GET /_rustybin/git/history` | `git.read` |

```bash
rustybin git status --fetch
rustybin git init
rustybin git commit -m "Add the orders mock"
rustybin git push
rustybin git pull --apply --dry-run
rustybin git resolve --with repository --yes
rustybin git key --generate
rustybin git history
```

## harness

Load tests on the instance (--remote); validate, run and report work offline too

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin harness status` | Harness mode: roles, caps, runs, the kill switch. | `GET /_rustybin/harness` | `harness.read` |
| `rustybin harness schema` | The JSON Schema of a scenario. | `GET /_rustybin/harness/schema` | `harness.read` |
| `rustybin harness scenarios` | The built-in and saved scenarios. | `GET /_rustybin/harness/scenarios` | `harness.read` |
| `rustybin harness scenario <name> [--yaml]` | One scenario. Flags: `--yaml`. | `GET /_rustybin/harness/scenarios/{name}` | `harness.read` |
| `rustybin harness save-scenario <name> <file>` | Save a scenario under a name. | `PUT /_rustybin/harness/scenarios/{name}` | `harness.run` |
| `rustybin harness delete-scenario <name>` | Delete a saved scenario. | `DELETE /_rustybin/harness/scenarios/{name}` | `harness.run` |
| `rustybin harness validate <file> --remote` | Validate a scenario on the instance (without --remote: offline, in this process). Flags: `--remote`. | `POST /_rustybin/harness/validate` | `harness.read` |
| `rustybin harness preview <file>` | The run plan of a scenario: stages, requests, duration. | `POST /_rustybin/harness/preview` | `harness.read` |
| `rustybin harness runs` | The runs on this node. | `GET /_rustybin/harness/runs` | `harness.read` |
| `rustybin harness run <file> --remote [--wait]` | Start a run on the instance (without --remote: from this process); --wait follows it and prints the report. Flags: `--remote`, `--wait`, `--format` value. | `POST /_rustybin/harness/runs` | `harness.run` |
| `rustybin harness show <run>` | One run: state, progress, the live controls. | `GET /_rustybin/harness/runs/{run}` | `harness.read` |
| `rustybin harness tune <run> <change>` | A live change (JSON or a file): rate, users, pause, resume, ... | `PATCH /_rustybin/harness/runs/{run}` | `harness.run` |
| `rustybin harness stop <run>` | Stop a run. | `POST /_rustybin/harness/runs/{run}/stop` | `harness.run` |
| `rustybin harness stop-all` | The kill switch: stop every run on the node. | `POST /_rustybin/harness/stop` | `harness.run` |
| `rustybin harness events <run>` | Follow a run's events. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/harness/runs/{run}/events` | `harness.read` |
| `rustybin harness report <run> --remote [--format F]` | A run's report (without --remote: render a saved report file). Flags: `--remote`, `--format` value. | `GET /_rustybin/harness/runs/{run}/report` | `harness.read` |
| `rustybin harness clusters` | The clusters this node leads. | `GET /_rustybin/harness/clusters` | `harness.read` |
| `rustybin harness create-cluster [--body JSON]` | Create a cluster (cluster_id, min_nodes, lease, join_token). Flags: `--body` value. | `POST /_rustybin/harness/clusters` | `harness.cluster` |
| `rustybin harness cluster <id>` | A cluster: nodes, leader, runs. | `GET /_rustybin/harness/clusters/{id}` | `harness.read` |
| `rustybin harness delete-cluster <id>` | Dissolve a cluster. | `DELETE /_rustybin/harness/clusters/{id}` | `harness.cluster` |
| `rustybin harness leader <id>` | The cluster's leader. | `GET /_rustybin/harness/clusters/{id}/leader` | `harness.read` |
| `rustybin harness step-aside <id>` | Before maintenance: the leader hands the cluster to the first in the succession, with the run still on. | `POST /_rustybin/harness/clusters/{id}/leader/step-aside` | `harness.cluster` |
| `rustybin harness upstreams <id>` | The cluster's server nodes as the gateway reaches them. | `GET /_rustybin/harness/clusters/{id}/upstreams` | `harness.read` |
| `rustybin harness cluster-runs <id>` | The cluster's runs. | `GET /_rustybin/harness/clusters/{id}/runs` | `harness.read` |
| `rustybin harness cluster-run <id> <file>` | Create a cluster run from a scenario (start it with cluster-start). | `POST /_rustybin/harness/clusters/{id}/runs` | `harness.run` |
| `rustybin harness cluster-show <id> <run>` | One cluster run. | `GET /_rustybin/harness/clusters/{id}/runs/{run}` | `harness.read` |
| `rustybin harness cluster-tune <id> <run> <change>` | A live change on a cluster run. | `PATCH /_rustybin/harness/clusters/{id}/runs/{run}` | `harness.run` |
| `rustybin harness cluster-start <id> <run>` | Start a cluster run. | `POST /_rustybin/harness/clusters/{id}/runs/{run}/start` | `harness.run` |
| `rustybin harness cluster-stop <id> <run>` | Stop a cluster run. | `POST /_rustybin/harness/clusters/{id}/runs/{run}/stop` | `harness.run` |
| `rustybin harness cluster-events <id> <run>` | Follow a cluster run's events. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/harness/clusters/{id}/runs/{run}/events` | `harness.read` |
| `rustybin harness cluster-report <id> <run> [--format F]` | A cluster run's report. Flags: `--format` value. | `GET /_rustybin/harness/clusters/{id}/runs/{run}/report` | `harness.read` |
| `rustybin harness join <cluster-url> --join-token T` | Make this node join a cluster. Flags: `--join-token` value, `--advertise` value. | `POST /_rustybin/harness/join` | `harness.cluster` |
| `rustybin harness leave` | Leave the cluster. | `POST /_rustybin/harness/leave` | `harness.cluster` |

```bash
rustybin harness status
rustybin harness schema
rustybin harness scenarios
rustybin harness scenario smoke --yaml
rustybin harness save-scenario my-smoke smoke.yaml
rustybin harness delete-scenario my-smoke --yes
rustybin harness validate smoke.yaml --remote
rustybin harness preview smoke.yaml
rustybin harness runs
rustybin harness run smoke.yaml --remote --wait
rustybin harness show r-1
rustybin harness tune r-1 '{"rate": 50}'
rustybin harness stop r-1
rustybin harness stop-all --yes
rustybin harness events r-1
rustybin harness report r-1 --remote --format junit
rustybin harness clusters
rustybin harness create-cluster --body '{"min_nodes": {"clients": 2}}'
rustybin harness cluster c-1
rustybin harness delete-cluster c-1 --yes
rustybin harness leader c-1
rustybin harness step-aside perf-1
rustybin harness upstreams c-1
rustybin harness cluster-runs c-1
rustybin harness cluster-run c-1 smoke.yaml
rustybin harness cluster-show c-1 cr-1
rustybin harness cluster-tune c-1 cr-1 '{"rate": 50}'
rustybin harness cluster-start c-1 cr-1
rustybin harness cluster-stop c-1 cr-1
rustybin harness cluster-events c-1 cr-1
rustybin harness cluster-report c-1 cr-1
rustybin harness join https://leader.example/_rustybin/harness/clusters/c-1 --join-token ...
rustybin harness leave
```

## keys

API keys in shared mode

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin keys usage [--day D] [--after A] [--limit N]` | Per-key usage in shared mode. Flags: `--day` value, `--after` value, `--limit` value. | `GET /_rustybin/keys/usage` | `settings.read` |

```bash
rustybin keys usage --day 2026-10-30
```

## egress

The outbound connection policy

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin egress show` | The outbound policy: the allowlist, each feature's rule, the last decisions. | `GET /_rustybin/egress` | `settings.read` |
| `rustybin egress check <url> [--purpose P]` | What the outbound policy would say to a URL, without calling it. Exits 1 when it would refuse. Flags: `--purpose` value. | `GET /_rustybin/egress/check` | `settings.read` |

```bash
rustybin egress show
rustybin egress check https://api.example.com --purpose git
```

## audit

The audit log: who changed what, every sign-in and every refusal

| Command | What it does | Calls | Needs |
|---|---|---|---|
| `rustybin audit list [--actor A] [--action A] [--target T] [--since S] [--until U] [--result R] [--limit N] [--cursor C]` | The audit log, newest first: who did what and when, sign-ins and refusals (audit.read). --cursor pages back with the next_cursor printed last. Flags: `--actor` value, `--action` value, `--target` value, `--since` value, `--until` value, `--result` value, `--limit` value, `--cursor` value. | `GET /_rustybin/audit` | `audit.read` |
| `rustybin audit tail [--actor A] [--action A] [--target T] [--result R]` | Follow the audit log: one line per new entry that matches (audit.read). Flags: `--actor` value, `--action` value, `--target` value, `--result` value, `--max` value, `--timeout` value. | `GET /_rustybin/audit/stream` | `audit.read` |

```bash
rustybin audit list --since 1h --result refused
rustybin audit tail --action mocks --max 10
```

## Not covered by a command

The management MCP endpoint (`rustybin mcp-stdio` bridges a client to it), the agent sign-in (OAuth) routes a browser walks, the harness cluster's node-to-node routes, a request bin's capture URLs and a mock's data plane.
