# Configuration

Rustybin is configured through environment variables; the only command-line
option that changes server configuration is `--config <file>`, an
[instance configuration document](configuration-as-code.md) (also accepting the
[version 1 mocks format](mocks/config.md)). Most invalid environment values log a
warning and fall back to the default. Required configuration files and features
such as a required git source can still prevent startup. Values are
trimmed, and an empty value counts as unset (except for `RUSTYBIN_CORS_ORIGINS`, see
below). Booleans accept `1`, `true`, `yes`, `on` and `0`, `false`, `no`, `off`
(case-insensitive).

`GET /_rustybin/config` returns the effective configuration without secrets (the
admin token is only reported as `admin_token_configured`).

A few values are also **instance settings** (`GET /_rustybin/settings`, the console's
Management view): a setting saved on the instance wins over its environment
variable, which wins over the default. See [State directory, settings and
secrets](#state-directory-settings-and-secrets) for the rule, the lock and the
secrets.

This list is checked against `src/config.rs` and every `std::env::var` call in
`src/` (the MCP, mock LLM and A2A modules read a few variables of their own).

## Listeners and process

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HTTP_PORT` | `80` | HTTP port. Failing to bind it is fatal (exit code 1). `0` picks a free port. |
| `RUSTYBIN_HTTPS_PORT` | `443` | HTTPS port (same routes as HTTP). Optional: a bind or certificate failure only logs a warning. |
| `RUSTYBIN_GRPC_PORT` | `50051` | gRPC port (`EchoService`, health, reflection, gRPC-Web). Optional, like HTTPS. |
| `RUSTYBIN_HOST` | `0.0.0.0` | Bind address for all listeners: an IPv4 or IPv6 address (`::`, `[::]`) or `localhost` (127.0.0.1). |
| `RUSTYBIN_LOG_LEVEL` | `info` | Log filter in `tracing` `EnvFilter` syntax (`debug`, `warn`, `rustybin=debug,tower_http=info`, ...). Falls back to `RUST_LOG`; an unparsable filter means `info`. Logs go to stdout, coloured only when stdout is a terminal. |
| `RUSTYBIN_LOG_FORMAT` | `text` | `text` or `json` (one JSON object per line, with the request's `method`, `path` and `request_id` in a `span` object). `json` by default in hosted mode. See [Logs](concepts/control-plane-security.md#logs). |
| `RUSTYBIN_GRPC_ON_HTTP` | `false` | Also serve the gRPC services on the HTTP and HTTPS listeners for requests with `content-type: application/grpc*` (HTTP/2; the HTTPS listener then offers `h2` through ALPN), so a TLS-terminating proxy can forward gRPC on 443. See [gRPC](reference/grpc.md#on-the-http-and-https-ports). |
| `RUSTYBIN_INSTANCE_ID` | random UUID | Instance name returned by `/identity`, `/health`, the mock LLM (`X-Rustybin-Instance`) and the gRPC `EchoService`. Set it per replica for load balancing demos. |
| `RUSTYBIN_REQUEST_TIMEOUT` | `120` | Seconds until the response headers must be ready, else `503`. Streaming bodies (SSE, WebSocket, slow drips) are not cut once headers are sent. `0` disables the timeout. A request that times out is counted in the metrics but does not appear in the request inspector. |
| `RUSTYBIN_BODY_LIMIT` | `1048576` | Maximum request body in bytes; larger bodies get `413`. Also the size up to which `/echo` shows a body. |

The process stops gracefully on SIGTERM or SIGINT: listeners stop accepting and
in-flight connections (streams included) get 10 seconds to finish. `GET /_rustybin/ready`
is the readiness probe (always `200` while serving); `GET /` the liveness check.
The Docker image runs as an unprivileged user and supports a read-only root
filesystem, see [Containers](concepts/control-plane-security.md#containers).

## Proxies and TLS

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_TRUST_FORWARD` | `false` | Trust proxy headers. Client IP: `Fly-Client-IP`, then RFC 7239 `Forwarded: for=`, `X-Forwarded-For` (the rightmost address that is not private or loopback, so clients cannot spoof it by prepending), then `X-Real-IP`. Scheme, host and port: `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Port` and `Forwarded: proto= host=`. The OIDC issuer also honours `X-Forwarded-Prefix`. Enable it only behind a proxy you control. Off, every client behind a proxy shares the proxy's address (the sign-in throttle and every limit): a public instance without it warns at startup, and the first sign-in that carries a forwarding header warns once. |
| `RUSTYBIN_TRUST_FORWARD_HEADER` | `any` | With `RUSTYBIN_TRUST_FORWARD`, the one header that names the client: `fly` (`Fly-Client-IP`), `forwarded`, `x-forwarded-for` or `x-real-ip`; `any` keeps the order above. Seeds the `proxy.client_ip_header` setting (Management, Network). Name your proxy's header: with `any`, a client behind a proxy that does not set every header can name its own address. |
| `RUSTYBIN_TLS_CERT` | `certs/server.crt` | Certificate of the HTTPS listener (relative to the working directory). |
| `RUSTYBIN_TLS_KEY` | `certs/server.key` | Private key of the HTTPS listener. |
| `RUSTYBIN_MTLS_IN_HEADER` | unset | Name of a request header in which a gateway that terminated mTLS forwards the client certificate (URL-encoded PEM, raw PEM or base64 DER). `/auth/mtls` then verifies it against the demo CA. See [Authentication](reference/auth.md#mtls). |

**Demo PKI.** At startup Rustybin loads a demo CA from `ca.crt` and `ca.key` in the
directory of `RUSTYBIN_TLS_CERT`, or generates one and writes it there (best effort),
so client certificates issued before a restart stay valid. Rustybin only reuses or
replaces files it created (a CA certificate with the subject `CN=Rustybin Demo CA`): if
`ca.crt` / `ca.key` hold another CA, such as your own, they are left untouched with a
warning and the demo CA lives in `rustybin-demo-ca.crt` / `rustybin-demo-ca.key` instead
(if those names are taken by something else too, it is kept in memory only). For the
HTTPS listener:

- if both `RUSTYBIN_TLS_CERT` and `RUSTYBIN_TLS_KEY` exist, they are used (a stale
  pair issued by an older demo CA is replaced; a key that does not match the
  certificate is an error and HTTPS does not start);
- if neither exists, a server certificate for `localhost`, `127.0.0.1`, `::1` and
  `rustybin` signed by the demo CA is generated and written there;
- if only one exists, nothing is written and the generated pair is used in memory.

Download the CA and a client certificate from `/auth/mtls/get-ca-cert` and
`/auth/mtls/get-client-cert`. The HTTPS listener asks for a client certificate but
does not require one; when one is presented it must chain to the demo CA.

## Shared instances

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_PUBLIC_MODE` | `false` | Hardening for shared, internet-facing instances: lower caps everywhere, the inspector only captures requests tagged with `X-Rustybin-Session`, and instance-global mutations are refused unless an admin token is configured. See [Sessions, public mode and the admin token](concepts/sessions.md). |
| `RUSTYBIN_ADMIN_TOKEN` | unset | When set, instance-global mutations (health toggles, `POST /flaky/reset?scope=all`, clearing every captured request) need `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>` (or a control-plane JWT with the `admin` scope, the console's sign-in token included). With `RUSTYBIN_CONTROL_AUTH=password`, `token` or `jwt` it also opens the whole control plane. Shorter than 24 characters it warns at startup: choose a long random value (`openssl rand -hex 32`). Never returned by any endpoint. |
| `RUSTYBIN_CORS_ORIGINS` | `*` | Allowed CORS origins, comma separated. `off` or an empty value removes the CORS layer entirely, so a gateway's own CORS plugin can be demonstrated. The data plane answers the list (`*`: any origin); the control plane (`/_rustybin/*`, the sign-in and the OAuth endpoints included) answers cross-origin only to the origins listed by name, never to `*`, so a web page cannot sign in or drive the control API from a visitor's browser. A browser app on another origin that calls the control API (or a browser MCP client on `/_rustybin/mcp`) needs its origin listed, for example `*,https://ops.example.com`. |
| `RUSTYBIN_INSPECTOR_CAPACITY` | `500` | Requests kept by the [request inspector](concepts/inspector.md) ring buffer (1 to 10000). |

## OIDC provider

The built-in provider ([OIDC reference](reference/oidc.md)).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_OIDC_USERS_FILE` | unset | Users file (JSON) of the built-in OIDC provider: its users replace the demo users, which can no longer sign in ([Your own users](reference/oidc.md#your-own-users)). Passwords are PBKDF2 hashes from `rustybin oidc-hash-password`. Read at start; an invalid file stops the server. |

## Control plane and console

See [Control-plane security, metrics and operations](concepts/control-plane-security.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_CONTROL_AUTH` | `password` | `password`: every `/_rustybin/*` route except `/_rustybin/ready`, `POST /_rustybin/login` and the OAuth sign-in endpoints (`/_rustybin/oauth/{register,authorize,token}`) needs a token from the [sign-in](concepts/control-plane-security.md#user-name-and-password) with the console's user name and password (or the admin token, or a control JWT). `open`: no credentials (the default before 0.2). `token`: the admin token only. `jwt`: the admin token or an Ed25519 JWT signed by `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` (scopes `inspector`, `console`, `harness`, `admin`). Data-plane routes and the console's static files are never affected. `jwt` by default in hosted mode, `open` in shared mode. |
| `RUSTYBIN_CONSOLE_USER` | `rustybin` | The user name of the sign-in (letters, digits and `. _ @ -`, at most 64 characters). Seeds the `console.user` setting. |
| `RUSTYBIN_CONSOLE_PASSWORD` | `rustybin` | The password of the sign-in, in the clear. With the default the instance warns at startup, `/_rustybin/status` says `default_password: true` and the console shows a banner; a public instance refuses to start. Never returned by any endpoint. |
| `RUSTYBIN_CONSOLE_PASSWORD_HASH` | unset | The password as an argon2id PHC string (`$argon2id$v=19$m=...`), preferred for deployments; wins over `RUSTYBIN_CONSOLE_PASSWORD`. Seeds the secret `console.password_hash`, which a password changed in the console replaces (unless `RUSTYBIN_SETTINGS_LOCK`). Invalid: warning, ignored. |
| `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` | unset | Ed25519 public key for control-plane JWTs: PEM (newlines may be `\n`), base64 DER, or base64 / base64url of the raw 32 bytes. Invalid: warning, JWTs refused. |
| `RUSTYBIN_CONTROL_JWT_AUDIENCE` | `RUSTYBIN_INSTANCE_ID` | Required `aud` claim of control-plane JWTs. |
| `RUSTYBIN_CONTROL_OAUTH` | `true` | OAuth sign-in for agents on the management MCP server ([Sign in with OAuth](agent-mcp.md#sign-in-with-oauth)): with `password`, `token` or `jwt` control auth (not in shared mode), `/_rustybin/mcp` answers `401` with RFC 9728 metadata and the built-in authorization server at `/_rustybin/oauth` issues tokens after approval on the console's consent page. `false` turns it off. |
| `RUSTYBIN_CONTROL_OAUTH_ISSUER` | unset | An external authorization server (absolute `http(s)` URL) named in the metadata instead of the built-in one, which then answers `404`. Its tokens must be control JWTs (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`, `RUSTYBIN_CONTROL_JWT_AUDIENCE`). Invalid: warning, built-in server. |
| `RUSTYBIN_CONTROL_OAUTH_KEY` | random | Secret (32 characters or more; shorter: warning, ignored) signing the built-in server's client registrations and tokens, and the console's sign-in tokens. Unset: a random key kept in `<state>/control-oauth.key` (mode 0600) with `RUSTYBIN_STATE_DIR`, else random per process (agents and the console sign in again after a restart). Never derived from `RUSTYBIN_ADMIN_TOKEN`. Set the same value on every replica that does not share the state directory. Rotating it (or deleting the key file) signs every agent and every console out. Never returned by any endpoint. |
| `RUSTYBIN_AGENTS_DEFAULT_CEILING` | `full` | The most an AI agent on the management MCP server may do unless a rule of `rustybin.yaml` or its own rights say otherwise: `read-only`, `traffic`, `manage` or `full` ([Managing agents](agent-mcp.md#managing-agents); an invalid value warns and is ignored). Seeds the `agents.default_ceiling` setting. |
| `RUSTYBIN_SSO_ENABLED`, `_NAME`, `_ISSUER`, `_CLIENT_ID`, `_CLIENT_SECRET`, `_SCOPES`, `_GROUPS_CLAIM`, `_ROLE_MAP`, `_DEFAULT_ROLES`, `_ALLOWED_DOMAINS`, `_PROVISION`, `_PASSWORD_SIGN_IN` | see [Single sign-on](sso.md#set-it-up) | [Single sign-on](sso.md) with any OpenID Connect provider (the password sign-in only). Each seeds its `sso.*` setting (`RUSTYBIN_SSO_CLIENT_SECRET` the secret `sso.client_secret`); lists are comma separated (`RUSTYBIN_SSO_ROLE_MAP=admins=admin,developers=editor tester`). An invalid value warns and seeds nothing. |
| `RUSTYBIN_HOSTED_MODE` | `false` | Preset for platforms running one instance per customer: control auth `jwt` and JSON logs, unless `RUSTYBIN_CONTROL_AUTH` / `RUSTYBIN_LOG_FORMAT` say otherwise. |
| `RUSTYBIN_CONSOLE_TITLE` | unset | Shown in the console header and tab title (control characters removed, at most 80 characters). Seeds the `console.title` setting. |
| `RUSTYBIN_CONSOLE_BACKLINK` | unset | `http://` or `https://` URL shown as a "Back to ..." link in the console header and sign-in screen; anything else is ignored with a warning. Seeds the `console.backlink` setting. |

## State directory, settings and secrets

One directory holds everything an instance persists. Without it everything lives in
memory, as before, and the settings API says `"persisted": false`.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_STATE_DIR` | unset | Created (mode `0700`) when missing. Holds `settings.json` (the instance settings, mode `0600`), `secrets.key` (the key that seals stored secrets, generated once), and the defaults of `RUSTYBIN_MOCKS_STATE_DIR` (`<dir>/mocks`) and `RUSTYBIN_HARNESS_STATE_DIR` (`<dir>/harness`), which still override it when set. The startup log names the directory and what lives in it. |
| `RUSTYBIN_AUDIT_RETENTION_DAYS` | `30` | How many UTC days of the [audit log](audit-log.md)'s files (`<state>/audit/audit-YYYY-MM-DD.jsonl`) are kept, 1 to 365. Seeds `audit.retention_days`; an invalid value warns and seeds nothing. |
| `RUSTYBIN_AUDIT_MAX_BYTES` | `268435456` (256 MiB) | The audit files' largest total, 65536 or more: past it the oldest days are deleted, and when the day being written reaches it alone, new entries stay in memory until the next day. Seeds `audit.max_bytes`. |
| `RUSTYBIN_SETTINGS_LOCK` | `false` | The environment is authoritative: stored values of the lockable settings are ignored, and writes to them answer `409`. For instances managed by a platform or from a configuration repository. |
| `RUSTYBIN_SECRETS_KEY` | unset | The 32-byte key (base64, URL-safe base64 or hex) that seals stored secrets with ChaCha20-Poly1305. Unset: `<state>/secrets.key`, or a process-lifetime key without a state directory. Changing it makes the stored secrets unreadable: the instance logs it once, the API reports them `unreadable`, the console asks for them again, and the environment seed is used meanwhile. Never exposed. |

The state directory also holds `config.json` (the registry and agents sections last
applied through the API or a repository), `repo/` (the [source control](source-control.md)
working copy), `repo.key` (the deploy key), `control-oauth.key` (the signing key of
sign-in and OAuth tokens, unless `RUSTYBIN_CONTROL_OAUTH_KEY` is set), `sessions.json`
(listed and revoked console sessions), `tests.json` (the [saved tests](saved-tests.md)),
`test-runs/` (the [history of their runs](saved-tests.md#history)), `checkpoints/` (the [checkpoints](checkpoints.md)) and `agents.json`, `portal.json`,
`mcp-inspector.json`.

## Source control

See [Source control](source-control.md). These seed the `git.*` settings; `git.remote`
and the rest can also be set under **Management, Source control**.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_CONFIG_GIT` | unset | The configuration repository: `https://host/org/repo.git`, `ssh://git@host/org/repo.git`, `git@host:org/repo.git` or a local path (`file://`, with the loopback allowance). At startup the instance clones its branch and applies the document (with prune) before serving. Seeds `git.remote`. |
| `RUSTYBIN_CONFIG_GIT_REF` | `main` | The branch (seeds `git.branch`). |
| `RUSTYBIN_CONFIG_GIT_PATH` | `rustybin` | The directory in the repository holding `rustybin.yaml` and `specs/`; empty for the root (seeds `git.path`). |
| `RUSTYBIN_CONFIG_GIT_AUTH` | `token` | `token` (https) or `deploy_key` (ssh; the key is `<state>/repo.key`). Seeds `git.auth`. |
| `RUSTYBIN_CONFIG_GIT_TOKEN` | unset | The token of an https remote (seeds the secret `git.token`). Handed to git through `GIT_ASKPASS`; never exposed. |
| `RUSTYBIN_CONFIG_GIT_AUTO` | `off` | `push_on_change` (commit and push every change, checked every 30 s) or `pull` (pull and apply with prune every `git.pull_every`, 5 minutes by default). Seeds `git.auto`. |
| `RUSTYBIN_CONFIG_GIT_REQUIRED` | `false` | A clone or apply failure at startup stops the instance. By default it warns and starts without the repository's configuration. |

The other `git.*` settings have no variable: `git.username` (the user name sent with the
token, `x-access-token` by default), `git.author` (the commits' `Name <email>`,
`Rustybin <rustybin@example.invalid>` by default), `git.pull_every` (the pull mode's
interval, `1m` to `24h`, `5m` by default), the secret `git.webhook_secret` (a git host's
[push webhook](source-control.md#a-webhook-instead-of-polling)), and `git.sign` (off by
default) with the secret `git.signing_key` for [signed
commits](source-control.md#signed-commits). No document sets any of them from the
repository, and `git.branch` changes with a [switch of
branch](source-control.md#branches).

**Settings** are dotted keys owned by features (`console.title`, `egress.allow`,
`leads.webhook_token`), each with a schema, a default, and usually an environment
variable that seeds it. Resolution, for every key: the value saved through the API,
the console, the CLI or the management MCP wins; else the environment variable;
else the default. `GET /_rustybin/settings` lists every registered key with its
value, its `source` (`stored`, `env`, `default`), whether the lock holds it and its
schema; `PATCH` changes values and secrets; `DELETE /_rustybin/settings/{key}`
forgets a stored value. All three need the admin token (or a control token with the
`admin` scope). In password mode `console.user` and `console.password_hash` change only
with the current password (`current_password` in the `PATCH` body, or
`PUT /_rustybin/password`; else `403 current_password_required`) or the admin token
itself, and a change signs the first admin out everywhere (their sessions and their
agents; other users stay signed in); `DELETE` resets them only with the admin
token (the recovery of a forgotten password). Every change is logged on the `rustybin::settings` target with the
key, the source and the principal, never the value.

```hurl
{{#include ../examples/concepts/settings.hurl:get}}
```

```hurl
{{#include ../examples/concepts/settings.hurl:patch}}
```

Values are validated against each setting's schema; unknown keys and bad values
answer `400` with one problem per key:

```hurl
{{#include ../examples/concepts/settings.hurl:invalid}}
```

```hurl
{{#include ../examples/concepts/settings.hurl:delete}}
```

**Secrets** (a setting marked `secret`) are sealed at rest, compared in constant
time where they are checked, returned by the API only as `{ "set": true, "hint":
"k3Qx" }` (the last four characters, and only for secrets of eight characters or
more), never written to a configuration export in the clear, and never settable
through the management MCP (its `update_settings` also asks the user to confirm a
change under `egress.`, `agents.`, `console.` or `git.`, and never changes the sign-in
in password mode). The settings of this release: `console.title`,
`console.backlink`, the sign-in's `console.user` and secret `console.password_hash`,
the `assist.*` settings of [AI assist](ai-assist.md) (the provider keys are secrets,
`assist.job.<task>` the model per job), `proxy.client_ip_header`, `egress.allow`,
`egress.allow_loopback`, `agents.default_ceiling` and `agents.default_ceiling_for` (the default ceiling of the agents of a role's users, `role=ceiling` entries), `agents.secret_requests` (off by default: lets an agent [ask for a secret to be set](agent-mcp.md#a-secret-at-an-agents-request) by a person), `registry.scenario`,
`registry.saved`, the `portal.*` settings of the [dev portal](#dev-portal), the secret
`leads.webhook_token`, the [test schedule notices](saved-tests.md#the-failure-notice)' secret
`notify.webhook_token` and `notify.base_url`, `mocks.require_revision` (no variable; `false` by default: when
on, every mock edit must name the draft revision it was made from, else `428`, see
[two people editing one mock](mocks/index.md#two-people-editing-one-mock)) and the
`git.*` settings of [source control](#source-control), including the secret `git.token`.

## Outbound connections

Every request the instance sends of its own (a webhook, a key registry, an AI
provider, an MCP server, a registry, a git remote) goes through one policy
(`src/egress.rs`): the destination is parsed, checked against the allowlists,
resolved once and pinned (the connection goes to those addresses only, so a name
cannot change meaning between the check and the connect), and the decision is
logged on the `rustybin::egress` target and kept for `GET /_rustybin/egress` (the
last 100, newest first, shown in the console's Management, Network tab).

To ask before configuring a feature, check a URL without calling it:
`GET /_rustybin/egress/check?url=...&purpose=git`, `rustybin egress check <url>
--purpose git` (exit 1 when it would be refused), `check_egress` on the management MCP
server, or **Check a URL** on the Network tab:

```hurl
{{#include ../examples/concepts/settings.hurl:egress_check}}
```

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_EGRESS_ALLOW` | unset | Host names, `*.domain` wildcards, IP addresses and CIDRs, each with an optional `:port`, comma separated: what every outbound feature may reach, on top of each feature's own rule. Seeds the `egress.allow` setting. |
| `RUSTYBIN_EGRESS_ALLOW_LOOPBACK` | `false` | Let outbound calls reach `127.0.0.1`, `::1` and `localhost` (a mock MCP server or a webhook on the same machine). Seeds the `egress.allow_loopback` setting. |

Always refused, whatever the lists say: link-local and cloud metadata addresses
(`169.254.169.254`, `100.100.100.200`, `192.0.0.192`, `168.63.129.16`, `fd00:ec2::254`), the
unspecified, multicast and broadcast addresses, the same addresses carried inside
IPv6 (NAT64 `64:ff9b::/96`, 6to4 `2002::/16`, IPv4-compatible), this instance's own
control plane (`/_rustybin`, `/ui`), and loopback unless allowed. A `*.domain` entry
does not reach private addresses (RFC 1918, `100.64.0.0/10`, `fc00::/7`) unless a
network entry covers them too. Outbound traffic never uses a system proxy
(`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` are ignored): a proxy would resolve the name
itself, and every connection goes to the addresses the policy checked, whatever form
the URL gives the name (a trailing dot included). Every feature resolves
under a **purpose** with its own rule, on only when the feature is configured:

| Purpose | Its own rule |
|---|---|
| `a2a_push` | `RUSTYBIN_A2A_PUSH_ALLOWLIST`; `RUSTYBIN_A2A_PUSH_ALLOW_ALL` lets it reach any host outside public mode (link-local and metadata addresses stay refused). Loopback allowed when listed. |
| `leads` | The host and port of `RUSTYBIN_LEADS_WEBHOOK_URL`. Loopback allowed. |
| `key_registry` | The host and port of `RUSTYBIN_KEYS_URL`. Loopback allowed. |
| `registry` | `registry.modelcontextprotocol.io` (the MCP registry) and the hosts of the saved registries (`registry.saved`), for [browsing a registry](mcp/registry.md#browsing-a-real-registry). |
| `assist` | The [AI assist](ai-assist.md) provider's host once its key is set (`api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com`, the compatible base URL's host). |
| `git` | The git remote's host once configured; see [source control](source-control.md). |
| `mcp_relay` | No default: name the MCP servers the [inspector](mcp/inspector.md) may relay to and run checks against in `RUSTYBIN_EGRESS_ALLOW`. This instance's own MCP servers are relayed in process and need no entry. |
| `replay` | No default: name the gateways [saved tests](saved-tests.md) run through in `RUSTYBIN_EGRESS_ALLOW`. Tests without a gateway run in process and need no entry. |
| `scenario` | No default: name the gateway a [gateway scenario](gateway-scenarios.md) checks through in `RUSTYBIN_EGRESS_ALLOW` (`RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true` for a gateway on the same machine). |
| `notify` | No default: name the webhooks of [test schedules](saved-tests.md#the-failure-notice) in `RUSTYBIN_EGRESS_ALLOW`. A refused notice is reported in the schedule's status and never retried. |
| `otlp` | The host of `harness.otlp_endpoint`, once set: [a finished run's metrics](harness.md#opentelemetry). |
| `spec_fetch` | No default: name the hosts [specs are fetched from](mocks/index.md#a-spec-from-a-url) in `RUSTYBIN_EGRESS_ALLOW`. Never a visitor's. |
| `record` | No default: name the hosts of the APIs you [record](mocks/record.md) in `RUSTYBIN_EGRESS_ALLOW`. Never a visitor's. |
| `events` | No default: name the hosts of the [webhook subscribers](mocks/asyncapi.md#webhook-subscribers) of event mocks in `RUSTYBIN_EGRESS_ALLOW`. Never a visitor's. |
| `sso` | The issuer's host once [single sign-on](sso.md) is set up (`sso.enabled`, `sso.issuer`, `sso.client_id`): discovery, the keys and the token endpoint. Keys or a token endpoint on another host need `RUSTYBIN_EGRESS_ALLOW`; a provider on the same machine `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true`. |

```hurl
{{#include ../examples/concepts/settings.hurl:egress}}
```

The harness keeps its own, stricter list (`RUSTYBIN_HARNESS_TARGETS`) and its target
verification: load never goes anywhere the egress policy alone would allow.

## OpenAPI mocks

See [Configuration as code](mocks/config.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_CONFIG_FILE` | `./rustybin.yaml` if it exists | A `rustybin.yaml` declaring [OpenAPI mocks](mocks/index.md) (version 1) or [the whole instance](configuration-as-code.md) (version 2: settings, secret references, registry entries, portal items, saved scenarios, agent rules, health), loaded at startup and reloaded by `SIGHUP` or `POST /_rustybin/mocks/reload`. The command-line option `--config <file>` takes precedence. Problems are logged and listed in `GET /_rustybin/mocks` (`load_errors`); they never stop the server. |
| `RUSTYBIN_MOCKS_DIR` | unset | A directory whose `*.yaml`, `*.yml` and `*.json` OpenAPI files (up to 200) each become a mock named after the file, with default settings. |
| `RUSTYBIN_MOCKS_SAMPLE` | `true` (`false` in public mode) | Create the built-in `petstore` mock. |
| `RUSTYBIN_MOCKS_STATE_DIR` | `<RUSTYBIN_STATE_DIR>/mocks`, else unset | Save mocks created or edited through the API or console in this directory (one JSON file each, written on every change) and load them at startup. File-defined mocks are not saved. See [Configuration as code](mocks/config.md#keeping-console-changes). |
| `RUSTYBIN_MOCKS_HOST` | unset | Domain for host-based mock URLs, e.g. `mock.example.com`: a request to `petstore.mock.example.com/v1/pets` is served by the mock `petstore`. Needs wildcard DNS (and a wildcard certificate for HTTPS). See [Configuration as code](mocks/config.md#host-based-urls). |

## Self-service mocks

Visitors of a public instance create and manage their own mocks without the admin
token. See [Self-service mocks](mocks/self-service.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_MOCKS_SELF_SERVICE` | `false` | Turn self-service on. Needs public mode (or shared mode) and control auth `open` or `password` (or shared mode); otherwise it is ignored with a warning. With `password`, [only the operator signs in](mocks/self-service.md#an-operator-who-signs-in). Off by default, also in public mode. |
| `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` | unset | The mocks domain (a separate registrable domain, e.g. `rustybin-mock.dev`): visitor mocks answer as `<name>.<domain>` and the domain serves nothing else. Unset: they are served under `/mock/<name>` (a warning says this is for testing). |
| `RUSTYBIN_MOCKS_SELF_SERVICE_HOME` | unset | Where the bare mocks domain redirects (else `404`). |
| `RUSTYBIN_MOCKS_PER_OWNER` | `3` | Mocks per visitor (or API key in shared mode). |
| `RUSTYBIN_MOCKS_SELF_SERVICE_MAX` | `200` | Visitor mocks on the instance (on top of the instance's own). |
| `RUSTYBIN_MOCKS_SELF_SERVICE_MAX_MB` | `192` | Memory all visitor mocks hold together, in MiB, every kept revision counted (an estimate from each document). Past it a new mock or an edit answers `503`. About a third of the instance's memory. |
| `RUSTYBIN_MOCKS_SELF_SERVICE_STATEFUL` | `false` | Let visitors use `stateful` mode, at most 50 items and 64 KiB per mock. Off because it stores what callers send. |
| `RUSTYBIN_MOCKS_CREATE_PER_HOUR` | `10` | Mocks a session may create per hour. |
| `RUSTYBIN_MOCKS_VISIT_MINUTES` | `60` | First period of a visit and the length of each extension (1 to 1440). |
| `RUSTYBIN_MOCKS_ANON_HOURS` | `3` | Visit time without an email address. |
| `RUSTYBIN_MOCKS_DAILY_HOURS` | `6` | Time per email address and UTC day (never below the anonymous hours). |
| `RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY` | `3` | Anonymous visits per session and UTC day; after that the email step comes first (`0`: always). |
| `RUSTYBIN_MOCKS_BLOCKED_NAMES` | unset | Comma separated globs of names visitors may not use (`*paypal*,login*`). |
| `RUSTYBIN_LEADS_WEBHOOK_URL` | unset | Where captured email addresses are posted as JSON (retried for an hour). |
| `RUSTYBIN_LEADS_WEBHOOK_TOKEN` | unset | Sent as `Authorization: Bearer` to the webhook. Seeds the secret setting `leads.webhook_token` (changeable in the console's Management, Network tab). Never exposed. |
| `RUSTYBIN_LEADS_FILE` | unset | A file the leads are appended to, one JSON object per line. |
| `RUSTYBIN_PRIVACY_URL` | unset | The privacy notice the email form links to. Email capture is off without it. |
| `RUSTYBIN_LEADS_CONSENT` | `checkbox` | `checkbox`: a separate, unticked marketing checkbox. `notice`: no checkbox. |
| `RUSTYBIN_LEADS_SALT` | generated | Salt of the email hashes in the daily ledger; set it to keep daily counts across restarts. Never exposed. |

`${VAR}` and `${VAR:-default}` in the config file are expanded from the environment
(see [Environment variables](mocks/config.md#environment-variables)). The
[`rustybin mock` CLI](mocks/cli.md) reads `RUSTYBIN_SERVER` (default
`http://localhost` on `RUSTYBIN_HTTP_PORT`, port 80 when unset) and `RUSTYBIN_TOKEN` (else `RUSTYBIN_ADMIN_TOKEN`) as
defaults for `--server` and `--token`; the rest of the command line is under
[Command line and saved tests](#command-line-and-saved-tests).

## Plan limits

Off by default. See [Plans and limits](concepts/plans-and-limits.md) for the presets,
the response headers and what is exempt.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_PLAN` | `none` | `none` (nothing enforced, no extra header), `free`, `pro`, `team` or `enterprise` (no limits, plan name reported). |
| `RUSTYBIN_LIMIT_RPS` | plan | Sustained requests per second (token bucket refill rate). |
| `RUSTYBIN_LIMIT_BURST` | plan | Token bucket capacity; `0` means the same as the rps value. |
| `RUSTYBIN_LIMIT_CONCURRENCY` | plan | Requests in flight at once (a response counts until its body is sent). |
| `RUSTYBIN_LIMIT_STREAMS` | plan | Open SSE responses and WebSocket connections at once. |
| `RUSTYBIN_LIMIT_STREAM_SECS` | plan | Maximum stream lifetime in seconds; longer streams are ended cleanly. |
| `RUSTYBIN_LIMIT_REQUESTS` | plan | Requests per period. |
| `RUSTYBIN_LIMIT_EGRESS_MB` | plan | Response body megabytes per period (1 MB = 1,000,000 bytes, fractions such as `0.5` allowed). |
| `RUSTYBIN_LIMIT_PERIOD` | plan | Quota period: `day` or `month` (calendar UTC). |
| `RUSTYBIN_LIMIT_SCOPE` | plan | `instance` (one set of counters) or `session` (one set per `X-Rustybin-Session`, else client IP). |
| `RUSTYBIN_USAGE_FILE` | unset | Path of a JSON file holding the instance quota counters, written every 30 s and on graceful shutdown and read at startup, so a restart cannot reset a monthly quota. Session counters are never persisted. |

Every `RUSTYBIN_LIMIT_*` value accepts `0` or `unlimited` to turn that dimension off.
Setting any of them without a plan enables the limiter with the plan name `custom`.

## API keys and shared mode

Off by default. See [Shared mode and API keys](concepts/shared-mode.md) for the
registry format, quotas, scoping and the control plane.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_SHARED_MODE` | `false` | One instance for many API keys: every data-plane request needs a key (`X-Rustybin-Key`, else `?rb_key=`), each key has its own limits and daily quotas, inspector and usage are scoped by key, the rest of the control plane needs the admin token. Implies `RUSTYBIN_PUBLIC_MODE`, `RUSTYBIN_LIMIT_SCOPE=session` and `RUSTYBIN_LIMIT_PERIOD=day`. |
| `RUSTYBIN_REQUIRE_KEY` | `false` | On a normal instance: the data plane needs a valid key from the registry; no per-key limits, control-plane auth unchanged. |
| `RUSTYBIN_KEYS_FILE` | unset | Key registry file (JSON), read at startup and reloaded when it changes (checked every 5 s). Wins over `RUSTYBIN_KEYS_URL`. |
| `RUSTYBIN_KEYS_URL` | unset | Key registry URL (`http://` or `https://`), fetched at startup and every `RUSTYBIN_KEYS_POLL_SECS` with `If-None-Match`, through the [egress policy](#outbound-connections) (its host is allowed). Errors keep the last good registry. |
| `RUSTYBIN_KEYS_TOKEN` | unset | Sent to `RUSTYBIN_KEYS_URL` as `Authorization: Bearer <token>`. Never returned by any endpoint. |
| `RUSTYBIN_KEYS_POLL_SECS` | `30` | Registry URL poll interval in seconds (every 5 s at most while nothing was loaded yet). |
| `RUSTYBIN_KEYS_MAX` | `200000` | Most keys accepted in a registry; a larger registry is refused with a warning (the last good one stays). |
| `RUSTYBIN_QUOTA_STORE` | unset | `postgres://...` URL of a PostgreSQL database shared by the replicas for daily quotas (table `key_usage`, created when missing). Needs a binary built with `--features quota-postgres`; otherwise a warning and local counting. Never returned by any endpoint. |
| `RUSTYBIN_QUOTA_STORE_CA` | unset | PEM file with extra CA certificates for the quota store's TLS (besides the public web roots). |
| `RUSTYBIN_QUOTA_FLUSH_MS` | `2000` | How often each replica flushes its counts to the quota store, in milliseconds. |
| `RUSTYBIN_QUOTA_UPGRADE_URL` | unset | `http://` or `https://` link returned as `upgrade_url` in daily quota 429 bodies (`null` when unset). |

## MCP server

Read when the server starts.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_MCP_API_KEY` | unset | The only `X-API-Key` value `/mcp/apikey` accepts. Unset: any non-empty key is accepted. |
| `RUSTYBIN_MCP_ALLOWED_ORIGINS` | `*` | Comma-separated `Origin` values accepted by every MCP endpoint (DNS rebinding protection). Requests with another `Origin` get `403`; requests without `Origin` are always accepted. |
| `RUSTYBIN_MCP_ACCEPTED_AUDIENCES` | `rustybin` | Token audiences `/mcp/protected` accepts besides its own resource URL. The default is the identity provider's default audience, so a plain `client_credentials` token works. `none` accepts only tokens issued for the resource (strict RFC 8707). |
| `RUSTYBIN_MCP_ATTACK_SERVERS` | `true` | Seeds the `mcp.attack_servers` setting: the [MCP attack servers](mcp/security.md) at `/mcp/attack/{kind}` and the `mcp_*` gateway scenarios. Always off in public mode. |
| `RUSTYBIN_MCP_RESOURCE_URL` | derived | Override the resource identifier of `/mcp/protected` (by default `<scheme>://<host>/mcp/protected` from the request). Set it to the gateway's public URL when the gateway does not forward `Host`. |
| `RUSTYBIN_MCP_CLOCK_TICK_SECS` | `5` | Update interval of the subscribable `rustybin://clock` resource (1 to 3600). |
| `RUSTYBIN_REGISTRY_SCENARIO` | `normal` | The [mock MCP registry](mcp/registry.md#scenarios)'s scenario: `normal`, `empty`, `large`, `flaky`, `slow`, `stale`, `invalid` or `auth_fail` (an invalid value warns and is ignored). Seeds the `registry.scenario` setting. |
| `RUSTYBIN_REGISTRY_SAVED` | unset | Comma-separated registry base URLs the console offers under Browse a registry; their hosts are allowed for browsing. Seeds the `registry.saved` setting. |

## Dev portal

The [dev portal](portal.md) at `/portal`. Each variable seeds a setting of the
`portal` group (Management, Portal); a value saved there wins unless
`RUSTYBIN_SETTINGS_LOCK` is on. The branding settings (`portal.title`, `portal.intro`,
`portal.accent`, `portal.contact`) have no variable.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_PORTAL` | `auto` | `auto`: `/portal` is on once an item is published; `on`; `off` (`/portal` answers `404`). Seeds `portal.enabled`; an invalid value warns. |
| `RUSTYBIN_PORTAL_AUTH` | `open` | Who may read the portal's data: `open` (each item's `access` decides: a closed one is read by everyone and used by portal readers only), `password` (the whole portal behind the portal sign-in, or a bearer control credential holding `portal.read`; every item closed), `key` (an API key of the key registry reads the open items; a closed one still needs the sign-in). Seeds `portal.auth`; an invalid value warns. |
| `RUSTYBIN_PORTAL_BASE_URL` | unset | The URL testers call when a gateway sits in front of the instance (`https://gateway.example.com/rustybin`): snippets, collections and Try it use it. Unset: the URL the portal was opened with. Seeds `portal.base_url`. |
| `RUSTYBIN_PORTAL_VISITORS` | `false` | On a self-service instance, visitors may publish their own mocks. Seeds `portal.visitors`. |

Published items and the counters are saved in `<RUSTYBIN_STATE_DIR>/portal.json`
(mode 0600; in memory without a state directory). The `portal.items` section of
`rustybin.yaml` publishes items as code.

## Mock LLM

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_AI_REQUIRE_AUTH` | `false` | Require each provider's native credential on every `/ai/*` call (per request: `X-Rustybin-Require-Auth: true`). |
| `RUSTYBIN_AI_API_KEY` | unset | The only accepted key (implies `RUSTYBIN_AI_REQUIRE_AUTH`). For Bedrock SigV4 it is compared with the access key id. |

See [Faults, latency, credentials, inspection](ai/gateway-features.md#credential-checks).

## Event mocks

The caps of [event mocks](mocks/asyncapi.md) are [instance settings](#state-directory-settings-and-secrets) without
a variable: `asyncapi.max_connections` (128: WebSocket and server-sent event connections to
channels at once), `asyncapi.session_minutes` (60: the longest connection) and
`asyncapi.max_subscribers` (50: webhook subscribers on the instance). Webhook subscribers reach
out under the [outbound purpose](#outbound-connections) `events`.

## AI assist

See [Bring your own AI](ai-assist.md). The provider, the models, the base URL and the
keys seed `assist.*` settings, which a value saved under Management, AI overrides; the
two caps and `RUSTYBIN_ASSIST_VISITORS` are read from the environment only.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_ASSIST_PROVIDER` | unset | `anthropic`, `openai`, `gemini`, `compatible` (any OpenAI-compatible Chat Completions endpoint) or `mock` (the built-in deterministic provider); the aliases `claude`, `google` and `openai-compatible` are stored as the provider's name. Unset: AI assist is off and every entry point in the console invites you to set it up. Invalid: warning, off. |
| `RUSTYBIN_ASSIST_MODEL` | provider's default | The model for specs, edits and free answers (the compatible endpoint needs one). Free text; the curated table in the console lists current ids. |
| `RUSTYBIN_ASSIST_FAST_MODEL` | provider's fast default | The model for cheap tasks (suggestions, explanations). |
| `RUSTYBIN_ASSIST_BASE_URL` | unset | The compatible endpoint's base URL, without `/chat/completions` (for example `http://localhost:11434/v1`; loopback needs `RUSTYBIN_EGRESS_ALLOW_LOOPBACK`). Invalid: warning, ignored. |
| `RUSTYBIN_ASSIST_ANTHROPIC_KEY`, `RUSTYBIN_ASSIST_OPENAI_KEY`, `RUSTYBIN_ASSIST_GEMINI_KEY`, `RUSTYBIN_ASSIST_COMPATIBLE_KEY` | unset | The provider keys (secrets `assist.*_key`: sealed at rest, returned as `{set, hint}`, never in prompts, exports or captures). Not to be confused with `RUSTYBIN_AI_API_KEY`, the key the mock LLM *accepts*. |
| `RUSTYBIN_ASSIST_MAX_CALLS_PER_MINUTE` | `20` | AI calls per minute per caller (the control principal: a token caller by its credential, whatever its `X-Rustybin-Session`; plus the client address on an open control plane); more answer `429` with `Retry-After`. 1 to 1000. |
| `RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS` | `8192` | The output cap of every answer. 256 to 128000. |
| (setting only) `assist.job.<task>` | unset | The model one job runs on, one setting per task (`assist.job.explain`, `assist.job.spec_from_description`, ...; group "AI models per job"). Unset: the job's tier default. A model the provider does not offer is ignored (see [Models and jobs](ai-assist.md#models-and-jobs)). |
| `RUSTYBIN_ASSIST_VISITORS` | `false` | On a [self-service](mocks/self-service.md) instance, let visitors use the instance's AI key on their own mocks (describe, generate examples, edit). Off: visitors get `403` from the AI routes; the deterministic generator stays available to them. |

## A2A agents

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_A2A_PUSH_ALLOWLIST` | unset | Comma-separated `host` or `host:port` entries that push notification URLs may point to. The built-in sink `/a2a/webhook-sink/{id}` on this server is always allowed. Deliveries go through the [egress policy](#outbound-connections). |
| `RUSTYBIN_A2A_PUSH_ALLOW_ALL` | `false` | Allow push notifications to any http(s) URL, including loopback and private addresses (link-local and cloud metadata addresses stay refused). Ignored in public mode. |

## Harness mode

The guide, with tested examples, is [Load testing through a gateway](harness.md); this
section is the reference. Rustybin as a load client (`docs/plans/harness.md`): one node with its own engine and
allowlisted targets, or a cluster of nodes led by a coordinator. Off by default. Starting with `RUSTYBIN_HARNESS=on` in
public mode, with self-service mocks, or with the client role and no targets is a
fatal error, never a warning. The control routes live under `/_rustybin/harness`;
starting and stopping runs needs the `harness` (or `admin`) control scope when the
control plane is protected, and follows the admin token on an open one.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HARNESS` | `off` | Turn harness mode on. |
| `RUSTYBIN_HARNESS_ROLES` | `server` | `client` (generates load), `server` (an upstream), `both` (loop-through: the node sends to a gateway that routes back to it), `coordinator`; comma separated. |
| `RUSTYBIN_HARNESS_TARGETS` | unset | What a client may send load to: host names, `*.domain` wildcards, IP addresses and CIDRs, each with an optional `:port`, comma separated. Nothing is allowed without it. Names are resolved once per run and the connections pinned to those addresses; link-local, cloud metadata, multicast and the node's own control plane are always refused. |
| `RUSTYBIN_HARNESS_VERIFY_TARGETS` | `false` (`true` in hosted mode) | Before a run every target must serve this node's token at `/.well-known/rustybin-harness`. A gateway routing to the node passes, which also proves the route before any load is sent. It proves the route, not the target owner's consent: that comes from `RUSTYBIN_HARNESS_TARGETS` and the caps. |
| `RUSTYBIN_HARNESS_MAX_RATE` | `50000` | Requests per second per node (the sum of a scenario's layer peaks). |
| `RUSTYBIN_HARNESS_MAX_INFLIGHT` | `10000` | Requests in flight per node, and at most what the descriptor limit allows (the engine raises the soft limit to the hard limit at startup and keeps 256 descriptors free; in `both` mode each request costs two). Beyond it requests are counted as `not_sent` and the run is flagged as saturated. |
| `RUSTYBIN_HARNESS_MAX_DURATION` | `1h` | Longest run (`30m`, `2h`, up to `24h`). |
| `RUSTYBIN_HARNESS_THREADS` | half the cores (`both`), all but one (client only) | Worker threads of the engine runtime; the scheduler has its own thread. |
| `RUSTYBIN_HARNESS_DATA_PLANE` | `on` | `off` on a client-only node serves only `/`, `/_rustybin/*`, `/ui` and the verification path. |
| `RUSTYBIN_HARNESS_REPORT_DIR` | unset | Every final report is also written here as `<run_id>.json`. |
| `RUSTYBIN_HARNESS_STATE_DIR` | `<RUSTYBIN_STATE_DIR>/harness`, else unset | Saved scenarios are also written here (`scenarios/<name>.json`) and loaded at startup; without it they live in memory. |
| `RUSTYBIN_HARNESS_SCALE_HOOK` | unset | Where the leader of a cluster POSTs a run's `scale_request` when `scale.client_nodes` is `auto` and its client nodes are not enough (`http` or `https`; the host must be allowed by `RUSTYBIN_HARNESS_TARGETS`). Set it on every node that may lead. See below for the signature. |
| `RUSTYBIN_HARNESS_SCENARIO` | unset | Headless mode: the scenario file to run at startup. |
| `RUSTYBIN_HARNESS_START` | `manual` | `immediately` starts the scenario once the node is up; `on-quorum` on a coordinator waits until the nodes the scenario asks for (`scale.client_nodes`, `scale.server_nodes`) have joined its cluster (on a single node it is the same as `immediately`). |
| `RUSTYBIN_HARNESS_REPORT` | unset | Headless mode: where the final report goes (`-` for stdout, else a file). |
| `RUSTYBIN_HARNESS_EXIT` | `false` | Headless mode: exit after the run with code 0 (thresholds pass), 1 (they fail) or 2 (error). |
| `RUSTYBIN_HARNESS_CLUSTER` | unset | The cluster id a coordinator creates at startup (it needs `RUSTYBIN_HARNESS_JOIN_TOKEN`), or the id a node joins when `RUSTYBIN_HARNESS_JOIN` is the leader's base URL. |
| `RUSTYBIN_HARNESS_JOIN` | unset | A client or server node joins this cluster at startup, retrying until the leader answers: `https://leader:8080/_rustybin/harness/clusters/<id>`, or `https://leader:8080` with `RUSTYBIN_HARNESS_CLUSTER`. |
| `RUSTYBIN_HARNESS_JOIN_TOKEN` | unset | The cluster's join secret (16 to 256 characters), shared by the coordinator and its nodes. Never logged or shown. |
| `RUSTYBIN_HARNESS_ADVERTISE` | the listener's address | A server node's data-plane URL as the gateway reaches it, exported at `GET /_rustybin/harness/clusters/<id>/upstreams` for the gateway's upstream pool. |
| `RUSTYBIN_HARNESS_CONTROL_URL` | unset | This node's control URL as the other nodes of a cluster reach it. Set means it may lead (eligible); unset means it never does. `auto` uses the listener's address when it binds a specific one (`RUSTYBIN_HOST=127.0.0.1` in a local demo). |
| `RUSTYBIN_HARNESS_LEADER` | `auto` | `auto` (eligible with a control URL), `prefer` (a standby: first in the succession whatever its work) or `never`. A node with only the coordinator role, a control URL and `RUSTYBIN_HARNESS_JOIN` joins as a standby. |
| `RUSTYBIN_HARNESS_ELECTION_TIMEOUT` | `3s` | How long without the leader before the eligible nodes elect a new one, for the clusters this node creates (200 ms to 60 s; a cluster keeps it below its lease, `election_timeout` on `POST .../clusters` sets it per cluster). |

Every harness request is signed with a per-run key (`X-Rustybin-Harness`) and
carries an `X-Request-Id` and a `traceparent`; a server layer on every data-plane route
records what arrived, samples captures into the inspector (the scenario's
`servers.capture_sample`, 1 in N plus every anomaly) and tags every answer
(`Server-Timing: rb`, `X-Rustybin-Served-By`), so the report separates `upstream`,
`gateway`, `transport` and `lost` requests and shows the gateway's overhead and what it
changed on the way. `/harness/{*path}` is a shaped upstream that answers with the run's
`servers` profile (latency, status mix, body size, streaming). The metrics gain
`rustybin_harness_*` series with a run label bounded to the active run plus the last 4.

A cluster spreads a run over several nodes. A coordinator creates it
(`RUSTYBIN_HARNESS_CLUSTER` and `RUSTYBIN_HARNESS_JOIN_TOKEN` at startup, or
`POST /_rustybin/harness/clusters`); client and server nodes join it with the join
token (`RUSTYBIN_HARNESS_JOIN`, or `POST /_rustybin/harness/join`) and then pull from
the leader: a heartbeat long poll carries their stats and clock stamps and brings
back assignments. A scenario submitted to `POST .../clusters/<id>/runs` starts with
`.../start` once its quorum is there: the rate is split over the client nodes by their
declared `RUSTYBIN_HARNESS_MAX_RATE`, each gets its own range of virtual clients, and
all of them start at the same instant on the leader's clock. Server nodes get the
run key and correlate; after the run the leader asks them which failed requests they
saw, so `lost` is exact across the cluster. The report is one merge of every node's
counters and histograms (no averaged percentiles), with the nodes, their shares,
clock offsets and start lag. Each node checks its assignment against its own caps
and its own `RUSTYBIN_HARNESS_TARGETS`, so a leader cannot aim a node anywhere its
operator did not allow. A client stops sending when no leader has answered for the
cluster's lease (5 s by default).

The leader can fail over. The node a cluster was created on leads term 1 and is a
member of its own cluster. Nodes with `RUSTYBIN_HARNESS_CONTROL_URL` are eligible to
lead; the leader replicates a versioned snapshot to every node (who leads, the term,
who leads next), and to the eligible nodes also the members, the runs with a
checkpoint of their stats (every 5 s, or every lease when shorter) and the cluster's
secrets. It orders the live eligible nodes into a succession: `prefer` standbys
first, then by work (the rate a node is assigned, the requests arriving at it,
requests in flight, CPU and scheduler lag). When the leader has not answered for the
election timeout, the first node in line stands for election, the next ones after
a short wait each; it needs the votes of a majority of the eligible nodes, so a
cluster needs three of them for failover (`GET .../clusters/<id>/leader` says
whether it has). The new leader takes over from the snapshot within the lease, so
the clients never stop: they report their cumulative stats to it again, which fills
the gap exactly. It hands its own client share back (`scale.leader_sheds_load`,
default true) once another client node carries the run with it, and takes the share
of a client node that went silent once that node's lease has surely expired, so the
cluster never sends more than planned. Requests the old leader's own client part may
have sent after its last checkpoint are reported as `unreported`, an upper bound,
never dropped silently. A leader that does not hear a majority of the eligible nodes
stops renewing leases, so the minority side of a partition stops sending. A member's
claim of a newer term never deposes it: it steps down only on a proven newer leader (an
eligible member answering as leader at the control URL it joined with), then follows it
as a member. A crashed
leader that restarts is a new, empty node: start it as a standby (with
`RUSTYBIN_HARNESS_JOIN` pointing at the leader or any eligible member, which names
the leader) to rejoin the cluster. The node routes
(`.../nodes`, `.../nodes/<node>/heartbeat`, `.../election/vote` and
`.../election/elected`) authenticate with the join token and the node credential
instead of the control-plane credentials.

A scenario's `targets` are the gateways (or gateway nodes) the load goes to, each a URL
or an object:

| `targets[]` | Default | Description |
|---|---|---|
| `url` | required | `http` or `https`, without query or fragment; its path leads every request. Allowed by `RUSTYBIN_HARNESS_TARGETS` on every client node, never a control plane path. |
| `weight` | `1` | The share of the requests this target gets (1 to 1000). |
| `host` | the URL's host | The `Host` header and TLS server name to send instead. |
| `http` | `auto` | `auto` (ALPN on TLS, HTTP/1.1 on plain connections), `h1`, `h2` (TLS only), `h2c` (prior knowledge on plain connections) or `h3` (HTTP/3 over QUIC, `https` only). |
| `insecure` | `false` | Skip TLS certificate verification (a demo gateway with a self-signed certificate). |

**HTTP/3 targets.** `http: h3` needs a build with the cargo feature `harness-http3`
(`cargo build --release --features harness-http3`, or the image built with
`--build-arg CARGO_FEATURES="quota-postgres harness-http3"`); the default binary and the
published image have no QUIC stack, and refuse the scenario at validation with a message
that names the feature. `GET /_rustybin/harness` says `http3: true` on a build that has
it. An HTTP/3 target goes through the same fences as any other: the allowlist entry's
port is the UDP port, the name is resolved once and QUIC connects to those pinned
addresses only (no `Alt-Svc`, no redirect, no move to an address the server prefers),
forbidden addresses are refused, target verification runs over HTTP/3, the kill switch
stops it, and TLS is verified unless `insecure`. Requests carry the marker and answers
are classed and reported as over HTTP/1 and 2. HTTP/3 carries the plain HTTP sources
(`request`, `requests`, `mock`): the protocol sources, the faults `connection_churn` and
`http2`, and `identities.source_addresses` are refused with an `h3` target. A target
keeps 8 QUIC connections, round robin over its pinned addresses, and requests are
streams on them.

**Server-to-server chains.** `servers.chain` makes a server that answers a request of the
run call a further hop, so the report gains the leg of each hop
([the guide](harness.md#server-to-server-chains) has the rules). Off without it.

| `servers.chain` | Default | Description |
|---|---|---|
| `hops` | required | 1 to 3 hops in order, never the same one twice: the server answering a client's request calls the first, the server answering that call the second. Each is a URL or an object. |
| `hops[].url` | required | A gateway URL (`http` or `https`, no query or fragment). Allowed by `RUSTYBIN_HARNESS_TARGETS` **on the server node**, pinned, and verified like a target when `RUSTYBIN_HARNESS_VERIFY_TARGETS` is on. Never a control plane path. |
| `hops[].path` | `/` | The one path the call asks for, after the URL's own. No placeholders: a hop is one fixed `GET`. |
| `hops[].host`, `http`, `insecure` | as a target's | The `Host` header and TLS name, the protocol (`auto`, `h1`, `h2`, `h2c`; not `h3`) and whether to skip certificate verification. |
| `timeout` | `5s` | Per onward call (1 ms to 30 s). |
| `max_concurrent` | `64` | Onward calls in flight per run on a server node (1 to 1024); one over it is refused, not queued. |

A server calls on only for a request carrying the run's signed marker, only while the run
is live (in a cluster: on its leader's word, renewed like a lease), at most once per
request and hop, and the kill switch stops the calls at once. A server node needs
`RUSTYBIN_HARNESS_TARGETS` for the hops it may call; one whose list does not allow a hop
makes no call and the report's `server.chain.problems` says why. The report's
`server.chain.hops[]` has `calls`, `upstream`, `gateway`, `transport`, `statuses`,
`transport_kinds`, `refused` (by reason: `not_live`, `kill_switch`, `seen_before`,
`untracked`, `busy`, `not_prepared`), `arrivals`, and the `latency`, `upstream_latency`
and `overhead` of the leg.

Each layer's `pattern` says how hard it pushes over time, as a rate (open model) or
a number of users (closed model):

| Pattern | Parameters |
|---|---|
| `constant` | `rate`, `for` |
| `ramp` | `from`, `to`, `over`, `curve: linear \| exponential` |
| `step` | `start`, `step`, `every`, `max`, `hold` (how much longer the last plateau lasts); each plateau is reported on its own |
| `target` | `rate` or `users`, reached `within` and held for `hold`; or `overhead_p99_ms` (a closed loop: the rate moves every second toward the gateway overhead goal, from `start` up to `max`) |
| `breakpoint` | `start`, `growth` (a fraction per plateau), `every`, `until` (`error_rate`, `latency_p99_ms`, `overhead_p99_ms`), `max`: the run stops at the first plateau that fails and reports the last good rate |
| `spike` | `base`, `peak`, `rise`, `hold`, `fall`, `recover` |
| `pulse` | `high`, `low`, `on`, `off`, `cycles` |
| `burst` | `base`, `size` requests sent `within` a window, `every`, `for` |
| `wave` | `min`, `max`, `period`, `shape: sine \| triangle \| sawtooth`, `for` |
| `limit_test` | `test` (below), `limit` and `window` (default: the layer's `expect`), and the test's own parameters |

Patterns chain with `then:` (up to 16 stages). Every pattern takes `jitter` (0 to 1: each
second the value varies by up to that fraction, the same on every node for a given run)
and `sync`: `cluster` (the default) has every client node follow the curve in lockstep
on the cluster clock, so a burst reaches the gateway as one burst; `node` gives each node
its own phase and its own jitter, which spreads the load. `POST /_rustybin/harness/preview`
returns the expected requests per second of a scenario, each layer and the total, from
the pattern math (one point per second, coarser past an hour). The built-in scenarios
include `step-plateaus`, `breakpoint`, `spike-and-recover`, `pulse`, `burst`,
`daily-wave`, `rate-limit`, `limit-probe`, `many-ips`, `petstore-customers`,
`llm-streaming`, `flaky-upstream` and `jittery-clients`;
`PUT /_rustybin/harness/scenarios/<name>` saves your own next to them
(validated, at most 100 per node, kept in `RUSTYBIN_HARNESS_STATE_DIR` when set) and
`DELETE` removes one.

**Rate-limit tests.** A layer's `expect` says what the gateway's limiter should do with
its traffic: `limit` requests per `window`, `per` (`key`, `ip`, `consumer`, `route`,
`global`), an optional `burst` allowance on top of the limit, the refusal `status`
(default 429) and a `node_header` naming the gateway node that answered (`via`, say). A
`limit_test` pattern probes the limiter for a known weak spot:

| Test | Sends | Finds |
|---|---|---|
| `over_limit` | `factor` (10) times the limit `for` a while | Enforcement, the time to the first refusal, how many got through |
| `window_edge` | `size` (the limit) requests `within` 100 ms just before a window boundary and again just after, at every other boundary on the wall clock (`align: start` for windows that open with the first request), `cycles` times | Fixed-window double dipping: up to twice the limit in one window length |
| `just_under` | `fraction` (0.9) of the limit `for` a while | False positives |
| `distributed` | `factor` (4) times the limit at each window start, from every client node at once, over every target | Counters local to each gateway node |
| `many_keys` | `identities` (20) keys (`{{id}}` in a header or the path) under their own limit, `factor` (5) times a global limit together | Whether global, per-IP and per-route limits hold |
| `many_ips` | rotating forwarded addresses from 198.18.0.0/15 (`header`, default `X-Forwarded-For`; `{{ip}}`) | Per-IP limits that trust a forwarded header |
| `retry_wave` | `factor` (3) times the limit; each refusal retried `retries` (3) times at once, or after its `Retry-After` with `honour_retry_after` | Recovery under retries |
| `concurrency` | `size` requests at once `every` window, `cycles` times | Connection and concurrency limits |
| `slow_rise` | from half to one and a half times the limit (`from`, `to`) `over` a while | The rate where enforcement starts |
| `limit_probe` | a binary search between `min` and `max`, a probe `every` 2 s, `steps` (8) probes | The effective limit when nobody wrote it down |

Every test but `limit_probe` takes `recover`: after the test, half the limit for that
long, to measure the time to recovery. The report's `rate_limits` has, per layer that
asks for it (an `expect`, a `limit_test`, or layer thresholds): admitted, refused and
failed requests; admission per window and in the worst window-long span (over- and
under-admission against `expect`); the time to the first refusal and the requests
admitted before it; recovery; the window as measured (the spacing of admission resuming
after refusals); per identity and per gateway node (target or `node_header`) breakdowns;
the rate-limit headers (`RateLimit` and `RateLimit-Policy` in any IETF draft form,
`X-RateLimit-*`, `Retry-After`): how often they come, whether the advertised limit is the
expected one and whether the remaining count counts down; upstream protection (what
reached the server side, by layer, against what was admitted: a limiter that forwards
and then rewrites the answer does not shield the upstream); collateral damage (the other
layers' refusals and latency during the test); and findings in words. A layer's own
`thresholds` (`rate_limited`, `over_admission`, `under_admission`) fail the run like the
run's. `limit_probe` is a closed loop like `breakpoint`: it reads the run's refusals per
second (all layers), so run it on its own.

**Mock-driven traffic.** A layer's `source` can name an OpenAPI mock instead of request
templates: `source: { mock: petstore }` sends requests for every operation of the mock's
live revision on the client node (every client node needs the mock, from the same
`rustybin.yaml`), with generated parameters and bodies (a few variants per operation,
each one a request the operation accepts) under `base_path` (default `/mock/<name>`, as
a Rustybin upstream serves it; empty when the gateway maps the mock's own paths).
`operations` takes `include`, `exclude` and `weights` by operation id. `invalid_fraction`
makes that share of the requests invalid (a body of the wrong type, a required query
parameter left out, a number that is not one); they expect a 4xx, and the report says
how many got through anyway. `stateful: true` follows every successful create (a POST on
a collection) with a read of the item it returned, and lets the other item operations
answer 404. `POST /_rustybin/harness/validate` and a node starting a run say when the
mock or an operation is missing.

**Identities.** A layer's `identities` say what its requests carry to tell clients apart.
The layer cycles through `count` identities (default: the largest pool), and identity
`n` takes the `n`th of each:

| Pool | Parameters |
|---|---|
| `api_keys` | `header` (default `apikey`) or `query`; `values`, or `mint` keys named `<prefix><n>` (`prefix` default `harness-key-`: provision the same names in the gateway) |
| `jwt` | JWTs minted by the client node's built-in IdP: `mint` subjects `<subject_prefix><n>` (default `harness-user-`), `issuer` (default `rustybin-harness`), `audience`, `scope`, `ttl` (1h), `alg`: `rs256` (the node's IdP key, its JWKS at `/oauth/jwks`) or `hs256` (the demo secret, the same on every node of a cluster) |
| `oauth` | OAuth client credentials: `token_url` (allowed by `RUSTYBIN_HARNESS_TARGETS` like a target), `clients` (`client_id`, `client_secret`), `scope`, `audience`. Every client's token is fetched before the run (a run whose tokens cannot be had does not start), cached and fetched again before it expires |
| `forwarded` | `count` addresses from `cidr` (default 198.18.0.0/15) in `header` (default `X-Forwarded-For`); `{{ip}}` in a template |
| `source_addresses` | Local addresses of the client node to connect from (checked when the run starts) |

The credentials and addresses replace a template's headers of the same names; `{{id}}`
is the identity. Layers added while a run runs take every pool but `oauth` and
`source_addresses`. The report breaks the answers down by `operations` (mock layers;
invalid requests and reads after a create on their own) and by `credentials` (a label
without the secret: the minted key's name, `key <n>` for listed keys, the JWT subject,
the OAuth client id, the address), with answers, errors, refusals and approximate p50
and p99, merged across a cluster's client nodes. Listed API keys and client secrets are
redacted in reports.

**Protocol sources.** A layer's `source` can also be `llm` (a chat completion in the
OpenAI shape, streamed by default: `path`, `model`, `prompt`, `tokens`, `stream`,
`headers`; the mock LLM's `X-Rustybin-TTFT-Ms` and `X-Rustybin-Tokens-Per-Second`
headers give it known timings), `sse` (a server-sent event stream: `path`, `events` to
read, or `hold_unread` for a reader that never reads), `websocket` (a session on an
HTTP/1.1 target: `messages` of `size` bytes, each echo awaited, an optional `hold`),
`grpc` (a unary call of Rustybin's echo service over HTTP/2: `http: h2c` on plain
targets; `path`, `message`) or `mcp` (a stateless `tools/call`: `path`, `tool`,
`arguments`). The report's `streams` has, per streamed layer (llm, sse, websocket), the
streams, items per stream and per second (tokens, events, round trips), the time to the
first item (time to first token), the gaps between items (inter-token latency), the
stream length, and the streams that stalled (no item for the request timeout) or ended
early. A streamed answer's latency is the stream's length; the gateway overhead is not
measured on streamed answers. A gRPC answer is judged by its `grpc-status`, an MCP answer by its JSON-RPC
result (`isError` is an unexpected answer), a WebSocket session by its echoes. Answers
that cannot carry the served-by tag (gRPC always, WebSocket upgrades outside a Rustybin
server's layer) are `unattributed`, neither upstream nor gateway.

**Fault injection.** The scenario's `faults` block (and the live change `faults`, which
replaces it whole) names a `preset` and overrides any field. Client faults are drawn per
request by fraction, the same way on every node:

| `faults.client` | What it does |
|---|---|
| `connection_churn` | The fraction sent on a new connection, closed after the answer |
| `abandon: {fraction, after}` | Cancelled `after` sending; counted as `abandoned`, neither answered nor failed |
| `slow_body: {fraction, bytes_per_second}` | Request bodies trickled at that rate |
| `malformed_json` | JSON bodies cut short (they expect a 4xx) |
| `wrong_content_type` | JSON bodies sent as `text/plain` (they expect a 4xx) |
| `oversized_headers: {fraction, bytes}` | An extra header of `bytes` (default 8192; they expect a 4xx) |
| `retry_on_5xx: {retries, synchronised}` | Immediate retries of 5xx answers; `synchronised` retries at the next whole second of the run clock on every node at once |
| `expired_tokens` | Minted JWTs (`identities.jwt`) sent expired (they expect a 4xx) |
| `http2` | The fraction sent over HTTP/2 (ALPN on TLS targets, prior knowledge on plain ones), the rest over HTTP/1.1 |

Server faults travel to the server nodes with the run and act on marked requests at
`/harness/*` and on `/health`:

| `faults.server` | What it does |
|---|---|
| `drop` | The connection is reset after the headers |
| `stall: {fraction, for}` | Nothing comes after the headers for `for` (longer than the client's timeout: a timeout) |
| `flapping_health: {every, down_for}` | `/health` answers 503 for `down_for` of every `every` |
| `churn: {every, down_for}` | Node churn: in each `every`, one server node in turn is down for `down_for`: marked requests get a 503 with the connection closed and `/health` answers 503, which exercises the gateway's health checks and circuit breakers |

Presets: `calm`, `jittery` (connection churn, abandoned requests, slow bodies, an HTTP/2
mix), `flaky-upstream` (drops, stalls, flapping health), `retries`, `synchronised`,
`churn` and `all`. The report counts the injected faults by name (`totals.faults`), the
abandoned and unattributed answers, and the built-ins `llm-streaming`, `flaky-upstream`
and `jittery-clients` show them.

A running run takes changes without stopping: `PATCH /_rustybin/harness/runs/<run>` (or
`.../clusters/<id>/runs/<run>`) with any of `rate_multiplier` (0 to 10), `pause`,
`resume`, `hold` (freeze the pattern where it is; `false` releases it), `skip_stage`,
`set` (`{"<layer>.pattern.<field>": value}`; `then.` reaches the next stage of a chain),
`add_layer` and `remove_layer`, for the whole run or one `layer`. A change is checked
like a new scenario, against the caps of every client node for its share, applied on
the scheduler's next pass (in a cluster, the heartbeats bring it to every node at once),
logged with the run time it was made at, and carried in the snapshot, so it survives a
failover. The report lists the changes, the findings of the closed loops and every
stage as it ran (a hold stretches its stage, a skip cuts it short) with the run's
results in each. `GET .../runs/<run>/events` is the run's feed (server-sent events):
a `snapshot` every second, `change`, `finding` and `finished`, plus `leader_changed`
and `scale_request` for a cluster run.

`scale.client_nodes: auto` lets the leader size a cluster run: when the client nodes
it has cannot carry the run's peak rate (their `RUSTYBIN_HARNESS_MAX_RATE`, or
`scale.max_rate_per_node` when lower), the run waits and the leader emits a
`scale_request` (`needed_client_nodes`, `client_nodes`, `reason`) on the feed and, with
`RUSTYBIN_HARNESS_SCALE_HOOK`, POSTs it to the hook with `X-Rustybin-Cluster: <id>` and
`X-Rustybin-Signature: sha256=<hex>`, the HMAC-SHA256 of the body keyed with the hex
SHA-256 of the cluster's join token. A controller that provisions nodes makes them join;
the run starts once they are there, or after `scale.scale_timeout` (5 minutes by
default) undersized, with a warning in the report. Rustybin never starts machines
itself. `scale.connections_per_client` caps the keep-alive connections of each virtual
client (the pool per target is the node's virtual clients times this, within the in-flight cap), which sets
how many connections the gateway sees.

The same engine runs from the command line: `rustybin harness run scenario.yaml
--local-server` loops a scenario through an in-process Rustybin, `rustybin harness
validate` checks a scenario offline, `rustybin harness report` renders a saved report
(text or JUnit). `GET /_rustybin/harness/schema` is the JSON Schema of a scenario.

## Command line and saved tests

The [`rustybin` command line](cli-guide.md) and `rustybin mcp-stdio` read these on the
machine they run on, not on the instance; `RUSTYBIN_TEST_*` and `RUSTYBIN_NOTIFY_*` are
read by the instance.

| Variable | Used by |
|---|---|
| `RUSTYBIN_URL` (also `RUSTYBIN_SERVER`) | The instance a command calls when there is no `--url` (it wins over the profile; with neither, `http://localhost` on `RUSTYBIN_HTTP_PORT`, port 80 when unset). |
| `RUSTYBIN_TOKEN`, else `RUSTYBIN_ADMIN_TOKEN` | The credential when there is no `--token`. Sent only to `RUSTYBIN_URL`'s instance or one on this machine; plain http to another host warns. |
| `RUSTYBIN_PROFILE` | The profile of `rustybin login` to use (also for `rustybin mcp-stdio`). |
| `RUSTYBIN_CONFIG_HOME` | Where `profiles.json` lives (else `$XDG_CONFIG_HOME/rustybin`, else `~/.config/rustybin`). |
| `RUSTYBIN_MOCK_TOKEN` | A self-service manage token, when there is no `--mock-token`. |
| `RUSTYBIN_MCP_URL` | `rustybin mcp-stdio`'s `--server` ([Management MCP](agent-mcp.md)). |
| `RUSTYBIN_TEST_<NAME>` | On the instance: the value of a [saved test](saved-tests.md)'s `{{NAME}}` variable when the run does not pass it (only that prefix is ever read; a [scheduled run](saved-tests.md#schedules) reads nothing else). |
| `RUSTYBIN_NOTIFY_WEBHOOK_TOKEN` | On the instance: sent as `Authorization: Bearer` with every [test schedule notice](saved-tests.md#the-failure-notice). Seeds the secret setting `notify.webhook_token` (changeable in the console's Management, Network tab). Never exposed. |
| `RUSTYBIN_NOTIFY_BASE_URL` | On the instance: the address people open it at (`https://mocks.example.com`); a notice's `run_url` is it plus `/_rustybin/tests/runs/<id>`. Seeds `notify.base_url`; unset, notices carry `run_path` only. |

## Development only

| Variable | Used by |
|---|---|
| `RUSTYBIN_UPDATE_README` | `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints` rewrites the endpoint tables in `README.md`. |
| `RUSTYBIN_BIN`, `RUSTYBIN_DOCS_PORT`, `REQUIRE_ALL_TOOLS` | `docs/examples/run.sh` (see [How the examples work](examples.md)). |
| `PYTHON`, `RUSTYBIN_CONFORMANCE_PORT`, `RUSTYBIN_URL`, `MCP_BASE` | The SDK conformance scripts under `conformance/`. |
| `RUSTYBIN_UI_PORT`, `RUSTYBIN_JWT_URL`, `RUSTYBIN_SS_URL`, `RUSTYBIN_HARNESS_URL`, `RUSTYBIN_PASSWORD_URL`, `CONTROL_JWT_PRIVATE_KEY`, `CONTROL_JWT_AUDIENCE` | `conformance/ui/run.sh` and `console_e2e.py` (the console check, including the sign-in flows, self-service and the harness builder). |
| `RUSTYBIN_GIT_SHA` | Build time: the commit reported by `/_rustybin/version` and the metrics when the source tree has no `.git` (Docker builds: `--build-arg RUSTYBIN_GIT_SHA=<commit>`). |

## Limits that are not configurable

Request-reachable state is always bounded. The main caps (normal mode / public mode):

| What | Normal | Public |
|---|---|---|
| `X-Rustybin-Delay`, `/drip` duration and delay, JSON-RPC `sleep`, bin response delay | 30 s | 10 s |
| `/delay/{duration}` | 60 s | 10 s |
| `/bytes`, `/stream-bytes`, `/range` size | 100 KiB | 10 KiB |
| `/stream/{n}` lines | 100 | 20 |
| `/drip` bytes | 10240 | 1024 |
| `/sse` events / interval / stream lifetime | 1000 / 60 s / 10 min | 100 / 10 s / 2 min |
| `/sse/chat` tokens / delay per token | 500 / 2 s | 200 / 1 s |
| JSON-RPC batch size | 100 | 20 |
| gRPC message size, and the bytes of one `ClientStream` call | `RUSTYBIN_BODY_LIMIT` | same |
| WebSocket message size / idle timeout / lifetime | 1 MiB / 5 min / 1 h | 256 KiB / 1 min / 10 min |
| Connections at once (the HTTP and HTTPS listeners together) / time to send a request head | 16384 / 30 s | same |
| Request bins (total / per session / per address / requests per bin / bytes per bin / TTL) | 200 / 200 / 200 / 100 / 1 MiB / 24 h | 100 / 10 / 20 / 50 / 256 KiB / 1 h |
| Flaky counters (idle TTL 1 h) / per address | 10000 / no cap | 2000 / 200 |
| OAuth codes, refresh tokens, revocations / registered clients / registrations per address and minute | 10000 / 1000 / 100 | 2000 / 200 / 10 |
| Mock LLM request records (1 h TTL) / choices `n` / embedding dimensions | 1000 / 8 / 4096 | 200 / 2 / 1536 |
| MCP sessions (idle TTL) / long-lived streams | 1000 (30 min) / 1 h | 200 (10 min) / 5 min |
| A2A tasks (total / per session / idle TTL / bytes in all) | 5000 / 1000 / 1 h / 256 MiB | 2000 / 50 / 15 min / 64 MiB |
| A2A message / task bytes | 256 KiB / 1 MiB | same |
| A2A webhook sinks / notifications per sink (1 h TTL) | 500 / 50 | 100 / 10 (ids of 16 characters or more) |
| OpenAPI mocks (mocks / spec size / operations / captured requests per mock / lifetime of API-created mocks) | 100 / 1 MiB / 500 / 200 / unlimited | 50 / 256 KiB / 200 / 50 / 24 h |
| Spec compiles per minute for a caller without admin rights (per visit, key or address) | 30 | same |
| MCP inspector check runs / relay calls at once | 2 / 16 | same |
| Registry token mints per address and minute | 60 | 10 |
| Inspector entries / captured bytes in all / captured headers per request | `RUSTYBIN_INSPECTOR_CAPACITY` (max 10000) / 64 MiB / 16 KiB | same |
| Metrics series (`route` values / LLM token series) | 1024 / 256 (beyond: `other`) | same |
