# Changelog

## Unreleased

The build of [Rustybin 0.2](../plans/next/master-plan.md), step by step.

- **Your own OIDC users.** `RUSTYBIN_OIDC_USERS_FILE` replaces the built-in provider's demo users
  with users of your own (PBKDF2-HMAC-SHA256 password hashes from the new
  `rustybin oidc-hash-password`), so the provider can guard something real; the login page stops
  listing the demo passwords. An invalid file stops the server.
- **Foundations** (step 01). One state directory, `RUSTYBIN_STATE_DIR`: settings,
  the secrets key, and the defaults of the mocks and harness state directories.
  Instance settings at `GET`, `PATCH /_rustybin/settings` and `DELETE
  /_rustybin/settings/{key}` (admin): dotted keys with a schema, seeded by their
  environment variables and overridden by what is saved; `RUSTYBIN_SETTINGS_LOCK`
  makes the environment authoritative. Secrets sealed at rest with
  ChaCha20-Poly1305 (`RUSTYBIN_SECRETS_KEY` or a generated `secrets.key`), returned
  only as `{set, hint}`, audited without values. One egress policy for every
  outbound call (`src/egress.rs`): `RUSTYBIN_EGRESS_ALLOW` and
  `RUSTYBIN_EGRESS_ALLOW_LOOPBACK` plus a rule per feature, pinned addresses,
  always-refused ranges, `GET /_rustybin/egress` with the last 100 decisions; A2A
  push notifications, the leads webhook and the key registry URL now resolve
  through it (their variables and semantics are unchanged, except that
  `RUSTYBIN_A2A_PUSH_ALLOW_ALL` no longer reaches link-local or cloud metadata
  addresses). The console title and back link are the settings `console.title` and
  `console.backlink`; the leads webhook token is the secret `leads.webhook_token`.
  The console's settings dialog is replaced by the **Management** view (the gear
  opens it): General (console settings, this browser, the agent panel) and Network
  (the policy, each feature's rule, the decisions, the leads token), with forms
  generated from the settings schema.
- **Console sign-in with a user name and password** (step 02). **Changed default:**
  `RUSTYBIN_CONTROL_AUTH` is `password` when unset (`jwt` in hosted mode, `open` in
  shared mode): the control plane and the console ask for `rustybin` / `rustybin`
  (`RUSTYBIN_CONSOLE_USER`, `RUSTYBIN_CONSOLE_PASSWORD` or an argon2id
  `RUSTYBIN_CONSOLE_PASSWORD_HASH`). Instances that relied on an open control plane
  set `RUSTYBIN_CONTROL_AUTH=open` (self-service instances must); `token` and `jwt`
  are unchanged. `POST /_rustybin/login` (never behind control auth) answers a 12
  hour control token (HS256, the instance key, scope `admin`) that every
  control-plane route, the management MCP server and the admin guard accept;
  `POST /_rustybin/logout` revokes it; `GET /_rustybin/session` says who a credential
  is; `GET`, `DELETE /_rustybin/sessions`, `DELETE /_rustybin/sessions/{id}` and
  `PUT /_rustybin/password` (admin, the current password required) manage sessions
  and the password, which is stored as the secret `console.password_hash` (the user
  name as `console.user`). Five failed attempts per address and minute, then `429`
  with `Retry-After`, doubling up to ten minutes; attempts are audited on
  `rustybin::auth` without the password. With the default password the instance
  warns at startup, `/_rustybin/status` carries `default_password: true` and the
  console shows a banner; a public instance refuses to start. The console: a user
  name and password sign-in screen with Remember me (the token form stays under "Use
  a token instead"), the signed-in user and expiry in the top bar, and Management,
  Sign-in (change the user name and password, the sessions, Sign out everywhere).
  The docker image's CI smoke test signs in with the defaults.
- **AI assist: providers, keys, the assist bar** (step 03). The console's AI features
  run on the user's own provider and key: `RUSTYBIN_ASSIST_PROVIDER` (`anthropic`,
  `openai`, `gemini`, `compatible`, `mock`), `_MODEL`, `_FAST_MODEL`, `_BASE_URL` and
  one `_*_KEY` per provider seed the `assist.*` settings (keys are secrets: sealed,
  `{set, hint}`, never in prompts, exports or captures). Every call is a task with a
  validated input, a system and user prompt, an optional output schema (the provider's
  native JSON mode, validated with the mocks' validator, repaired at most twice) and a
  deterministic mock answer: `free_answer`, `explain`, `suggest` this step. Routes
  `GET /_rustybin/assist`, `POST /_rustybin/assist/run` (SSE events `sent`, `delta`,
  `json`, `action`, `usage`, `error`, `done`, or one JSON document),
  `POST /_rustybin/assist/test` and `GET /_rustybin/assist/usage` (admin). Egress under
  the `assist` purpose (the provider's host once a key is set); caps
  `RUSTYBIN_ASSIST_MAX_CALLS_PER_MINUTE` (20, per caller, `429`) and
  `RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS` (8192); a redact pass refuses inputs that hold a
  stored secret or the admin token (`422`); usage per provider, model and task and the
  metrics `rustybin_assist_calls_total`, `rustybin_assist_tokens_total`. The console:
  the assist bar (`Ctrl+K`) with per-view suggestions, the drawer with streamed
  markdown, "What was sent", tokens and estimated cost, apply cards (`open_view`,
  `copy`, `run_request`), sparkles in the navigation, an **Ask Rustybin** card on the
  overview, **Explain** on a captured request, and Management, AI (curated models with
  prices, keys, Test the key, the built-in mock LLM, usage). New book page
  [Bring your own AI](ai-assist.md).
- **AI mocks: specs from a prompt, examples, AI edit** (step 04). Four tasks:
  `spec_from_description` (an OpenAPI 3.1.0, 3.0.3 or Swagger 2.0 document from a
  description with auth, pagination and error style, parsed and run through the
  deploy checks), `examples_for_operations` (example payloads for the success
  responses that lack them, each validated against its schema, ten operations per
  call), `edit_spec` (the whole document after an instruction, with a bounded
  structural diff) and `request_examples`. Examples stored with a mock
  (`operations.<id>.examples.<status>`, source `user`, `ai` or `generated`, at most 8
  per status) are served where the spec has none and by name, by the REST routes and
  the mock's MCP tools alike. Every revision reports its **example coverage**: a
  banner on the detail page with **Generate with AI** and **Use generated values**
  (the deterministic generator, no key), a `no_examples` warning in the deploy
  report, `check_mock` and `rustybin mock validate`, the count in `get_mock`, and
  `GET /_rustybin/mocks/{name}/coverage`. Routes `POST /_rustybin/mocks/ai/spec`,
  `POST /_rustybin/mocks/{name}/ai/examples` and `POST /_rustybin/mocks/{name}/ai/edit`
  (SSE progress or one JSON document); visitors need `RUSTYBIN_ASSIST_VISITORS`.
  Console: the wizard's **Describe it** tab (first when a key is set), the coverage
  banner, the Generate examples dialog with a preview per operation, **Edit with AI**
  with a streamed diff and Apply as draft, stored examples in the operation editor,
  and the apply cards `create_mock`, `edit_spec`, `generate_examples` from the assist
  bar. New book page [Build mocks with AI](mocks/ai.md).
- **MCP inspector for any server, every auth mode, test runs** (step 05). The console's
  MCP view is rebuilt in three columns (the server and connection, the protocol tabs,
  the timeline with exports) and connects to any MCP server: this instance's servers
  from the new `GET /_rustybin/mcp-servers` (`/mcp` and its variants, the management
  server, every mock's server; a mock's MCP tab opens the inspector on it), saved
  connections, or any URL; over 2026-07-28, the 2025 session versions and legacy
  HTTP+SSE; with no auth, a bearer, an API key (header or query), basic, custom headers,
  or OAuth 2.1 (the discovery walk as a timeline of documents, dynamic registration,
  PKCE in a popup for any issuer, client credentials, refresh). The relay, `POST
  /_rustybin/mcp-relay` (SSE back, at most 64 sessions idle 10 minutes, `DELETE
  /_rustybin/mcp-relay/{session}`), makes the calls when the browser cannot: this
  instance's servers in process, other hosts under the egress purpose `mcp_relay`
  (`RUSTYBIN_EGRESS_ALLOW`), and `POST /_rustybin/mcp-relay/fetch` fetches discovery
  documents. **Checks**: `POST /_rustybin/mcp-checks` runs fourteen checks (connect,
  version, auth refusal, tool schemas, every tool called with generated or AI arguments,
  unknown tool and method, bad arguments, resources, prompts, ping, routing headers, the
  server stream, the session lifecycle) with latency, as JSON or JUnit; runs are saved
  (at most 50). Saved connections (`/_rustybin/mcp-connections/{id}`, admin, at most 50)
  keep their credentials sealed with the settings key and used only by the relay. Two
  assist tasks: `tool_arguments` (Generate arguments, validated against the schema) and
  `sampling` (answer a server's `sampling/createMessage` with your provider). New book
  page [MCP inspector](mcp/inspector.md).
- **MCP registry: a mock registry and inspecting real ones** (step 06). A mock MCP
  registry at `/mcp-registry` in the registry API's `/v0.1` shape (`/v0` answers the
  same): `GET /v0.1/servers` with `cursor`, `limit`, `search`, `updated_since`,
  `version` and `include_deleted`, `GET .../servers/{serverName}/versions[/{version}]`,
  `POST /v0.1/publish` with a token from the `auth/*` stubs (`github-at`,
  `github-oidc`, `oidc`, `dns`, `http`, `none`), the status `PATCH` routes,
  `POST /v0.1/validate`, `health`, `ping`, `version`. It lists every MCP server of the
  instance (`io.rustybin/mcp` and its variants, `io.rustybin/manage`,
  `io.rustybin/mock-<name>` per serving mock, remotes under the request's origin), the
  new `registry.servers` section of `rustybin.yaml` and published entries (at most 256
  together). `server.json` documents are validated against the vendored 2025-12-11
  schema plus the registry's semantic checks. The setting `registry.scenario`
  (`RUSTYBIN_REGISTRY_SCENARIO`) breaks it on purpose: `empty`, `large` (2000
  synthetic entries), `flaky` (every third list `503`), `slow` (2 s), `stale`,
  `invalid`, `auth_fail`. Control plane: `GET /_rustybin/registry`, `DELETE
  /_rustybin/registry/servers/{serverName}` (admin), `POST /_rustybin/registry/validate`
  and `GET /_rustybin/registry/browse`, which fetches a real registry under the egress
  purpose `registry` (`registry.modelcontextprotocol.io` and the saved registries,
  `registry.saved` / `RUSTYBIN_REGISTRY_SAVED`), cached for a minute. The console's new
  **MCP registry** view (Mock registry, Browse a registry; Open in inspector hands a
  remote and its auth to the inspector), the assist task `server_json` ("Write a
  server.json for my mock") and the explain kind `server_entry`. Settings can now be a
  choice (a select in the console). New book page [MCP registry](mcp/registry.md).
- **Dev portal: publish and consume** (step 07). A read-only portal at `/portal` (a
  second SPA embedded like the console, `ui-portal/`) that shows only what was
  published: mock APIs (base URL, authentication with the demo credentials, every
  operation with Try it and curl, the full API reference, downloads in seven formats,
  the mock's MCP server), the mock MCP servers (tools, schemas, `claude mcp add` and
  `mcpServers` snippets), the mock LLM (every provider with a sample and Try it), A2A
  agents, the HTTP tools and the mock MCP registry, with search and a "new this week"
  strip. Publish with `PUT /_rustybin/portal/items/{id}` (`mock:<name>`,
  `mcp:<server>`, `llm`, `a2a:<agent>`, `tools`, `registry`; title, Markdown summary,
  tags, contact, order, credentials), `DELETE` to unpublish, `GET /_rustybin/portal`
  for every item and why a published one is hidden, or the new `portal.items` section
  of `rustybin.yaml`. The portal reads `GET /portal/api/catalog` (published items only,
  ETag). Settings `portal.enabled` (`RUSTYBIN_PORTAL`: `auto`, on once something is
  published), `portal.auth` (`RUSTYBIN_PORTAL_AUTH`: `open`, `password`, `key`),
  `portal.base_url` (`RUSTYBIN_PORTAL_BASE_URL`, a gateway URL for snippets and Try
  it), the branding (`portal.title`, `portal.intro` in Markdown, `portal.accent`,
  `portal.contact`) and `portal.visitors` (`RUSTYBIN_PORTAL_VISITORS`: visitors publish
  their own mocks). Insights: views per day for 30 days, downloads by format and Try it
  calls per item, no visitor identifiers (`GET /_rustybin/portal/insights`); items and
  counts persist in `<state>/portal.json`. Console: **Management, Portal** (status,
  branding, items with bulk publish, insights, Preview), **Publish to portal** on a
  mock's page with **Write with AI** (the assist task `summarise_for_portal`), portal
  badges. Settings may be Markdown (a textarea). `/portal/*` is not captured, ignores
  fault headers and is exempt from the key layer. New book page [Dev portal](portal.md).
- **AI agents: who talks to the instance over MCP** (step 08). Every client of the
  management MCP server is an agent, named by its credential (admin token, console
  sign-in, control JWT, OAuth client, API key, visit, or none) and its `clientInfo`
  name: two clients on one token are two agents. `GET /_rustybin/agents` lists them
  (first and last seen, calls today and in total, sessions, status, rights), `GET
  /_rustybin/agents/{id}` adds calls per tool with their outcomes, the last 20 errors
  and the sessions, `GET /_rustybin/agents/events` streams calls (SSE). Rights below
  what the credential allows: a ceiling (`read-only`, `traffic`, `manage`, `full`) and
  tool allow and deny lists, per agent (`PATCH /_rustybin/agents/{id}`), from the new
  `agents:` section of `rustybin.yaml` (`default_ceiling` and `rules` matching the
  client name and credential kind) or the default (`agents.default_ceiling`,
  `RUSTYBIN_AGENTS_DEFAULT_CEILING`, `full`); a tool outside them is not listed and
  answers a tool error. `POST /_rustybin/agents/{id}/revoke` refuses the agent's calls,
  ends its sessions (`403` from then on) and revokes its console and OAuth tokens and
  an OAuth client's refresh tokens; `/restore` undoes it; `DELETE .../sessions` ends
  sessions only. The admin token is never revoked per agent. Control JWTs with a
  revoked `jti` are refused too. Rights, notes, revocations and totals persist in
  `<state>/agents.json`. Audit lines of the management server carry the agent id.
  Console: the **AI agents** view (the list, a detail panel with the rights form, tool
  chips, Revoke, Restore, End sessions, **Explain what this agent did**, the live feed,
  the defaults and rules) now holds **Connect an agent**, moved from Management,
  General. The stdio bridge registers as transport `stdio`.
- **The whole instance as code, and source control** (step 09). `rustybin.yaml`
  version 2 holds the whole instance: `instance` (the console title and user),
  `settings` (stored, non-secret settings by key), `secrets` (references only:
  `${VAR}` from the environment, `secret:stored` for what the instance holds), the
  `mocks`, `registry`, `portal` and `agents` sections as before, `harness` (saved
  scenarios) and `health`; version 1 files still load. `GET /_rustybin/config/export`
  (YAML, JSON, or a `.tar.gz` bundle with the specs as files), `POST
  /_rustybin/config/apply` `{document, prune, dry_run}` answering the diff (`mocks.orders:
  spec changed (2 operation(s) added)`), each section's result and the problems, `GET
  /_rustybin/config/schema`. An absent section is left alone; with `prune` a present
  section is the whole truth. `RUSTYBIN_CONFIG_FILE` loads every section at startup
  and on reload. Source control (`src/sourcecontrol.rs`, the `git.*` settings seeded by
  `RUSTYBIN_CONFIG_GIT*`): a working copy in `<state>/repo` driven by the `git` binary
  (now in the container image, with `openssh-client`), a token through `GIT_ASKPASS`
  or a generated ed25519 deploy key, the remote's host under the egress purpose `git`;
  `GET /_rustybin/git` (status, ahead and behind, the drift a commit would record, the
  timeline), `POST /_rustybin/git/init`, `/commit`, `/push`, `/pull` (fast-forward,
  then apply with a dry run), `/resolve` (a diverged branch: the instance's copy or the
  repository's, never a merge), `GET`, `POST /_rustybin/git/key`, `GET
  /_rustybin/git/history`; `RUSTYBIN_CONFIG_GIT` clones and applies before serving
  (`_REQUIRED` makes a failure fatal); `git.auto` pushes every change or pulls at an
  interval. Console: **Management, Export** (the document, copy, download, bundle, the
  diff since the last export, apply with a dry run and an explicit prune) and
  **Management, Source control** (the repository form, status, connect, commit and
  push with **Write the commit message**, pull with the diff, resolve, the deploy key,
  history, the operations). The mock MCP registry's published entries are now part of
  `AppState` (exported and pruned with the document).
- **Management MCP parity and the build-an-instance prompts** (step 11). Every
  control route now has a management tool (or a resource, or a written reason), kept so
  by a parity test over the catalogue: settings (`get_settings`, `update_settings`,
  never secrets), the outbound policy (`check_egress` with a dry check of a URL), AI
  assist (`assist_status`, `assist_usage`, `test_assist`, `run_assist_task` for the
  read tasks) and the AI-built mocks (`generate_spec` with `create`,
  `generate_examples` with `apply`, `edit_spec_with_ai` with `apply`, each confirmed
  when it writes; `get_mock` and `check_mock` carry the coverage), the MCP inspector
  (`list_mcp_servers`, `mcp_call` to any server with the handshake its transport
  needs, `run_mcp_checks`, the saved runs and connections), the mock registry
  (`list_registry_servers`, `set_registry_scenario`, `browse_registry`,
  `validate_server_json`, `unpublish_registry_server`), the dev portal
  (`list_portal_items`, `publish_portal_item` confirmed on an open portal,
  `unpublish_portal_item`, `set_portal_branding`, `get_portal_insights`),
  configuration as code at version 2 (`export_config`, `diff_config`, `apply_config`)
  and git (`git_status`, `git_init`, `git_commit`, `git_push` confirmed, `git_pull`,
  `git_resolve`, `git_deploy_key`), agents (`list_agents`, `get_agent`,
  `set_agent_rights`, `revoke_agent` confirmed and never oneself, `restore_agent`,
  `end_agent_sessions`), sign-in sessions (`list_sessions`, `end_session`) and
  `reload_mocks`. Calls to another host, a browsed registry and the answers of MCP
  servers are `untrusted_*` content; secrets are never tool inputs and never returned.
  Five new prompts walk the common jobs: `build_instance` (from a brief to a published,
  exported instance), `publish_portal`, `backup_to_git`, `test_mcp_server` and
  `onboard_agent`; `mock_an_api` gains the describe path. New resources
  `rustybin://harness.schema.json`, `rustybin://portal/catalog` and `rustybin://cli.md`;
  the server instructions are rewritten around the jobs. `rustybin mcp-stdio` takes
  `--url` and `--profile`, so a local agent manages a remote instance with the sign-in
  of `rustybin login`. The REST handlers of the AI mock routes, the portal, the registry
  and the inspector share their logic with the tools (one code path each). The MCP
  conformance suite walks `build_instance` with the Python SDK.
- **The `rustybin` command line for remote instances** (step 10). `rustybin login --url
  <url>` signs in with the console password (or keeps a token with `--token`) into a
  profile (`profiles.json` under `RUSTYBIN_CONFIG_HOME`, else `~/.config/rustybin`, mode
  `0600`; `RUSTYBIN_URL` and `RUSTYBIN_TOKEN` override, `--profile` picks one), then
  nouns and verbs mirror the control API: `status`, `whoami`, `logout`, `instance`,
  `settings`, `sessions`, `password`, `mocks` (every mock verb, the AI ones included),
  `visits`, `visit`, `requests`, `bins`, `agents`, `ai`, `mcp` (servers, saved
  connections, checks with `--junit`, calls through the relay), `registry`, `portal`,
  `config`, `git`, `harness` (the remote verbs; `validate`, `run` and `report` stay
  offline unless `--remote`), `keys`, `egress`, `profiles`, `completion`. Tables for
  people, `--json` for scripts, `--yes` for destructive verbs, `--max` and `--timeout`
  on the tails; exit codes 0, 1 (refused or failed), 2 (usage), 3 (network or
  credentials). A parity test fails when a control route lands without a command; the
  reference `docs/src/cli.md` is generated from the command tree.

## 0.1.0 (2026-10-29): the AI and agent gateway upgrade

A broad rework of Rustybin, from an httpbin-style echo service into an upstream for
API, AI and agent gateway demos.

### Foundation

- axum 0.8, a library plus a thin binary, a shared `AppState`, and a middleware stack
  with request ids, CORS, a time-to-headers timeout, a request body limit and the
  request inspector.
- The route catalogue as the single source of truth for the landing page, the eight
  collection exports, the README tables and the console's API explorer, with tests
  that keep routes, catalogue and OpenAPI in sync.
- Fault injection on every route (`X-Rustybin-Delay`, `X-Rustybin-Fail`), sessions
  (`X-Rustybin-Session`), public mode, an admin token for global mutations, and a
  control plane under `/_rustybin/`.
- Graceful shutdown; the HTTPS and gRPC listeners are optional; invalid settings
  warn instead of failing; bounded state everywhere.

### New

- **Mock LLM** (`/ai/*`): OpenAI (Chat Completions, Responses, completions,
  embeddings, moderations, images, audio), Azure OpenAI, Anthropic, Gemini, AWS
  Bedrock (with binary event streams), Ollama and Cohere, with modes, scripted
  rules, tool calls, structured output, deterministic tokens, native errors and
  rate-limit headers, latency and pace controls, credential checks and request
  inspection.
- **Guardrail mocks**: Azure AI Content Safety, Bedrock ApplyGuardrail, generic
  check and PII redaction.
- **MCP server**: Streamable HTTP for 2026-07-28 and the 2025 handshake versions,
  legacy HTTP+SSE, OAuth 2.1 protected and API key variants, named servers.
- **A2A agents**: seven agents over A2A v1.0 and v0.3, JSON-RPC and HTTP+JSON, with
  streaming, multi-turn, auth-required and SSRF-safe push notifications.
- **Identity provider**: authorization code + PKCE with a login page, refresh token
  rotation, password, client credentials, token exchange, introspection,
  revocation, dynamic client registration.
- **Real auth checks**: JWT validation (HS256 and RS256), HMAC signatures (hmac-auth
  and draft-cavage, with digests), mTLS on the HTTPS listener or forwarded in a
  header, with a persisted demo CA.
- Request bins, generic SSE streams, webhook signing and verification (Standard
  Webhooks, GitHub and Stripe styles), compression (gzip, deflate, Brotli, zstd),
  ranges, data transfer helpers, generic JSON-RPC, GraphQL subscriptions and
  automatic persisted queries, gRPC reflection, health and gRPC-Web.
- The **web console** at `/ui`.
- **OpenAPI mocks** (`/mock/{name}`): upload an OpenAPI 3.0 / 3.1 spec and get a
  named mock API answering with spec examples or deterministic data generated from
  the schemas (path parameters echoed), with rules, per-operation overrides, the
  `Prefer` header, request validation (warn or enforce), deploy checks with a
  report, drafts, rollback, an on/off switch, captured traffic, and mocked security
  from the spec (API key, basic, bearer, OAuth 2.0 / OpenID Connect scopes against
  the built-in identity provider, mTLS) or a fixed scheme, its secrets shown only to
  callers who may change the mock. Managed in the console,
  through `/_rustybin/mocks*`, from `rustybin.yaml` (`RUSTYBIN_CONFIG_FILE`,
  `--config`, reload on `SIGHUP`, JSON Schema for editors) or a directory of specs
  (`RUSTYBIN_MOCKS_DIR`), and with the `rustybin mock` CLI. A `petstore` sample is
  built in (`RUSTYBIN_MOCKS_SAMPLE`). A reload never deletes mocks because of a
  broken file or entry (they keep their last good revision and show a
  `load_error`), enforced validation answers `406` for an unacceptable `Accept`,
  missing scopes get an RFC 6750 `insufficient_scope` challenge, and requests are
  counted in `rustybin_mock_requests_total`.
- **Self-service mocks** for hosted demos (`RUSTYBIN_MOCKS_SELF_SERVICE`, off by
  default): visitors of a public instance create and manage their own mocks
  through a manage token, on a time allowance (an hour, extendable to three, then
  an email address for up to six hours a day, delivered to a webhook), on a
  separate mocks domain, with responses that can never set cookies, redirect
  elsewhere or ship active content. Operators can list, end and pause visits.
- Mock collections also export as a Bruno collection and an Insomnia export
  (`GET /_rustybin/mocks/{name}/collections/bruno|insomnia`, `rustybin mock
  collection`, the console and the management MCP's `export_collection`), in the
  same shapes as `/export/bruno.json` and `/export/insomnia.json`.
- **An MCP server for every OpenAPI mock** at `/mock/{name}/_rustybin/mcp` (and
  `<name>.<domain>/_rustybin/mcp` on the mocks host): one tool per operation, with
  flat arguments from the parameters and body, annotations from the method, an
  output schema from the default response, and the served spec as a resource. Each
  call runs through the mock (rules, examples, stateful data, mocked security with
  the MCP request's credentials, delays, captures marked as MCP), so tools return
  what the REST calls return; a status outside 2xx is a tool error. Settings
  `mcp.enabled` and `mcp.exclude`; an "MCP server" tab in the console.
- **Management MCP server** at `/_rustybin/mcp`: configure a Rustybin from an AI
  agent such as Claude Code. Tools for the instance, OpenAPI mocks (create, edit,
  check, deploy, roll back, delete, collections, `rustybin.yaml` export and apply),
  testing (`try_request` in process on this instance only, `issue_token`,
  `validate_payload`, `wait_for_requests`), captured traffic, usage and
  self-service visits, plus three prompts and two resources. Protected by
  control-plane authentication; `tools/list` shows only what the caller may use;
  destructive tools ask for confirmation (elicitation, else `confirm: true`);
  captured traffic is returned only under `untrusted_*` fields, truncated; changes
  are logged without arguments and counted in `rustybin_manage_mcp_calls_total`.
  The console shows the `claude mcp add` command for the instance, and
  `rustybin mcp-stdio` bridges stdio-only clients.
- **OAuth sign-in to the management MCP server**: where the control plane asks for
  credentials, agents sign in through the browser instead of pasting a token. A
  built-in authorization server (RFC 9728 and RFC 8414 metadata, dynamic client
  registration, PKCE, refresh tokens) sends the browser to a consent page in the
  console, which grants at most what the approver's own credential allows. Tokens
  open `/_rustybin/mcp` only. `RUSTYBIN_CONTROL_OAUTH`, `RUSTYBIN_CONTROL_OAUTH_KEY`,
  and `RUSTYBIN_CONTROL_OAUTH_ISSUER` for an external authorization server.
- **Plan limits** for hosted offerings (`RUSTYBIN_PLAN`, `RUSTYBIN_LIMIT_*`,
  `RUSTYBIN_USAGE_FILE`): rate, concurrency, streams and daily or monthly request and
  egress quotas, per instance or per session, with IETF RateLimit headers on 429s and
  `GET /_rustybin/usage`. Off by default.
- This documentation site, with every example tested in CI, and SDK conformance
  suites for the mock LLM, MCP, A2A and OAuth.

### Operations and hosting

- The management MCP server no longer passes the caller's control credentials
  into the requests its tools send to the data plane, and captures (inspector,
  bins, mock traffic) store `X-Rustybin-Admin-Token` and `X-Rustybin-Mock-Token` as
  `[redacted]`. Mock traffic now redacts API keys like the inspector does.
- A workflow publishes the image to `ghcr.io/coldbathcode/rustybin` on pushes to
  `main` and on `v*` tags.
- Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`): `token` (admin token) or
  `jwt` (admin token or Ed25519 JWTs with `aud`, `exp` and the scopes `inspector`,
  `console`, `admin`) for every `/_rustybin/*` route except the new readiness probe.
  The web console signs in with a token from the URL fragment or a sign-in screen
  and reads its live feed with `fetch` so the bearer header is sent.
  `RUSTYBIN_HOSTED_MODE=true` presets `jwt` and JSON logs.
- `GET /_rustybin/ready`: readiness, always 200 while serving, independent of the
  `/health` demo toggle, exempt from auth, plan limits and capture.
- `GET /_rustybin/metrics`: Prometheus metrics with bounded labels (route templates,
  status classes, protocols, mock LLM tokens by model family, injected faults,
  open streams, egress, build info).
- More metrics: `rustybin_limit_rejections_total{dimension}` (plan limiter 429s and
  gRPC `RESOURCE_EXHAUSTED`), `rustybin_llm_requests_total{provider,model_family,streaming}`
  and `rustybin_llm_faults_total{provider,kind}` (mock LLM faults by kind), and
  latency buckets from 1 ms to 60 s on `rustybin_request_duration_seconds` so p50
  and p99 can be computed with `histogram_quantile` (PromQL examples in the docs).
- `RUSTYBIN_LOG_FORMAT=json`: one JSON object per line with the request id.
- `RUSTYBIN_GRPC_ON_HTTP=true`: the gRPC services on the HTTP and HTTPS listeners
  (h2c and ALPN `h2`) for a TLS-terminating proxy.
- `RUSTYBIN_CONSOLE_TITLE` and `RUSTYBIN_CONSOLE_BACKLINK` in the console header.
- `/_rustybin/version`, `/_rustybin/status` and `/_rustybin/usage` report the build
  commit (`git_sha`) and the control auth mode.
- Shared mode (`RUSTYBIN_SHARED_MODE=true`): one instance for many API keys
  (`X-Rustybin-Key` or `?rb_key=`) from a registry file or URL, per-key limits and
  daily quotas (429 `rustybin_quota_exceeded` until 00:00 UTC), inspector and usage
  scoped by key, keys redacted in captures, readiness waiting for the registry, and
  `GET /_rustybin/keys/usage` for collectors. `RUSTYBIN_REQUIRE_KEY=true` requires a
  key on a normal instance.
- Optional cargo feature `quota-postgres`: replicas share daily quotas through
  PostgreSQL (`RUSTYBIN_QUOTA_STORE`), with `rustybin_quota_store_up` in the metrics.
- **Harness mode, single node** (`RUSTYBIN_HARNESS=on`, phase 1 of the harness plan):
  Rustybin as a load client with its own engine runtime, a dedicated scheduler
  thread, an in-flight cap from the descriptor limit and saturation detection, so a
  slow client is never read as a slow gateway. Scenarios (targets with weights,
  request templates with placeholders, open and closed load models, `constant` and
  `ramp` patterns chained with `then`, thresholds) are validated against the node's
  caps and one JSON Schema (`GET /_rustybin/harness/schema`) drives the API, the
  management MCP tools (`validate_scenario`, `create_run` with confirmation,
  `get_run_report`, `harness_stop`) and the console's scenario builder. Targets must
  be in `RUSTYBIN_HARNESS_TARGETS` and are pinned to their resolved addresses;
  link-local, metadata and control-plane targets are always refused; optional target
  verification through `/.well-known/rustybin-harness`; a kill switch at
  `POST /_rustybin/harness/stop`; the `harness` control scope. Reports as JSON, text
  and JUnit; headless runs at startup with exit codes; `rustybin harness run |
  validate | report`.
- **Harness correlation** (phase 2): every harness request carries a signed marker
  (`X-Rustybin-Harness`), an `X-Request-Id` and a `traceparent`; a server layer
  records arrivals, processing time and what the gateway changed (headers added,
  removed, rewritten, bodies, paths), samples captures into the inspector and tags
  every answer (`Server-Timing: rb`, `X-Rustybin-Served-By`). The client classifies
  answers as `upstream`, `gateway` (by status) or `transport`, counts `lost`
  requests after the run, and reports the gateway overhead and, in loop-through,
  one-way legs. Thresholds `gateway_overhead_p99_ms`, `lost`, `upstream_reach`; the
  scenario's `servers` block drives the shaped upstream `/harness/{*path}` (log-normal
  latency, status mix, body size, chunked streaming) and inspector sampling;
  `rustybin_harness_*` metrics with a bounded run label.
- **Harness clusters** (phase 3): a coordinator creates a cluster
  (`POST /_rustybin/harness/clusters`, or `RUSTYBIN_HARNESS_CLUSTER` with
  `RUSTYBIN_HARNESS_JOIN_TOKEN`); client and server nodes join with the token
  (`RUSTYBIN_HARNESS_JOIN`, `POST /_rustybin/harness/join`) and heartbeat with a long
  poll, carrying cumulative stats and NTP-style clock stamps. Runs are submitted, then
  started once the quorum is there (`scale.client_nodes`, `scale.server_nodes`, the
  cluster's `min_nodes`; headless `RUSTYBIN_HARNESS_START=on-quorum`): the rate is split
  by declared capacity, virtual clients by disjoint ranges, the start is synchronised,
  and one report merges every node's exact histograms, with `lost` checked against
  every server node. Leases stop a client that loses its leader. The server nodes are
  exported for the gateway's upstream pool (`GET .../clusters/{id}/upstreams`); members
  redirect controllers to the leader. MCP tools `create_cluster`, `list_cluster_nodes`,
  `export_upstreams`, `start_run` (and `cluster_id` on the run tools); a cluster card in
  the console.
- **Harness failover** (phase 3b): nodes with `RUSTYBIN_HARNESS_CONTROL_URL` are
  eligible to lead (`RUSTYBIN_HARNESS_LEADER=prefer` makes a standby). The leader
  replicates a versioned snapshot (members, runs with a checkpoint of their stats,
  secrets to eligible nodes only) and orders the live eligible nodes by work; when it
  goes silent for the election timeout (`RUSTYBIN_HARNESS_ELECTION_TIMEOUT`, 3 s), the
  first in line is elected by a majority of the eligible nodes and takes over within
  the lease, so clients never stop. Every message carries the term; an old leader
  steps down, a leader without a majority stops renewing leases (a minority partition
  stops sending). The new leader hands its own client share back and re-splits the
  share of silent clients once their lease expired, never sending more than planned;
  what an old leader's own part sent after its last checkpoint is reported as
  `unreported`. `GET .../clusters/{id}/leader` on any member, vote and announcement
  node routes, the MCP tool `get_leader`, and the leader, term, history and succession
  in the console.
- **Harness patterns and live tuning** (phase 4): layers run every pattern
  (`constant`, `ramp`, `step`, `target`, `breakpoint`, `spike`, `pulse`, `burst`,
  `wave`), chained with `then:`, with `jitter` and `sync: cluster | node`; the scheduler
  follows the exact curve second by second and lands a burst within its window. Closed
  loops: `target` on `overhead_p99_ms` moves the rate toward the gateway overhead goal,
  `breakpoint` stops at the first plateau that fails `until` and reports the last good
  rate. `PATCH .../runs/{run}` changes a running run (rate multiplier, pause and resume,
  hold, skip stage, `set` on any pattern field, add and remove layers), checked against
  the caps, logged, and carried in the cluster snapshot through a failover;
  `GET .../runs/{run}/events` is the run's feed (server-sent events). Reports gain the
  changes, the findings and every stage as it ran. `POST /_rustybin/harness/preview`
  returns the expected curve; saved scenarios (`PUT` and `DELETE
  /_rustybin/harness/scenarios/{name}`, `RUSTYBIN_HARNESS_STATE_DIR`) and six new
  built-ins. `scale.client_nodes: auto` waits for nodes and emits `scale_request`, also
  POSTed to a signed controller hook (`RUSTYBIN_HARNESS_SCALE_HOOK`), with
  `scale.scale_timeout`; `scale.connections_per_client`. MCP tools `list_patterns`,
  `preview_scenario`, `save_scenario`, `update_run`; in the console, a preview chart,
  saved scenarios, live run controls and the feed.
- **Harness rate-limit tests** (phase 5): a layer's `expect` (`limit` per `window`,
  `per` key, ip, consumer, route or global, a `burst` allowance, the refusal `status`, a
  `node_header`) and the `limit_test` pattern with ten tests: `over_limit`,
  `window_edge` (bursts around window boundaries on the wall clock), `just_under`,
  `distributed` (every client node at the same window start, over every target),
  `many_keys` and `many_ips` (identity pools: `{{id}}`, `{{ip}}`, rotating forwarded
  addresses from 198.18.0.0/15), `retry_wave` (refusals retried at once or after
  `Retry-After`), `concurrency`, `slow_rise` and `limit_probe` (a closed loop: a binary
  search for the effective limit), each with an optional `recover` stage. The report's
  `rate_limits`: admission per window and in the worst window-long span, over- and
  under-admission, the first refusal, recovery, the measured window, identities and
  gateway nodes, the rate-limit headers (IETF `RateLimit` drafts, `X-RateLimit-*`,
  `Retry-After`), upstream protection from the server side's arrivals by layer,
  collateral damage to the other layers, and findings; per-layer `thresholds`
  (`rate_limited`, `over_admission`, `under_admission`). Counted on every client node
  and merged by the leader. Built-ins `rate-limit`, `limit-probe` and `many-ips`; the
  analysis in the console's report and the MCP report summary.
- **Harness mock-driven traffic and identities** (phase 6): a layer's source can be an
  OpenAPI mock (`source: { mock: petstore }`): every operation of its live revision, with
  generated parameters and bodies, `operations` (include, exclude, weights),
  `invalid_fraction` (requests that expect a 4xx), `stateful` (a create followed by a
  read of what it made) and `base_path`. A layer's `identities`: API keys (listed or
  minted), JWTs minted by the node's built-in IdP (RS256 or the shared HS256 demo
  secret), OAuth client credentials against an allowlisted token endpoint (fetched
  before the run, cached, renewed), forwarded-address pools and source address binding.
  The report breaks the answers down by operation and by credential (labels without
  secrets) and says when invalid requests got through; listed keys and client secrets
  are redacted. The `petstore-customers` built-in.
- **Harness faults and protocols** (phase 7): a `faults` block with presets (`calm`,
  `jittery`, `flaky-upstream`, `retries`, `synchronised`, `churn`, `all`) and the live
  change `faults`. Client faults by fraction: connection churn, abandoned requests, slow
  bodies, malformed JSON, wrong content types, oversized headers, immediate (or
  synchronised) retries on 5xx, expired tokens, an HTTP/2 mix. Server faults on the
  run's marked requests: drops after the headers, stalls, flapping `/health`, and node
  churn (the server nodes down in turn, 503 with the connection closed). Protocol
  sources: `llm` (streamed chat completions timed token by token), `sse`, `websocket`
  (upgrade and masked frames on the pinned connections), `grpc` (unary echo over HTTP/2,
  judged by `grpc-status`) and `mcp` (stateless `tools/call`). The report's `streams`
  (time to first token, inter-token latency, tokens per second, stalls), the injected
  faults by name, `abandoned` and `unattributed` counts. Built-ins `llm-streaming`,
  `flaky-upstream` and `jittery-clients`.
- **Harness docs and conformance** (phase 8, the last): the guide
  [Load testing through a gateway](harness.md) with tested examples for the control API
  (status, schema, validate, preview, a run with a live change and its report in three
  formats, saved scenarios, the cluster API on one node, the kill switch) and the CLI;
  a compose cluster (`docs/examples/harness/cluster`: a leader, a standby, two clients,
  two servers and a stand-in gateway, headless on quorum); `conformance/harness/run.sh`,
  which runs that cluster, kills the leader mid-run and asserts the standby's report; the
  harness tools, prompt and resource on the agent page. The harness plan is complete.
  Found by that run and fixed: a member that followed a new leader's announcement stood
  for election itself at once, and candidates that split the votes retried in lockstep;
  following a leader now defers the node's next stand, retries add a random wait, and
  refused votes are logged with their reason.

### Changed

- Open redirects and other abuse-prone endpoints were removed or made safe
  (`/redirect-to` only redirects to the same host).
- Content negotiation serves JSON to browsers by default.
- The Docker image no longer generates certificates with OpenSSL at startup:
  Rustybin generates and persists its own demo PKI (the old script produced a CA
  that the server replaced, which broke mTLS and HTTPS verification in containers).
- `tests/smoke_test.sh` was replaced by the documentation examples
  (`docs/examples/run.sh`), which need Hurl 8 (CI pins 8.0.1).
- The Docker image runs as an unprivileged user (uid 10001) and supports a
  read-only root filesystem; `docker-compose.yml` keeps the demo PKI in a named
  volume, and `fly.toml` uses ports 8080 / 8443 and the readiness probe.
- MCP `tools/list` (and the other list methods) render their result once per server
  and protocol version instead of on every request: about 7x the throughput, same
  bytes on the wire.
- Minimum supported Rust version is now 1.88 (was 1.86), to take security fixes in
  `ruzstd` (zstd request bodies, RUSTSEC-2026-0344) and `time` (used when parsing
  client certificates, RUSTSEC-2026-0009).
- Security updates: rustls 0.23.46 (RUSTSEC-2026-0285, -0346, -0347, -0348, -0349),
  rustls-webpki 0.103.15 (RUSTSEC-2026-0104), h2 0.4.20 (RUSTSEC-2026-0258) and
  quick-xml 0.42 (RUSTSEC-2026-0194, -0195: memory exhaustion in the namespace reader
  used by the SOAP endpoint). SOAP field values still decode `&amp;`, character
  references and the other predefined entities; unknown entities are a Sender fault.
- CI runs `cargo audit` on every push.

### Fixed

From a full code review (2026-10-25):

- **GraphQL**: a query nested thousands of levels deep overflowed the parser's stack
  and aborted the whole process. Queries nested more than 64 levels are refused before
  parsing (`NESTING_LIMIT_EXCEEDED`, WebSocket close 1008).
- **Credentials in captures**: `Authorization: Bearer` with the admin token or a
  control JWT is captured as `[redacted]`; the `rb_key` query is redacted in OpenAPI
  mock captures; AI request records redact `X-Rustybin-Key` and `rb_key` and, in
  shared mode, are scoped by key.
- **Console**: the API explorer shows the console's own credentials masked; a valid
  token with a narrow scope signs in; the admin token is kept for the tab only.
- **Shared mode console**: a token bound to a key (`inspector` scope, `rb_key`) was
  sent back to the sign-in screen when an admin token was configured: the console's
  `GET /_rustybin/status` and every other route outside the key's reach answered `401`.
  The status now answers a key with the key's view, and a valid key gets `403` on the
  routes it may not use (`401` stays for a missing or invalid credential). The
  console's own data-plane requests send the token as `X-Rustybin-Key-Token` (it
  stands in for the key, redacted in captures), its traffic view no longer narrows a
  key's requests to the console session, the overview shows the key's view, and the
  API explorer may read the route catalogue.
- **MCP**: tool scopes are checked inside JSON-RPC batches on `/mcp/protected`; the
  legacy HTTP+SSE transport caps requests in flight per session.
- **Self-service**: redirects that browsers read as off-domain (`/\host`) are refused;
  a trailing-dot `Host` no longer reaches the console on the mocks domain; rotating a
  manage token can no longer drop the visit's mocks; every saved visitor mock loads
  after a restart; a visitor mock's spec answers only on the mocks domain.
- **OpenAPI mocks**: odd path templates, multibyte response keys and huge integer
  bounds no longer panic; validation, `minLength` and stateful collections are bounded.
- `/status` with weights whose sum overflows answers `400`; the Azure-shaped
  `text:analyze` caps blocklist names and repeated rows; `/response-headers` keeps
  reflected values inert under any `Content-Type`.
- File-defined mocks load before the HTTP listener serves; JWT control auth warns when
  the token audience would change on every restart; `/openapi.json` is built once.
