# Configuration

Rustybin is configured through environment variables; the only command-line
option of the server is `--config <file>`, the `rustybin.yaml` declaring
[OpenAPI mocks](mocks/config.md). Invalid values never
stop the server: they are logged as warnings and the default is used. Values are
trimmed, and an empty value counts as unset (except for `RUSTYBIN_CORS_ORIGINS`, see
below). Booleans accept `1`, `true`, `yes`, `on` and `0`, `false`, `no`, `off`
(case-insensitive).

`GET /_rustybin/config` returns the effective configuration without secrets (the
admin token is only reported as `admin_token_configured`).

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
| `RUSTYBIN_TRUST_FORWARD` | `false` | Trust proxy headers. Client IP: `Fly-Client-IP`, then RFC 7239 `Forwarded: for=`, `X-Forwarded-For` (the rightmost address that is not private or loopback, so clients cannot spoof it by prepending), then `X-Real-IP`. Scheme, host and port: `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Port` and `Forwarded: proto= host=`. The OIDC issuer also honours `X-Forwarded-Prefix`. Enable it only behind a proxy you control. |
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
| `RUSTYBIN_ADMIN_TOKEN` | unset | When set, instance-global mutations (health toggles, `POST /flaky/reset?scope=all`, clearing every captured request) need `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>` (or a control-plane JWT with the `admin` scope). With `RUSTYBIN_CONTROL_AUTH=token` or `jwt` it also opens the whole control plane. Never returned by any endpoint. |
| `RUSTYBIN_CORS_ORIGINS` | `*` | Allowed CORS origins, comma separated. `off` or an empty value removes the CORS layer entirely, so a gateway's own CORS plugin can be demonstrated. |
| `RUSTYBIN_INSPECTOR_CAPACITY` | `500` | Requests kept by the [request inspector](concepts/inspector.md) ring buffer (1 to 10000). |

## Control plane and console

