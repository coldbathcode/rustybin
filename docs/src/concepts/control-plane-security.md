# Control-plane security, metrics and operations

The control plane (`/_rustybin/*`) and the [web console](../console.md) are locked by
default: a fresh instance asks for a user name and password, `rustybin` / `rustybin`
out of the box, and warns in its log and in the console until the password is
changed. The data plane (everything outside `/_rustybin/`) is never affected: it is
the product your gateway calls.

```bash
rustybin                                         # sign in with rustybin / rustybin, then change it
RUSTYBIN_CONSOLE_PASSWORD=change-me rustybin     # or set it before the first start
RUSTYBIN_CONTROL_AUTH=open rustybin              # a laptop or a lab: no sign-in, as before 0.2
```

## Modes

| `RUSTYBIN_CONTROL_AUTH` | Control plane (`/_rustybin/*`) accepts |
|---|---|
| `password` (default) | A token from the [sign-in](#user-name-and-password) with the console's user name and password, plus everything `jwt` mode accepts (the admin token, control JWTs when a key is configured). `jwt` is the default in [hosted mode](#hosted-mode), `open` in [shared mode](shared-mode.md), where API keys authenticate the control plane. |
| `open` | Anyone. Single mutations still follow the [admin token rules](sessions.md). The default before 0.2; [self-service](../mocks/self-service.md) instances choose it on purpose. |
| `token` | Only `RUSTYBIN_ADMIN_TOKEN`, as `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>`. |
| `jwt` | The admin token, or an Ed25519 (EdDSA) JWT signed by the key in `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`. |

Always reachable without credentials, in every mode:

- `GET /_rustybin/ready`, the readiness probe (see below),
  `POST /_rustybin/login`, the sign-in itself, and `POST /_rustybin/join`, where an
  [invitation](../users.md) sets a new user's password;
- the data plane, including `GET /` (liveness) and `/health` (the demo toggle);
- the console's static files under `/ui/`. They are the same open-source files for
  every instance and carry no instance data: a browser must be able to load them to
  show the sign-in screen or pick up a token from the URL fragment, which never
  reaches the server. Everything the console shows comes from `/_rustybin/*` with the
  token.

Without a token the control plane answers `401` with a `WWW-Authenticate: Bearer`
challenge and a JSON body naming the reason (`missing_token`, `expired`,
`invalid_audience`, `invalid_signature`, `invalid_algorithm`, `not_yet_valid`,
`missing_claim`, `malformed`, `revoked`, `invalid_token`):

```hurl
{{#include ../../examples/concepts/login.hurl:locked}}
```

Unauthenticated control-plane requests are rejected before [plan limits](plans-and-limits.md)
count them; authenticated ones are counted but never rejected by the limiter.
`token` mode without `RUSTYBIN_ADMIN_TOKEN` (or `jwt` mode with neither a key nor an
admin token) refuses every control-plane request and logs a warning at startup. The
same lock in `jwt` mode, as a hosted platform runs it:

```hurl
{{#include ../../examples/concepts/control_auth.hurl:no_token}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:open_routes}}
```

## Permissions

Every control-plane route names the permission it needs, per method (`GET
/_rustybin/mocks` is `mocks.read`, `POST` is `mocks.write`), and one check makes the
decision before the route's handler runs; a few routes are open (the readiness probe,
the sign-in, an invitation's join, the OAuth sign-in endpoints, the harness cluster's
node routes, which check their own credential) and a few need only a signed-in caller
(`GET /_rustybin/session`, sign-out, your own account under `/_rustybin/me` and your
own password, the management MCP server, whose tools check their own permission, the
OAuth consent API and a self-service visit's own routes). The
[route catalogue](catalogue.md), the [CLI reference](../cli.md) (the **Needs** column)
and the [agent tool tables](../agent-mcp.md#tools) list what each one needs.

A caller without the permission gets `403` naming what is missing, never what it
holds, so the console stays signed in and a script can say why:

```json
{"error": "this needs the mocks.write permission", "reason": "permission_required", "permission": "mocks.write"}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:inspector_scope}}
```

The CLI prints `permission_required: mocks.write` and exits `3`; the console asks
`GET /_rustybin/session` for the caller's permissions and disables what they do not
allow, the control's title naming the permission.

Who holds which permissions:

| Caller | Permissions |
|---|---|
| A user (the sign-in) | its roles' (`viewer`, `tester`, `editor`, `admin`, `portal_reader`, custom roles) |
| The admin token | every one (break glass, never a user) |
| A control JWT | its `scope` as roles, plus `rb_roles` and `rb_perms` (see [jwt mode](#signed-access-tokens-jwt-mode)) |
| Nobody, on an open control plane without an admin token (outside public mode) | every one, as before |
| Nobody, on an open control plane with an admin token, or in public mode | the reads: `instance.read`, `mocks.read`, `traffic.read`, `harness.read`, `assist.use`, `mcp.inspect` |
| An API key in [shared mode](shared-mode.md) | the same reads, inside the key layer's fence (its own requests, usage, status, the catalogue, the management server, and with self-service its own mocks) |
| A [self-service](../mocks/self-service.md) visitor | the anonymous reads, and its own mocks through the visit's manage token |

On a self-service instance a visitor or an API key without `mocks.write` still
reaches the mock routes and manages its own mocks there; the instance's mocks stay
the operator's. A few checks depend on the request itself and are made on top of the
route's:

| Request | Also needs |
|---|---|
| `DELETE /_rustybin/requests` without `?session=` (everyone's captures) | `traffic.clear` (one session's need only `traffic.read`) |
| A settings change (`PATCH /_rustybin/settings`, `DELETE /_rustybin/settings/{key}`, a document's `settings:`) of `egress.*`, `proxy.*`, `agents.*`, `console.*`, `sso.*`, `assist.provider` or `assist.base_url` | `settings.security` |
| A settings change of a `portal.*` setting | `portal.settings` |
| A settings change that sets or clears a secret, a document with `secrets:` | `secrets.write` |
| A document (`POST /_rustybin/config/apply`, the `apply_config` tool) with `settings:` or `instance:`; with `agents:`; with `users:` or `roles:` | `settings.write`; `agents.manage`; `users.manage` |
| `PUT /_rustybin/password` by a caller who is not a user (the admin token, a control JWT): the first admin's password | `users.manage` |
| Inviting a user, giving roles, saving a custom role; changing, disabling, deleting or resetting a user | every permission of the roles given (or of the user's roles): nobody grants or manages more than they hold ([users and roles](../users.md#the-guard-rails)) |
| The MCP relay and checks: a saved connection's credentials, or a method that acts on a server on another host (`tools/call`) | `mcp.connections` |
| `GET /_rustybin/checkpoints?from=git` and a restore from the repository | `git.read` |
| The `/health` toggles and `POST /flaky/reset?scope=all` (data plane) | `settings.write` |
| `PUT /_rustybin/portal/items/{id}` with `secure: true` (turns on a mock's mocked security to close it) | `mocks.write` |
| A closed portal item's credentials, Try it, collections and connection snippets (`/portal/api/*`), and the whole portal with `portal.auth=password` | `portal.read`, from the [portal sign-in](../portal.md#portal-logins) or a bearer control credential |

## User name and password

In `password` mode the console's sign-in screen, the [`rustybin` CLI](../cli-guide.md)
and any script sign in with `POST /_rustybin/login` and get a **control token**: an
HS256 JWT signed with the instance key (the one the [OAuth sign-in](#agents-sign-in-with-oauth)
uses), scope `admin`, valid for 12 hours, carrying a session id and the token
generation. It is a control JWT like any other, so the whole control plane, the
[management MCP server](../agent-mcp.md) and the data plane's admin guard accept it
as `Authorization: Bearer`, and captures redact it.

| Setting | Default | Where |
|---|---|---|
| `RUSTYBIN_CONSOLE_USER` | `rustybin` | The setting `console.user`; a value saved in the console wins over the variable |
| `RUSTYBIN_CONSOLE_PASSWORD` | `rustybin` | The password in the clear, for a first deployment |
| `RUSTYBIN_CONSOLE_PASSWORD_HASH` | unset | An argon2id PHC string (`$argon2id$v=19$...`), preferred for deployments; wins over the plain password. Seeds the secret `console.password_hash` |

These are the first admin's (user one, the 0.3 console user). Every other user is
[invited](../users.md) and keeps an argon2id hash of their own in the user store,
which they change themselves (**My account**, `PUT /_rustybin/me/password`,
`rustybin passwd`, the current password required); nobody else ever sees or types it.
The first admin's password changed in the console (**My account** or **Management,
Sign-in**, the current password required) or through `PUT /_rustybin/me/password` or
the 0.3 `PUT /_rustybin/password` is stored as an argon2id hash in the
[settings store](#settings-and-secrets-at-rest) (the secret `console.password_hash`)
and wins over the environment, unless `RUSTYBIN_SETTINGS_LOCK` makes the
environment authoritative. The change ends every session, as in 0.4 (and the
management server's OAuth tokens, which act for no user yet); the caller gets a fresh
token. Another user's password change ends only their own other sessions. The settings API changes `console.user` and `console.password_hash` only
with the current password (`current_password` in the `PATCH /_rustybin/settings`
body; `403` with `reason: current_password_required` or `invalid_credentials`
otherwise), so a stolen sign-in token or an agent cannot lock the owner out or keep
its access; the management MCP server and documents never change them. Any change
of the first admin's user name or password signs every session out. Forgot it?
Another admin sends a new invitation (`POST /_rustybin/users/{id}/invite`, a reset),
or `DELETE /_rustybin/settings/console.password_hash` with the admin token
(`RUSTYBIN_ADMIN_TOKEN` itself) goes back to the environment's password or the
default.

```hurl
{{#include ../../examples/concepts/login.hurl:wrong_password}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:login}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:use_token}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:mcp}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:change_password}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:hash_in_settings}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:reset_needs_password}}
```

**Sessions.** `POST /_rustybin/logout` revokes the presented token for the rest of
its life (a bounded denylist of session ids, at most 4096, expiring with the
tokens). `GET /_rustybin/sessions` (admin) lists the sign-in tokens issued since the
last restart (at most 256: user, address, issue, last use and expiry times),
`DELETE /_rustybin/sessions/{id}` ends one, and `DELETE /_rustybin/sessions` signs out
everywhere by rotating the token generation every token carries, the caller's
included. One user's sessions: `GET` and `DELETE /_rustybin/users/{id}/sessions`
(`users.read`, `users.manage`); your own: `/_rustybin/me/sessions` (any signed-in
user). `GET /_rustybin/session` describes the credential a request presents
(`obtained`: `password`, `admin_token`, `control_jwt` or `api_key`), which is how
the console shows who is signed in: for a sign-in token the user (`user`, the sign-in
name, `user_id`, `display_name`), and for every credential the `roles` and
`permissions` resolved at that request (the admin token: the `admin` role, every
permission).

```hurl
{{#include ../../examples/concepts/login.hurl:sessions}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:end_session}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:logout}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:sign_out_everywhere}}
```

**Brute force.** Five failed attempts from one client address with one user name
within a minute, and they wait: `429` with `Retry-After`, a minute the first time,
doubling up to ten minutes. An IPv6 address counts by its /64, so rotating inside it
gains nothing, and a client trying other user names behind a shared address does not
lock the real user out. Past 100 failures in a minute from every address together,
every sign-in waits for the minute's end. Attempts count as they start, so five
guesses sent in parallel take the whole allowance. Every attempt is logged on the
`rustybin::auth` target with the user name (control characters escaped), the address
and the outcome, never the password. Passwords are compared in
constant time; hashes are verified off the request threads, at most four at once
(`429` with `reason: busy` beyond). The address is the TCP peer, or with
`RUSTYBIN_TRUST_FORWARD` the one header named by `proxy.client_ip_header`
(`RUSTYBIN_TRUST_FORWARD_HEADER`): `fly`, `forwarded`, `x-forwarded-for` (its
rightmost outside hop) or `x-real-ip`. The default, `any`, takes the first usable of
them, which a client can spoof behind a proxy that does not set them all: name your
proxy's header (Management, Network, Behind a proxy). Behind a proxy without
`RUSTYBIN_TRUST_FORWARD` every client has the proxy's address, for the sign-in and
every limit: the instance warns once when a sign-in carries a forwarding header it
does not trust, and a public instance warns at startup.

**The default password.** With it the instance logs a warning at startup,
`GET /_rustybin/status` and the sign-in answer carry `default_password: true`, and
the console shows a banner until it is changed. A public instance
(`RUSTYBIN_PUBLIC_MODE=true`) refuses to start with the default: set a password, or
choose `open` on purpose.

**Tokens across restarts.** Sign-in tokens are signed with the same key as the
OAuth sign-in: `RUSTYBIN_CONTROL_OAUTH_KEY`, else a random key kept in
`<state>/control-oauth.key` (mode 0600) with a state directory, else random per
process. It is never derived from `RUSTYBIN_ADMIN_TOKEN`, so nothing the instance
answers lets anyone test guesses of the admin token offline. With a key per process
(and the default audience, a random instance id) a restart signs everyone out; set a
state directory or the key, and `RUSTYBIN_INSTANCE_ID`, so sessions survive restarts,
and the key (or one state directory) on every replica. An admin token shorter than 24
characters warns at startup.

**What survives a restart.** With a state directory (`RUSTYBIN_STATE_DIR`),
`<state>/sessions.json` (mode 0600) keeps the token generation, the revoked token
ids (sign-outs, ended sessions), the revoked OAuth subjects and the token ids
revoked with an AI agent: a revoked token stays revoked, and a sign out everywhere
or a password change stays in force. Without one, the generation starts from the
start time, so a restart signs every token out. A sign out everywhere never undoes
an agent's revocation, and it ends the OAuth access and refresh tokens of the
management server's sign-in too (they carry the generation). The list of sessions
in Management, Sign-in starts empty after a restart.

**Users, roles and permissions.** The sign-in signs in a *user* of the user store
(`<state>/users.json`, mode 0600, at most 500 users; in memory without a state
directory, which the startup log and [diagnostics](../troubleshooting.md) warn
about). A user holds roles, and a role is a set of named permissions (`mocks.write`,
`traffic.read`, ...): the built-in roles are `viewer`, `tester`, `editor`, `admin`
and `portal_reader`. A sign-in token carries the user id (`sub`), the sign-in name
(`rb_user`), the instance's token generation (`rb_gen`) and the user's (`rb_ugen`),
and its permissions are resolved from the user at every request, so a role change or
a disable applies to the next one. The admin token is every permission and never a
user (break glass); an open control plane is every permission, as before. Every
route and tool checks one of them (see [Permissions](#permissions)). An admin
invites, changes, disables and removes users, and manages custom roles, from the
console, the CLI, an agent or `rustybin.yaml`: [Users and roles](../users.md). A
disabled user's sign-in gets the answer of a wrong password.

**Upgrading from 0.4.** Nothing to do: on the first start the console user (the
`console.user` and `console.password_hash` settings, or `RUSTYBIN_CONSOLE_USER` and
`RUSTYBIN_CONSOLE_PASSWORD` / `_HASH`) becomes user one with the `admin` role. Its
name and password stay where they were, so a downgrade still signs in, and the
environment keeps seeding them on every start (a public instance with the default
password still refuses to start). Sign-in tokens issued before the upgrade (no user
id) stay valid as user one until they expire.

**Migration from 0.1.** Instances that relied on an open control plane now ask for
`rustybin` / `rustybin`. Keep the old behaviour with `RUSTYBIN_CONTROL_AUTH=open`,
or set `RUSTYBIN_CONSOLE_PASSWORD`. `token` and `jwt` mode are unchanged. Platforms
in hosted mode keep `jwt`, shared instances keep `open` with API keys.

## Signed access tokens (`jwt` mode)

A portal, a CI job or a script that holds the private key issues short-lived tokens;
the instance only needs the public key. A token must have:

| Claim | Rule |
|---|---|
| header `alg` | `EdDSA` (Ed25519); any other algorithm is refused |
| `aud` | equal to `RUSTYBIN_CONTROL_JWT_AUDIENCE` (default: `RUSTYBIN_INSTANCE_ID`), so a token for one instance does not open another. With neither set the instance id is a fresh UUID on every start, so tokens stop working after a restart: the server warns at startup |
| `exp` | required; 30 seconds of clock skew are tolerated (also for `nbf` when present) |
| `scope` | optional: a space separated string or an array; absent means `console` (unless `rb_roles` or `rb_perms` is present) |
| `rb_roles` | optional: role names (built-in or custom), a string or an array; their permissions join the scope's |
| `rb_perms` | optional: permission names (`mocks.write`), a string or an array |

Unknown role and permission names are ignored and logged once, never an error, so a
platform can roll out a newer token format first. The scopes are roles too, and each
keeps the reach it had in 0.4:

| Scope | Permissions | What that reaches |
|---|---|---|
| `inspector` | `traffic.read` | the [request inspector](inspector.md) and its live feed, a mock's traffic |
| `console` | the `viewer` role plus `traffic.read`, `assist.use` and `mcp.inspect` | everything the console reads (metrics, settings without secret values, saved tests, source control status, agents included), the AI assist's read tasks, the MCP inspector |
| `harness` | `harness.read`, `harness.run`, `harness.cluster` | the [load test](../harness.md) routes, runs, stops and clusters |
| `admin` | the `admin` role: every permission | everything; also accepted wherever the [admin token](sessions.md) is (health toggles, global resets) |

A route the token's permissions do not cover answers `403` with `reason:
permission_required` and the `permission` it needs (see [Permissions](#permissions)).
The `console` scope previews a mock spec, validates a body against a mock operation,
tests its rules, runs the AI assist's read tasks, validates a `server.json` and uses
the MCP inspector's relay and checks; the relay and the checks send a server on
another host read-only methods only without `mcp.connections` (no `tools/call`: `403
admin_required`). The management MCP server (`/_rustybin/mcp`) checks each tool's
permission itself.

```hurl
{{#include ../../examples/concepts/control_auth.hurl:console_token}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:console_cannot_mutate}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:inspector_scope}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:refused_tokens}}
```

The admin token keeps working in `jwt` mode (for automation such as a usage
collector):

```hurl
{{#include ../../examples/concepts/control_auth.hurl:admin_token}}
```

### Keys and tokens with openssl

`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` takes a PEM public key (newlines may be written as
`\n`), the base64 of its DER, or the base64 / base64url of the raw 32 bytes.

```bash
openssl genpkey -algorithm ed25519 -out control.key
openssl pkey -in control.key -pubout -out control.pub
RUSTYBIN_CONTROL_AUTH=jwt RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="$(cat control.pub)" \
  RUSTYBIN_CONTROL_JWT_AUDIENCE=team-demo rustybin
```

Minting a token needs nothing but openssl 3
([`_mint-control-jwt.sh`](https://github.com/coldbathcode/rustybin/blob/main/docs/examples/concepts/_mint-control-jwt.sh)
`<key> <audience> [scope] [ttl]`, used by the documentation runner):

```bash
{{#include ../../examples/concepts/_mint-control-jwt.sh:mint}}
```

Any JWT library with EdDSA support works too (PyJWT with `cryptography`,
`jsonwebtoken` in Rust, `jose` in JavaScript).

### The console with a token

In `password` mode the console's sign-in screen asks for the user name and
password (a pasted token is the alternative, folded under "Use a token instead");
in `token` and `jwt` mode it asks for a token. A token can also come in the URL
fragment: `https://rustybin.example.com/ui/#token=<jwt>` (or `#/traffic&token=<jwt>`
to land on a view). The console keeps the token in memory and in `sessionStorage`
(this browser tab only; **Remember me** keeps a sign-in token in `localStorage`
until it expires), removes it from the address bar at once, and sends it as
`Authorization: Bearer` on every control-plane call, including the live traffic
feed, which it reads with `fetch` rather than `EventSource` so the header can be
sent (no token in a query string). When the token is missing, refused, revoked or
expired (the console also watches the `exp` claim), it shows the sign-in screen
again, with a link back to `RUSTYBIN_CONSOLE_BACKLINK` when one is set. The top bar
shows who is signed in and when the session expires; **Management, General, Sign
out** revokes a sign-in token and forgets it.

### Agents sign in with OAuth

In `password`, `token` and `jwt` mode the management MCP server (`/_rustybin/mcp`) also lets an
AI agent sign in through the browser: its `401` points at RFC 9728 metadata, the
agent registers with Rustybin's built-in authorization server (`/_rustybin/oauth`),
and you approve it on the console's consent page with the credential the console
holds. The agent's token is capped by that credential's scopes and opens
`/_rustybin/mcp` only, never the rest of the control plane. `RUSTYBIN_CONTROL_OAUTH`
turns it off, `RUSTYBIN_CONTROL_OAUTH_KEY` keeps sign-ins across restarts and
replicas, and `RUSTYBIN_CONTROL_OAUTH_ISSUER` hands sign-in to an external
authorization server that issues control JWTs. See [Sign in with
OAuth](../agent-mcp.md#sign-in-with-oauth).

## Hosted mode

`RUSTYBIN_HOSTED_MODE=true` is a preset for platforms that run one instance per
customer: control auth `jwt` and JSON logs. Explicit `RUSTYBIN_CONTROL_AUTH` and
`RUSTYBIN_LOG_FORMAT` values win over the preset. A typical setup:

```bash
RUSTYBIN_HOSTED_MODE=true
RUSTYBIN_INSTANCE_ID=acme                       # also the default JWT audience
RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="<platform public key>"
RUSTYBIN_ADMIN_TOKEN="<random, for the platform's own collector>"
RUSTYBIN_GRPC_ON_HTTP=true                      # gRPC through the HTTPS edge
RUSTYBIN_TRUST_FORWARD=true
RUSTYBIN_CONSOLE_TITLE="Acme demo"
RUSTYBIN_CONSOLE_BACKLINK=https://portal.example.com/instances/acme
RUSTYBIN_USAGE_FILE=/app/certs/usage.json       # on the writable volume
```

## Settings and secrets at rest

The instance settings (`/_rustybin/settings`, the console's Management view) hold
values that override environment variables and, for a few keys, secrets. With
`RUSTYBIN_STATE_DIR` they persist in `<dir>/settings.json` (mode `0600`, written
atomically); secrets are sealed with ChaCha20-Poly1305 under the key in
`RUSTYBIN_SECRETS_KEY` or `<dir>/secrets.key` (generated once, mode `0600`), so the
file never holds a secret in the clear. The API returns a secret as `set` plus a
four-character hint, never as its value; changes are audited on the
`rustybin::settings` target with the key and the principal (the `sub` of a control
JWT, the admin token, or `open`), never the value. `RUSTYBIN_SETTINGS_LOCK=true`
makes the environment authoritative for instances a platform or a configuration
repository manages. Reads and writes need the admin token or the `admin` scope:

```hurl
{{#include ../../examples/concepts/settings.hurl:admin}}
```

Everything the instance sends of its own goes through the [egress
policy](../configuration.md#outbound-connections): an allowlist, pinned addresses,
always-refused ranges and an audited decision per call.

## Listeners

The HTTP and HTTPS listeners close a connection whose request head has not arrived
within 30 seconds (an idle keep-alive connection too), and hold at most 16,384
connections at once between them; a connection over that is closed as it is
accepted. Neither is configurable.

## Readiness

`GET /_rustybin/ready` answers `200 {"status": "ready"}` whenever the process serves
HTTP, whatever the [`/health` toggle](../reference/reliability.md#health-toggle) says.
It needs no credentials, is never limited by a plan and is never captured by the
inspector, so it is the right target for an orchestrator's readiness or health check
(a presenter flipping `/health` to 503 must not get the instance restarted).

## Metrics

`GET /_rustybin/metrics` serves Prometheus text (version 0.0.4), protected like the
rest of the control plane (`console` scope or the admin token):

| Series | Labels |
|---|---|
| `rustybin_requests_total` | `route` (the matched route template such as `/status/{code}`, or `unmatched`), `method`, `status_class` (`2xx`...) |
| `rustybin_request_duration_seconds` (histogram) | `route`; time until response headers, buckets from 1 ms to 60 s (see below) |
| `rustybin_streams_open` (gauge) | open SSE responses and WebSocket connections |
| `rustybin_egress_bytes_total` | response body bytes |
| `rustybin_protocol_requests_total` | `protocol`: `http`, `graphql`, `grpc`, `websocket`, `sse`, `mcp`, `a2a`, `llm` |
| `rustybin_llm_requests_total` | mock LLM requests served: `provider` (`openai`, `azure`, `anthropic`, `gemini`, `bedrock`, `ollama`, `cohere`), `model_family` (as for tokens), `streaming` (`true`, `false`) |
| `rustybin_llm_tokens_total` | `provider`, `model_family` (a fixed list such as `gpt-4o`, `claude`, `gemini`; anything else is `other`), `direction` (`input`, `output`) |
| `rustybin_llm_faults_total` | faults injected on the mock LLM with `X-Rustybin-Fail` or `?fail=` (see [AI faults](../ai/gateway-features.md#faults)): `provider`, `kind` (`rate_limit`, `server_error`, `unavailable`, `overloaded`, `timeout`, `context_length`, `prompt_filter`, `content_filter`, `invalid_credential`, `missing_credential`, `forbidden`, `not_found`, `bad_request`, `too_large`, `status_4xx`, `status_5xx`) |
| `rustybin_limit_rejections_total` | requests rejected by the [plan limiter](plans-and-limits.md) (HTTP 429 or gRPC `RESOURCE_EXHAUSTED`): `dimension` (`rps`, `concurrency`, `streams`, `requests`, `egress`); all five are always exported |
| `rustybin_faults_injected_total` | `kind`: `fail`, `delay` (`X-Rustybin-Fail`, `X-Rustybin-Delay`), `ai` (mock LLM error responses) |
| `rustybin_mock_requests_total` | requests to [OpenAPI mocks](../mocks/index.md): `mock` (the mock name; not exported in public mode, at most 1024 series, then `other`), `outcome` (`mocked`, `not_found`, `method_not_allowed`, `unauthorized`, `forbidden`, `invalid_request`, `not_acceptable`, `disabled`, `not_deployed`) |
| `rustybin_mock_visits` (gauge) | running [self-service](../mocks/self-service.md) visits (only with self-service on) |
| `rustybin_mock_visit_events_total` | `event`: `started`, `extended`, `ended`, `email_required`, `daily_limit`, `time_used`, `too_early`, `busy`, `rate_limited`, `invalid_email`, `paused`, `not_found` |
| `rustybin_mock_leads_total` | email addresses captured by self-service visits, by `result`: `captured`, `delivered`, `retried`, `dropped` (never an address) |
| `rustybin_build_info` (always 1) | `version`, `git_sha` |

Every label is bounded: raw paths, query strings and model names never become label
values, so a scanner hitting random URLs cannot blow up the series count. Model names
map to a fixed list of families, providers and fault kinds come from fixed lists, and
every other value becomes `other`.

```hurl
{{#include ../../examples/concepts/control_auth.hurl:metrics}}
```

The mock LLM series after one completion and one injected `rate_limit` error:

```hurl
{{#include ../../examples/concepts/control.hurl:metrics_llm}}
```

### Latency percentiles

`rustybin_request_duration_seconds` has the buckets 0.001, 0.0025, 0.005, 0.01,
0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30 and 60 seconds (plus `+Inf`), so
`histogram_quantile` gives usable values from a fast stub (about 1 ms) up to a slow
mock LLM stream or a long injected delay. No two neighbouring bounds are more than
3x apart, so an estimated percentile is within one bucket of the true value.

```promql
# p50 and p99 per route over the last 5 minutes
histogram_quantile(0.50, sum by (le, route) (rate(rustybin_request_duration_seconds_bucket[5m])))
histogram_quantile(0.99, sum by (le, route) (rate(rustybin_request_duration_seconds_bucket[5m])))

# p99 over every route
histogram_quantile(0.99, sum by (le) (rate(rustybin_request_duration_seconds_bucket[5m])))

# Plan limit rejections per second, by dimension
sum by (dimension) (rate(rustybin_limit_rejections_total[5m]))

# Share of streaming mock LLM requests
sum(rate(rustybin_llm_requests_total{streaming="true"}[5m]))
  / sum(rate(rustybin_llm_requests_total[5m]))
```

The histogram measures the time until response headers: for an SSE or streaming LLM
response that is the time to the first byte, not the length of the stream.

A Prometheus scrape job:

```yaml
scrape_configs:
  - job_name: rustybin
    metrics_path: /_rustybin/metrics
    authorization:
      credentials: change-me        # RUSTYBIN_ADMIN_TOKEN
    static_configs:
      - targets: ["rustybin.example.com:80"]
```

`GET /_rustybin/usage` also carries the build (`version`, `git_sha`, `profile`), the
instance id and the control auth mode, so one call tells a collector which version
runs with which plan:

```hurl
{{#include ../../examples/concepts/control_auth.hurl:usage_build}}
```

## Logs

`RUSTYBIN_LOG_FORMAT=json` writes one JSON object per line: `timestamp`, `level`,
`target`, `message` and the event fields at the top level, plus a `span` object with
the request's `method`, `path` and `request_id` (the `X-Request-Id` the response
carries), so log lines join with gateway logs on the request id:

```json
{"timestamp":"2026-10-05T12:00:00.000000Z","level":"INFO","message":"finished processing request","latency":"0 ms","status":200,"target":"tower_http::trace::on_response","span":{"method":"GET","path":"/uuid","request_id":"7d0c...","name":"request"}}
```

## Containers

The Docker image runs as an unprivileged user (uid and gid `10001`) and works with a
read-only root filesystem. Rustybin writes only three things: the demo PKI next to
`RUSTYBIN_TLS_CERT` (default `/app/certs`), the optional `RUSTYBIN_USAGE_FILE`, and
the optional `RUSTYBIN_STATE_DIR` (settings, secrets, saved mocks and scenarios).
Put them on a writable volume:

```bash
docker run -d --read-only --cap-drop ALL --security-opt no-new-privileges \
  -v rustybin-data:/app/certs \
  -e RUSTYBIN_USAGE_FILE=/app/certs/usage.json -e RUSTYBIN_STATE_DIR=/app/certs/state \
  -p 8080:80 -p 8443:443 -p 50051:50051 ghcr.io/coldbathcode/rustybin:latest
```

The named volume inherits the image's ownership at `/app/certs` (uid `10001`).
Keeping state in its `state/` subdirectory avoids an unwritable, root-owned
volume at a new mount path. A bind mount or separate state volume needs ownership
prepared for uid `10001`.

Without a writable `/app/certs` the demo certificates live in memory and change at
every restart (client certificates issued earlier stop verifying). Docker lets an
unprivileged process bind ports below 1024 inside the container
(`net.ipv4.ip_unprivileged_port_start=0`). On runtimes that do not (Kubernetes
without that sysctl, some micro-VM platforms), move the listeners up:
`RUSTYBIN_HTTP_PORT=8080 RUSTYBIN_HTTPS_PORT=8443`.
