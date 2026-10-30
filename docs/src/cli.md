# CLI reference

Generated from the command tree (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`); the [CLI guide](cli-guide.md) says how to sign in, use profiles and script it, and the [mocks CLI](mocks/cli.md) covers the offline `rustybin mock` commands.

`rustybin <noun> <verb> [args] [flags]`. Every command takes the global flags; `--json` prints the instance's answer as it is, `--yes` skips a confirmation. Exit codes: `0` done, `1` refused or failed, `2` usage, `3` the network or the credentials.

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

| Command | What it does | Calls |
|---|---|---|
| `rustybin login [--url U] [--user U] [--password-stdin] [--profile P]` | Sign in with the console user name and password (prompted, or the first line of stdin) and keep the token in a profile; --token keeps a token you already have. Flags: `--user` value, `--password-stdin`. | `POST /_rustybin/login`, `GET /_rustybin/session` |

```bash
rustybin login --url https://mocks.example.com --user rustybin
```

## logout

Revoke the profile's token and forget it

| Command | What it does | Calls |
|---|---|---|
| `rustybin logout` | Revoke the profile's token on the instance and forget it. | `POST /_rustybin/logout` |

```bash
rustybin logout
```

## whoami

Who the current credential is

| Command | What it does | Calls |
|---|---|---|
| `rustybin whoami` | The credential the calls carry: user, scopes, expiry. | `GET /_rustybin/session` |

```bash
rustybin whoami
```

## status

The instance's status

| Command | What it does | Calls |
|---|---|---|
| `rustybin status` | Name, version, modes, health, the features that are on. | `GET /_rustybin/status` |

```bash
rustybin status
```

## instance

Version, readiness, the running configuration, the catalogue, metrics, usage

| Command | What it does | Calls |
|---|---|---|
| `rustybin instance version` | Version and build. | `GET /_rustybin/version` |
| `rustybin instance ready` | Readiness (never authenticated). | `GET /_rustybin/ready` |
| `rustybin instance config` | The running configuration (no secrets). | `GET /_rustybin/config` |
| `rustybin instance catalog [--category C]` | The route catalogue. Flags: `--category` value. | `GET /_rustybin/catalog` |
| `rustybin instance metrics` | Prometheus metrics, as text. | `GET /_rustybin/metrics` |
| `rustybin instance usage` | Plan limits and this client's usage. | `GET /_rustybin/usage` |

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

| Command | What it does | Calls |
|---|---|---|
| `rustybin profiles list` | The profiles and the default. |  |
| `rustybin profiles use <name>` | Make a profile the default. |  |
| `rustybin profiles remove <name>` | Forget a profile. |  |

```bash
rustybin profiles list
rustybin profiles use mocks.example.com
rustybin profiles remove old-instance
```

## settings

Instance settings: get, set, secrets, unset, the schema

| Command | What it does | Calls |
|---|---|---|
| `rustybin settings get [key]` | Every setting with its value and source, or one. | `GET /_rustybin/settings` |
| `rustybin settings set <key> <value>` | Save a setting (JSON values are parsed, anything else is a string). | `PATCH /_rustybin/settings` |
| `rustybin settings set-secret <key> [--stdin]` | Save a secret (prompted without echo, or the first line of stdin). Flags: `--stdin`. | `PATCH /_rustybin/settings` |
| `rustybin settings unset <key>` | Forget the stored value (back to the environment or the default). | `DELETE /_rustybin/settings/{key}` |
| `rustybin settings schema` | Every registered setting: key, kind, default, variable. | `GET /_rustybin/settings` |

```bash
rustybin settings get console.title
rustybin settings set console.title "Payments mocks"
rustybin settings set-secret assist.openai_key --stdin < key.txt
rustybin settings unset console.title
rustybin settings schema
```

## sessions

The console's sign-in sessions

| Command | What it does | Calls |
|---|---|---|
| `rustybin sessions list` | The sign-in sessions since the last restart. | `GET /_rustybin/sessions` |
| `rustybin sessions end <id>` | End one session. | `DELETE /_rustybin/sessions/{id}` |
| `rustybin sessions end-all` | Sign out everywhere (every token issued so far). | `DELETE /_rustybin/sessions` |

```bash
rustybin sessions list
rustybin sessions end 3f2a
rustybin sessions end-all --yes
```

## password

Change the console user name and password

| Command | What it does | Calls |
|---|---|---|
| `rustybin password change [--user U] [--stdin]` | Change the console password (and user name): the current and the new one prompted, or two lines of stdin. Flags: `--user` value, `--stdin`. | `PUT /_rustybin/password` |

```bash
rustybin password change --stdin < passwords.txt
```

## mocks

OpenAPI mocks on the instance (rustybin mock ... works offline)

| Command | What it does | Calls |
|---|---|---|
| `rustybin mocks list [--all]` | The mocks with their state and traffic. Flags: `--all`. | `GET /_rustybin/mocks` |
| `rustybin mocks get <name>` | One mock: settings, operations, the deploy report. | `GET /_rustybin/mocks/{name}` |
| `rustybin mocks create <name> --spec FILE` | Create a mock from an OpenAPI file (YAML or JSON). Flags: `--spec` value, `--mode` value, `--disabled`, `--no-deploy`, `--strip-server-prefix`. | `POST /_rustybin/mocks` |
| `rustybin mocks import --spec FILE` | Preview a spec: title, operations, the checks, the suggested name. Flags: `--spec` value. | `POST /_rustybin/mocks/import` |
| `rustybin mocks update <name> [--spec FILE] [--mode M] [--settings JSON]` | Change a mock's spec or settings (the draft, deployed unless --no-deploy). Flags: `--spec` value, `--mode` value, `--settings` value, `--no-deploy`, `--force`. | `PATCH /_rustybin/mocks/{name}` |
| `rustybin mocks deploy <name> [--force]` | Deploy the draft. Flags: `--force`. | `POST /_rustybin/mocks/{name}/deploy` |
| `rustybin mocks rollback <name> [--revision N]` | Go back to the previous (or a given) revision. Flags: `--revision` value. | `POST /_rustybin/mocks/{name}/rollback` |
| `rustybin mocks enable <name>` | Switch a mock on. | `POST /_rustybin/mocks/{name}/enable` |
| `rustybin mocks disable <name>` | Switch a mock off. | `POST /_rustybin/mocks/{name}/disable` |
| `rustybin mocks delete <name>` | Delete a mock. | `DELETE /_rustybin/mocks/{name}` |
| `rustybin mocks duplicate <name> [--name NEW]` | Copy a mock. Flags: `--name` value. | `POST /_rustybin/mocks/{name}/duplicate` |
| `rustybin mocks check <name>` | The deploy report: a valid request and response per operation. | `GET /_rustybin/mocks/{name}/report` |
| `rustybin mocks coverage <name>` | Which operations lack an example. | `GET /_rustybin/mocks/{name}/coverage` |
| `rustybin mocks spec <name>` | The mock's OpenAPI document as stored. | `GET /_rustybin/mocks/{name}/spec` |
| `rustybin mocks export [name]` | One mock, or all, as a version 1 rustybin.yaml. | `GET /_rustybin/mocks/{name}/export`, `GET /_rustybin/mocks/export` |
| `rustybin mocks apply <file> [--prune]` | Apply a rustybin.yaml (spec files are read locally and inlined); --prune deletes API mocks it does not name. Flags: `--prune`. | `POST /_rustybin/mocks/apply` |
| `rustybin mocks reload` | Reload the file-defined mocks (RUSTYBIN_CONFIG_FILE, RUSTYBIN_MOCKS_DIR). | `POST /_rustybin/mocks/reload` |
| `rustybin mocks schema` | The JSON Schema of rustybin.yaml. | `GET /_rustybin/mocks/config.schema.json` |
| `rustybin mocks sample <name> <operationId>` | A valid response for an operation. Flags: `--status` value, `--seed` value, `--mode` value, `--example` value. | `GET /_rustybin/mocks/{name}/operations/{op}/sample` |
| `rustybin mocks validate <name> <operationId> --body FILE` | Validate a request or response body against the spec. Flags: `--body` value, `--kind` value, `--status` value. | `POST /_rustybin/mocks/{name}/operations/{op}/validate` |
| `rustybin mocks operation <name> <operationId>` | One operation's settings and statistics. | `GET /_rustybin/mocks/{name}/operations/{op}` |
| `rustybin mocks set-operation <name> <operationId> --settings JSON` | Set an operation's settings (respond, rules, mode, delay_ms, public). Flags: `--settings` value. | `PUT /_rustybin/mocks/{name}/operations/{op}` |
| `rustybin mocks test-rules <name> <operationId> [--rules FILE] [--request N]` | Try an operation's rules (or given ones) on a captured request. Flags: `--rules` value, `--request` value. | `POST /_rustybin/mocks/{name}/operations/{op}/rules/test` |
| `rustybin mocks traffic <name> [--limit N]` | The mock's captured requests. Flags: `--limit` value. | `GET /_rustybin/mocks/{name}/requests` |
| `rustybin mocks traffic-get <name> <n>` | One captured request of the mock. | `GET /_rustybin/mocks/{name}/requests/{n}` |
| `rustybin mocks traffic-tail <name>` | Follow the mock's requests as they arrive. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/mocks/{name}/requests/stream` |
| `rustybin mocks traffic-clear <name>` | Forget the mock's captured requests. | `DELETE /_rustybin/mocks/{name}/requests` |
| `rustybin mocks data <name>` | The items a stateful mock stored. | `GET /_rustybin/mocks/{name}/state` |
| `rustybin mocks data-clear <name>` | Forget the items a stateful mock stored. | `DELETE /_rustybin/mocks/{name}/state` |
| `rustybin mocks collection <name> <format>` | The mock as a postman, hurl, curl, http, k6, bruno or insomnia collection. Flags: `--base-url` value. | `GET /_rustybin/mocks/{name}/collections/{format}` |
| `rustybin mocks describe "<description>"` | An OpenAPI spec from a description (AI), printed as JSON. Flags: `--openapi-version` value, `--model` value. | `POST /_rustybin/mocks/ai/spec` |
| `rustybin mocks examples <name> [--apply]` | Example payloads for the operations that lack one (AI). Flags: `--operations` value, `--per-response` value, `--persona` value, `--source` value, `--apply`. | `POST /_rustybin/mocks/{name}/ai/examples` |
| `rustybin mocks edit <name> "<instruction>" [--apply]` | Edit a spec with an instruction (AI): the diff, applied with --apply. Flags: `--apply`, `--model` value. | `POST /_rustybin/mocks/{name}/ai/edit` |

```bash
rustybin mocks list
rustybin mocks get petstore
rustybin mocks create orders --spec orders.yaml --mode generated
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

| Command | What it does | Calls |
|---|---|---|
| `rustybin visits list` | Every self-service visit. | `GET /_rustybin/mocks/visits` |
| `rustybin visits end <id>` | End a visit (its mocks go). | `DELETE /_rustybin/mocks/visits/{id}` |
| `rustybin visits end-all` | End every visit. | `DELETE /_rustybin/mocks/visits` |
| `rustybin visits pause` | Stop accepting new visits. | `POST /_rustybin/mocks/visits/pause` |
| `rustybin visits resume` | Accept new visits again. | `POST /_rustybin/mocks/visits/resume` |

```bash
rustybin visits list
rustybin visits end 9c1e --yes
rustybin visits end-all --yes
rustybin visits pause
rustybin visits resume
```

## visit

Your own self-service visit, as a visitor

| Command | What it does | Calls |
|---|---|---|
| `rustybin visit get` | Your visit (needs --mock-token or RUSTYBIN_MOCK_TOKEN). | `GET /_rustybin/mocks/visit` |
| `rustybin visit start [--email E]` | Start a visit; the manage token is shown once. Flags: `--email` value. | `POST /_rustybin/mocks/visit` |
| `rustybin visit extend` | Extend your visit. | `POST /_rustybin/mocks/visit/extend` |
| `rustybin visit rotate-token` | A new manage token for your visit. | `POST /_rustybin/mocks/visit/token` |
| `rustybin visit end` | End your visit. | `DELETE /_rustybin/mocks/visit` |

```bash
rustybin visit get --mock-token rbm_...
rustybin visit start
rustybin visit extend --mock-token rbm_...
rustybin visit rotate-token --mock-token rbm_...
rustybin visit end --mock-token rbm_... --yes
```

## requests

The request inspector

| Command | What it does | Calls |
|---|---|---|
| `rustybin requests list [--limit N] [--path-prefix P]` | The captured requests. Flags: `--limit` value, `--path-prefix` value. | `GET /_rustybin/requests` |
| `rustybin requests get <id>` | One captured request. | `GET /_rustybin/requests/{id}` |
| `rustybin requests tail` | Follow the requests as they arrive. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/requests/stream` |
| `rustybin requests clear` | Forget the captured requests. | `DELETE /_rustybin/requests` |

```bash
rustybin requests list --limit 20
rustybin requests get 42
rustybin requests tail --max 5
rustybin requests clear --yes
```

## bins

Request bins

| Command | What it does | Calls |
|---|---|---|
| `rustybin bins list` | Your request bins. | `GET /bin` |
| `rustybin bins create [--status N] [--body B] [--delay-ms N]` | A new bin, with the answer it gives. Flags: `--status` value, `--body` value, `--delay-ms` value. | `POST /bin` |
| `rustybin bins delete <id>` | Delete a bin. | `DELETE /bin/{id}` |
| `rustybin bins requests <id> [--limit N]` | What a bin captured. Flags: `--limit` value. | `GET /bin/{id}/requests` |
| `rustybin bins get <id> <n>` | One captured request of a bin. | `GET /bin/{id}/requests/{n}` |
| `rustybin bins tail <id>` | Follow a bin's requests. Flags: `--max` value, `--timeout` value. | `GET /bin/{id}/requests/stream` |

```bash
rustybin bins list
rustybin bins create --status 202
rustybin bins delete 0123456789abcdef01234567 --yes
rustybin bins requests 0123456789abcdef01234567
rustybin bins get 0123456789abcdef01234567 1
rustybin bins tail 0123456789abcdef01234567 --max 3
```

## agents

AI agents on the management MCP server

| Command | What it does | Calls |
|---|---|---|
| `rustybin agents list` | Every AI agent that called the management server, and the policy. | `GET /_rustybin/agents` |
| `rustybin agents get <id>` | One agent: tools, last errors, sessions. | `GET /_rustybin/agents/{id}` |
| `rustybin agents rights <id> [--ceiling C] [--allow T] [--deny T] [--note N] [--reset]` | Limit an agent: a ceiling, tools allowed or denied, a note; --reset goes back to the rules. Flags: `--ceiling` value, `--allow` value, `--deny` value, `--note` value, `--reset`. | `PATCH /_rustybin/agents/{id}` |
| `rustybin agents revoke <id>` | Refuse the agent's calls, end its sessions, revoke its tokens. | `POST /_rustybin/agents/{id}/revoke` |
| `rustybin agents restore <id>` | Undo a revocation. | `POST /_rustybin/agents/{id}/restore` |
| `rustybin agents end-sessions <id>` | End the agent's MCP sessions. | `DELETE /_rustybin/agents/{id}/sessions` |
| `rustybin agents tail` | Follow the agents' calls as they happen. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/agents/events` |

```bash
rustybin agents list
rustybin agents get 1f3a...
rustybin agents rights 1f3a... --ceiling read-only --deny try_request
rustybin agents revoke 1f3a... --yes
rustybin agents restore 1f3a...
rustybin agents end-sessions 1f3a...
rustybin agents tail --max 10
```

## ai

AI assist: status, models, usage, test, run a task

| Command | What it does | Calls |
|---|---|---|
| `rustybin ai status` | Whether AI assist is on, the provider, the caps. | `GET /_rustybin/assist` |
| `rustybin ai models` | The curated models of every provider. | `GET /_rustybin/assist` |
| `rustybin ai usage` | Calls and tokens since start. | `GET /_rustybin/assist/usage` |
| `rustybin ai test` | Call the provider once with the configured key. | `POST /_rustybin/assist/test` |
| `rustybin ai run <task> [--input JSON] [--model M]` | Run an assist task (free_answer, explain, suggest, ...) and print its answer. Flags: `--input` value, `--model` value. | `POST /_rustybin/assist/run` |

```bash
rustybin ai status
rustybin ai models
rustybin ai usage
rustybin ai test
rustybin ai run free_answer --input '{"question": "what does /delay do?"}'
```

## mcp

The MCP inspector: servers, saved connections, checks, calls through the relay

| Command | What it does | Calls |
|---|---|---|
| `rustybin mcp servers` | This instance's MCP servers. | `GET /_rustybin/mcp-servers` |
| `rustybin mcp connections` | The saved connections. | `GET /_rustybin/mcp-connections` |
| `rustybin mcp connection <id>` | One saved connection. | `GET /_rustybin/mcp-connections/{id}` |
| `rustybin mcp save-connection <id> --target URL` | Save a connection (its secrets sealed on the instance). Flags: `--target` value, `--name` value, `--transport` value, `--mode` value, `--auth` value, `--secret` value. | `PUT /_rustybin/mcp-connections/{id}` |
| `rustybin mcp delete-connection <id>` | Forget a saved connection. | `DELETE /_rustybin/mcp-connections/{id}` |
| `rustybin mcp check <target>` | Run the checks against a server (--junit writes a report). Flags: `--transport` value, `--version` value, `--header` value, `--connection` value, `--checks` value, `--junit` value. | `POST /_rustybin/mcp-checks` |
| `rustybin mcp runs` | The saved check runs. | `GET /_rustybin/mcp-checks` |
| `rustybin mcp run <id>` | One saved check run. | `GET /_rustybin/mcp-checks/{id}` |
| `rustybin mcp delete-run <id>` | Forget a saved check run. | `DELETE /_rustybin/mcp-checks/{id}` |
| `rustybin mcp call <target> <method> [--params JSON]` | Send one JSON-RPC message to a server through the instance's relay. Flags: `--params` value, `--transport` value, `--header` value, `--connection` value. | `POST /_rustybin/mcp-relay` |
| `rustybin mcp tools <target>` | The tools a server offers (tools/list through the relay). Flags: `--header` value, `--connection` value. | `POST /_rustybin/mcp-relay` |
| `rustybin mcp fetch <url>` | Fetch an OAuth discovery document through the instance. | `POST /_rustybin/mcp-relay/fetch` |
| `rustybin mcp end-relay <session>` | End a relay session. | `DELETE /_rustybin/mcp-relay/{session}` |

```bash
rustybin mcp servers
rustybin mcp connections
rustybin mcp connection acme
rustybin mcp save-connection acme --target https://mcp.acme.example/mcp --auth '{"kind": "bearer"}' --secret bearer=TOKEN
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

| Command | What it does | Calls |
|---|---|---|
| `rustybin registry status` | The mock registry: scenario, entries, capacity. | `GET /_rustybin/registry` |
| `rustybin registry publish <file>` | Publish a server.json to the mock registry (through its publish API). |  |
| `rustybin registry delete <name> [--version V]` | Delete a published entry. Flags: `--version` value. | `DELETE /_rustybin/registry/servers/{serverName}` |
| `rustybin registry validate <file>` | Validate a server.json against the schema and the semantic checks. | `POST /_rustybin/registry/validate` |
| `rustybin registry browse [--registry U] [--search S] [--cursor C]` | Browse a registry (the official one by default) through the instance. Flags: `--registry` value, `--search` value, `--cursor` value. | `GET /_rustybin/registry/browse` |

```bash
rustybin registry status
rustybin registry publish server.json
rustybin registry delete io.example/orders --yes
rustybin registry validate server.json
rustybin registry browse --search weather
```

## portal

The dev portal

| Command | What it does | Calls |
|---|---|---|
| `rustybin portal status` | The portal: on or off, items, who may read it. | `GET /_rustybin/portal` |
| `rustybin portal publish <id>` | Publish an item: mock:NAME, mcp:SERVER, llm, a2a:AGENT, tools or registry. Flags: `--title` value, `--summary` value, `--tags` value, `--contact` value, `--order` value, `--no-credentials`. | `PUT /_rustybin/portal/items/{id}` |
| `rustybin portal unpublish <id>` | Remove an item from the portal. | `DELETE /_rustybin/portal/items/{id}` |
| `rustybin portal insights` | Views, downloads and Try it calls. | `GET /_rustybin/portal/insights` |
| `rustybin portal branding` | The portal.* settings. | `GET /_rustybin/settings` |
| `rustybin portal open` | The portal's URL. |  |

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

| Command | What it does | Calls |
|---|---|---|
| `rustybin config export [--format F] [--secrets S] [--output FILE]` | The whole instance as rustybin.yaml (yaml, json, or a bundle to a file). Flags: `--format` value, `--secrets` value, `--output` value. | `GET /_rustybin/config/export` |
| `rustybin config apply <file> [--dry-run] [--prune]` | Apply a document; a dry run shows the changes and touches nothing. Flags: `--dry-run`, `--prune`. | `POST /_rustybin/config/apply` |
| `rustybin config diff <file>` | What applying a document would change (a dry run). Flags: `--prune`. | `POST /_rustybin/config/apply` |
| `rustybin config schema` | The JSON Schema of rustybin.yaml, version 2. | `GET /_rustybin/config/schema` |
| `rustybin config check <file>` | Check a document offline: every section parses, every mock compiles. |  |

```bash
rustybin config export --output rustybin.yaml
rustybin config apply rustybin.yaml --dry-run
rustybin config diff rustybin.yaml
rustybin config schema
rustybin config check rustybin.yaml
```

## git

Source control

| Command | What it does | Calls |
|---|---|---|
| `rustybin git status [--fetch]` | The repository, the working copy, the drift, the timeline. Flags: `--fetch`. | `GET /_rustybin/git` |
| `rustybin git init` | Connect the repository (clone, or start an empty working copy). | `POST /_rustybin/git/init` |
| `rustybin git commit -m "<message>"` | Export the instance into the working copy and commit. Flags: `--message` value, `-m` value. | `POST /_rustybin/git/commit` |
| `rustybin git push` | Push the branch. | `POST /_rustybin/git/push` |
| `rustybin git pull [--apply] [--dry-run] [--prune]` | Fetch and fast-forward; --apply applies the repository's document. Flags: `--apply`, `--dry-run`, `--prune`. | `POST /_rustybin/git/pull` |
| `rustybin git resolve --with instance\|repository` | After a diverged branch: the instance's copy wins, or the repository's. Flags: `--with` value, `--message` value. | `POST /_rustybin/git/resolve` |
| `rustybin git key [--generate] [--replace]` | The deploy key's public half; --generate makes one. Flags: `--generate`, `--replace`. | `GET /_rustybin/git/key`, `POST /_rustybin/git/key` |
| `rustybin git history` | The last commits of the working copy. | `GET /_rustybin/git/history` |

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

| Command | What it does | Calls |
|---|---|---|
| `rustybin harness status` | Harness mode: roles, caps, runs, the kill switch. | `GET /_rustybin/harness` |
| `rustybin harness schema` | The JSON Schema of a scenario. | `GET /_rustybin/harness/schema` |
| `rustybin harness scenarios` | The built-in and saved scenarios. | `GET /_rustybin/harness/scenarios` |
| `rustybin harness scenario <name> [--yaml]` | One scenario. Flags: `--yaml`. | `GET /_rustybin/harness/scenarios/{name}` |
| `rustybin harness save-scenario <name> <file>` | Save a scenario under a name. | `PUT /_rustybin/harness/scenarios/{name}` |
| `rustybin harness delete-scenario <name>` | Delete a saved scenario. | `DELETE /_rustybin/harness/scenarios/{name}` |
| `rustybin harness validate <file> --remote` | Validate a scenario on the instance (without --remote: offline, in this process). Flags: `--remote`. | `POST /_rustybin/harness/validate` |
| `rustybin harness preview <file>` | The run plan of a scenario: stages, requests, duration. | `POST /_rustybin/harness/preview` |
| `rustybin harness runs` | The runs on this node. | `GET /_rustybin/harness/runs` |
| `rustybin harness run <file> --remote [--wait]` | Start a run on the instance (without --remote: from this process); --wait follows it and prints the report. Flags: `--remote`, `--wait`, `--format` value. | `POST /_rustybin/harness/runs` |
| `rustybin harness show <run>` | One run: state, progress, the live controls. | `GET /_rustybin/harness/runs/{run}` |
| `rustybin harness tune <run> <change>` | A live change (JSON or a file): rate, users, pause, resume, ... | `PATCH /_rustybin/harness/runs/{run}` |
| `rustybin harness stop <run>` | Stop a run. | `POST /_rustybin/harness/runs/{run}/stop` |
| `rustybin harness stop-all` | The kill switch: stop every run on the node. | `POST /_rustybin/harness/stop` |
| `rustybin harness events <run>` | Follow a run's events. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/harness/runs/{run}/events` |
| `rustybin harness report <run> --remote [--format F]` | A run's report (without --remote: render a saved report file). Flags: `--remote`, `--format` value. | `GET /_rustybin/harness/runs/{run}/report` |
| `rustybin harness clusters` | The clusters this node leads. | `GET /_rustybin/harness/clusters` |
| `rustybin harness create-cluster [--body JSON]` | Create a cluster (cluster_id, min_nodes, lease, join_token). Flags: `--body` value. | `POST /_rustybin/harness/clusters` |
| `rustybin harness cluster <id>` | A cluster: nodes, leader, runs. | `GET /_rustybin/harness/clusters/{id}` |
| `rustybin harness delete-cluster <id>` | Dissolve a cluster. | `DELETE /_rustybin/harness/clusters/{id}` |
| `rustybin harness leader <id>` | The cluster's leader. | `GET /_rustybin/harness/clusters/{id}/leader` |
| `rustybin harness upstreams <id>` | The cluster's server nodes as the gateway reaches them. | `GET /_rustybin/harness/clusters/{id}/upstreams` |
| `rustybin harness cluster-runs <id>` | The cluster's runs. | `GET /_rustybin/harness/clusters/{id}/runs` |
| `rustybin harness cluster-run <id> <file>` | Create a cluster run from a scenario (start it with cluster-start). | `POST /_rustybin/harness/clusters/{id}/runs` |
| `rustybin harness cluster-show <id> <run>` | One cluster run. | `GET /_rustybin/harness/clusters/{id}/runs/{run}` |
| `rustybin harness cluster-tune <id> <run> <change>` | A live change on a cluster run. | `PATCH /_rustybin/harness/clusters/{id}/runs/{run}` |
| `rustybin harness cluster-start <id> <run>` | Start a cluster run. | `POST /_rustybin/harness/clusters/{id}/runs/{run}/start` |
| `rustybin harness cluster-stop <id> <run>` | Stop a cluster run. | `POST /_rustybin/harness/clusters/{id}/runs/{run}/stop` |
| `rustybin harness cluster-events <id> <run>` | Follow a cluster run's events. Flags: `--max` value, `--timeout` value. | `GET /_rustybin/harness/clusters/{id}/runs/{run}/events` |
| `rustybin harness cluster-report <id> <run> [--format F]` | A cluster run's report. Flags: `--format` value. | `GET /_rustybin/harness/clusters/{id}/runs/{run}/report` |
| `rustybin harness join <cluster-url> --join-token T` | Make this node join a cluster. Flags: `--join-token` value, `--advertise` value. | `POST /_rustybin/harness/join` |
| `rustybin harness leave` | Leave the cluster. | `POST /_rustybin/harness/leave` |

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

| Command | What it does | Calls |
|---|---|---|
| `rustybin keys usage [--day D] [--after A] [--limit N]` | Per-key usage in shared mode. Flags: `--day` value, `--after` value, `--limit` value. | `GET /_rustybin/keys/usage` |

```bash
rustybin keys usage --day 2026-10-30
```

## egress

The outbound connection policy

| Command | What it does | Calls |
|---|---|---|
| `rustybin egress show` | The outbound policy: the allowlist, each feature's rule, the last decisions. | `GET /_rustybin/egress` |

```bash
rustybin egress show
```

## Not covered by a command

The management MCP endpoint (`rustybin mcp-stdio` bridges a client to it), the agent sign-in (OAuth) routes a browser walks, the harness cluster's node-to-node routes, a request bin's capture URLs and a mock's data plane.