See [Control-plane security, metrics and operations](concepts/control-plane-security.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_CONTROL_AUTH` | `open` | `open`: `/_rustybin/*` needs no credentials. `token`: every `/_rustybin/*` route except `/_rustybin/ready` needs `RUSTYBIN_ADMIN_TOKEN`. `jwt`: the admin token or an Ed25519 JWT signed by `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` (scopes `inspector`, `console`, `admin`). Data-plane routes and the console's static files are never affected. `jwt` by default in hosted mode. |
| `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` | unset | Ed25519 public key for control-plane JWTs: PEM (newlines may be `\n`), base64 DER, or base64 / base64url of the raw 32 bytes. Invalid: warning, JWTs refused. |
| `RUSTYBIN_CONTROL_JWT_AUDIENCE` | `RUSTYBIN_INSTANCE_ID` | Required `aud` claim of control-plane JWTs. |
| `RUSTYBIN_CONTROL_OAUTH` | `true` | OAuth sign-in for agents on the management MCP server ([Sign in with OAuth](agent-mcp.md#sign-in-with-oauth)): with `token` or `jwt` control auth (not in shared mode), `/_rustybin/mcp` answers `401` with RFC 9728 metadata and the built-in authorization server at `/_rustybin/oauth` issues tokens after approval on the console's consent page. `false` turns it off. |
| `RUSTYBIN_CONTROL_OAUTH_ISSUER` | unset | An external authorization server (absolute `http(s)` URL) named in the metadata instead of the built-in one, which then answers `404`. Its tokens must be control JWTs (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`, `RUSTYBIN_CONTROL_JWT_AUDIENCE`). Invalid: warning, built-in server. |
| `RUSTYBIN_CONTROL_OAUTH_KEY` | derived | Secret (32 characters or more) signing the built-in server's client registrations and tokens. Unset: derived from `RUSTYBIN_ADMIN_TOKEN`, else random per process (agents sign in again after a restart). Set the same value on every replica. Rotating it signs every agent out. Never returned by any endpoint. |
| `RUSTYBIN_HOSTED_MODE` | `false` | Preset for platforms running one instance per customer: control auth `jwt` and JSON logs, unless `RUSTYBIN_CONTROL_AUTH` / `RUSTYBIN_LOG_FORMAT` say otherwise. |
| `RUSTYBIN_CONSOLE_TITLE` | unset | Shown in the console header and tab title (control characters removed, at most 80 characters). |
| `RUSTYBIN_CONSOLE_BACKLINK` | unset | `http://` or `https://` URL shown as a "Back to ..." link in the console header and sign-in screen; anything else is ignored with a warning. |

## OpenAPI mocks

See [Configuration as code](mocks/config.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_CONFIG_FILE` | `./rustybin.yaml` if it exists | A `rustybin.yaml` declaring [OpenAPI mocks](mocks/index.md), loaded at startup and reloaded by `SIGHUP` or `POST /_rustybin/mocks/reload`. The command-line option `--config <file>` takes precedence. Problems are logged and listed in `GET /_rustybin/mocks` (`load_errors`); they never stop the server. |
| `RUSTYBIN_MOCKS_DIR` | unset | A directory whose `*.yaml`, `*.yml` and `*.json` OpenAPI files (up to 200) each become a mock named after the file, with default settings. |
| `RUSTYBIN_MOCKS_SAMPLE` | `true` (`false` in public mode) | Create the built-in `petstore` mock. |
| `RUSTYBIN_MOCKS_STATE_DIR` | unset | Save mocks created or edited through the API or console in this directory (one JSON file each, written on every change) and load them at startup. File-defined mocks are not saved. See [Configuration as code](mocks/config.md#keeping-console-changes). |
| `RUSTYBIN_MOCKS_HOST` | unset | Domain for host-based mock URLs, e.g. `mock.example.com`: a request to `petstore.mock.example.com/v1/pets` is served by the mock `petstore`. Needs wildcard DNS (and a wildcard certificate for HTTPS). See [Configuration as code](mocks/config.md#host-based-urls). |

## Self-service mocks

Visitors of a public instance create and manage their own mocks without the admin
token. See [Self-service mocks](mocks/self-service.md).

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_MOCKS_SELF_SERVICE` | `false` | Turn self-service on. Needs public mode (or shared mode) and control auth `open` (or shared mode); otherwise it is ignored with a warning. Off by default, also in public mode. |
| `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` | unset | The mocks domain (a separate registrable domain, e.g. `rustybin-mock.dev`): visitor mocks answer as `<name>.<domain>` and the domain serves nothing else. Unset: they are served under `/mock/<name>` (a warning says this is for testing). |
| `RUSTYBIN_MOCKS_SELF_SERVICE_HOME` | unset | Where the bare mocks domain redirects (else `404`). |
| `RUSTYBIN_MOCKS_PER_OWNER` | `3` | Mocks per visitor (or API key in shared mode). |
| `RUSTYBIN_MOCKS_SELF_SERVICE_MAX` | `200` | Visitor mocks on the instance (on top of the instance's own). |
| `RUSTYBIN_MOCKS_CREATE_PER_HOUR` | `10` | Mocks a session may create per hour. |
| `RUSTYBIN_MOCKS_VISIT_MINUTES` | `60` | First period of a visit and the length of each extension (1 to 1440). |
| `RUSTYBIN_MOCKS_ANON_HOURS` | `3` | Visit time without an email address. |
| `RUSTYBIN_MOCKS_DAILY_HOURS` | `6` | Time per email address and UTC day (never below the anonymous hours). |
| `RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY` | `3` | Anonymous visits per session and UTC day; after that the email step comes first (`0`: always). |
| `RUSTYBIN_MOCKS_BLOCKED_NAMES` | unset | Comma separated globs of names visitors may not use (`*paypal*,login*`). |
| `RUSTYBIN_LEADS_WEBHOOK_URL` | unset | Where captured email addresses are posted as JSON (retried for an hour). |
| `RUSTYBIN_LEADS_WEBHOOK_TOKEN` | unset | Sent as `Authorization: Bearer` to the webhook. Never exposed. |
| `RUSTYBIN_LEADS_FILE` | unset | A file the leads are appended to, one JSON object per line. |
| `RUSTYBIN_PRIVACY_URL` | unset | The privacy notice the email form links to. Email capture is off without it. |
| `RUSTYBIN_LEADS_CONSENT` | `checkbox` | `checkbox`: a separate, unticked marketing checkbox. `notice`: no checkbox. |
| `RUSTYBIN_LEADS_SALT` | generated | Salt of the email hashes in the daily ledger; set it to keep daily counts across restarts. Never exposed. |

`${VAR}` and `${VAR:-default}` in the config file are expanded from the environment
(see [Environment variables](mocks/config.md#environment-variables)). The
[`rustybin mock` CLI](mocks/cli.md) reads `RUSTYBIN_SERVER` (default
`http://localhost` on `RUSTYBIN_HTTP_PORT`, port 80 when unset) and `RUSTYBIN_TOKEN` (else `RUSTYBIN_ADMIN_TOKEN`) as
defaults for `--server` and `--token`.

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
| `RUSTYBIN_KEYS_URL` | unset | Key registry URL (`http://` or `https://`), fetched at startup and every `RUSTYBIN_KEYS_POLL_SECS` with `If-None-Match`. Errors keep the last good registry. |
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
| `RUSTYBIN_MCP_RESOURCE_URL` | derived | Override the resource identifier of `/mcp/protected` (by default `<scheme>://<host>/mcp/protected` from the request). Set it to the gateway's public URL when the gateway does not forward `Host`. |
| `RUSTYBIN_MCP_CLOCK_TICK_SECS` | `5` | Update interval of the subscribable `rustybin://clock` resource (1 to 3600). |

## Mock LLM

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_AI_REQUIRE_AUTH` | `false` | Require each provider's native credential on every `/ai/*` call (per request: `X-Rustybin-Require-Auth: true`). |
| `RUSTYBIN_AI_API_KEY` | unset | The only accepted key (implies `RUSTYBIN_AI_REQUIRE_AUTH`). For Bedrock SigV4 it is compared with the access key id. |

See [Faults, latency, credentials, inspection](ai/gateway-features.md#credential-checks).

## A2A agents

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_A2A_PUSH_ALLOWLIST` | unset | Comma-separated `host` or `host:port` entries that push notification URLs may point to. The built-in sink `/a2a/webhook-sink/{id}` on this server is always allowed. |
| `RUSTYBIN_A2A_PUSH_ALLOW_ALL` | `false` | Allow push notifications to any http(s) URL, including loopback and private addresses. Ignored in public mode. |

## Harness mode

Rustybin as a load client (`docs/plans/harness.md`): one node with its own engine and
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
| `RUSTYBIN_HARNESS_VERIFY_TARGETS` | `false` (`true` in hosted mode) | Before a run every target must serve this node's token at `/.well-known/rustybin-harness`. A gateway routing to the node passes, which also proves the route before any load is sent. |
| `RUSTYBIN_HARNESS_MAX_RATE` | `50000` | Requests per second per node (the sum of a scenario's layer peaks). |
| `RUSTYBIN_HARNESS_MAX_INFLIGHT` | `10000` | Requests in flight per node, and at most what the descriptor limit allows (the engine raises the soft limit to the hard limit at startup and keeps 256 descriptors free; in `both` mode each request costs two). Beyond it requests are counted as `not_sent` and the run is flagged as saturated. |
| `RUSTYBIN_HARNESS_MAX_DURATION` | `1h` | Longest run (`30m`, `2h`, up to `24h`). |
| `RUSTYBIN_HARNESS_THREADS` | half the cores (`both`), all but one (client only) | Worker threads of the engine runtime; the scheduler has its own thread. |
| `RUSTYBIN_HARNESS_DATA_PLANE` | `on` | `off` on a client-only node serves only `/`, `/_rustybin/*`, `/ui` and the verification path. |
| `RUSTYBIN_HARNESS_REPORT_DIR` | unset | Every final report is also written here as `<run_id>.json`. |
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
stops renewing leases, so the minority side of a partition stops sending; when it
hears a newer term it steps down and follows the new leader as a member. A crashed
leader that restarts is a new, empty node: start it as a standby (with
`RUSTYBIN_HARNESS_JOIN` pointing at the leader or any eligible member, which names
the leader) to rejoin the cluster. The node routes
(`.../nodes`, `.../nodes/<node>/heartbeat`, `.../election/vote` and
`.../election/elected`) authenticate with the join token and the node credential
instead of the control-plane credentials.

The same engine runs from the command line: `rustybin harness run scenario.yaml
--local-server` loops a scenario through an in-process Rustybin, `rustybin harness
validate` checks a scenario offline, `rustybin harness report` renders a saved report
(text or JUnit). `GET /_rustybin/harness/schema` is the JSON Schema of a scenario.

## Development only

| Variable | Used by |
|---|---|
| `RUSTYBIN_UPDATE_README` | `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints` rewrites the endpoint tables in `README.md`. |
| `RUSTYBIN_BIN`, `RUSTYBIN_DOCS_PORT`, `REQUIRE_ALL_TOOLS` | `docs/examples/run.sh` (see [How the examples work](examples.md)). |
| `PYTHON`, `RUSTYBIN_CONFORMANCE_PORT`, `RUSTYBIN_URL`, `MCP_BASE` | The SDK conformance scripts under `conformance/`. |
| `RUSTYBIN_UI_PORT`, `RUSTYBIN_JWT_URL`, `RUSTYBIN_SS_URL`, `RUSTYBIN_HARNESS_URL`, `CONTROL_JWT_PRIVATE_KEY`, `CONTROL_JWT_AUDIENCE` | `conformance/ui/run.sh` and `console_e2e.py` (the console check, including the sign-in flow, self-service and the harness builder). |
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
| WebSocket message size / idle timeout / lifetime | 1 MiB / 5 min / 1 h | 256 KiB / 1 min / 10 min |
| Request bins (total / per session / requests per bin / bytes per bin / TTL) | 200 / 200 / 100 / 1 MiB / 24 h | 100 / 10 / 50 / 256 KiB / 1 h |
| Flaky counters (idle TTL 1 h) | 10000 | 2000 |
| OAuth codes, refresh tokens, revocations / registered clients | 10000 / 1000 | 2000 / 200 |
| Mock LLM request records (1 h TTL) / choices `n` / embedding dimensions | 1000 / 8 / 4096 | 200 / 2 / 1536 |
| MCP sessions (idle TTL) / long-lived streams | 1000 (30 min) / 1 h | 200 (10 min) / 5 min |
| A2A tasks (total / per session / idle TTL) | 5000 / 1000 / 1 h | 2000 / 50 / 15 min |
| OpenAPI mocks (mocks / spec size / operations / captured requests per mock / lifetime of API-created mocks) | 100 / 1 MiB / 500 / 200 / unlimited | 50 / 256 KiB / 200 / 50 / 24 h |
| Inspector entries | `RUSTYBIN_INSPECTOR_CAPACITY` (max 10000) | same |
| Metrics series (`route` values / LLM token series) | 1024 / 256 (beyond: `other`) | same |
