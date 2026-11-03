# The audit log

Who did what, and when, on one page: every change to the instance, every sign-in
and every refusal is an entry, recorded against a person (or the admin token, a
control JWT's subject, an API key, a visit), searchable from every surface. Reading
it needs `audit.read`, which only the built-in `admin` role grants.

An entry is:

| Field | What |
|---|---|
| `id` | Increasing; the cursor of the pages |
| `time` | When (UTC) |
| `actor` | `kind` (`user`, `admin_token`, `token`, `key`, `visit`, `open`, `anonymous`, `system`), the user's `id` and `name` (a token's subject, the key's id, the visit's fingerprint), and the `agent` when an [AI agent](agent-mcp.md#managing-agents) made the call |
| `via` | `api` (a route), `mcp` (a management tool), `system` (the instance on its own) |
| `action` | The route template and method (`PUT /_rustybin/projects/{name}`), or the tool (`deploy_mock`) |
| `event` | What the handler said happened: `login`, `logout`, `join`, `invite`, `reset`, `update`, `disable`, `delete`, `create_role`, `create_project`, `sso`, `sso_token`, `portal_login`, `close_item`, `open_item`, `revoke_agent`, `apply_config`, `git_pull`, ... |
| `targets` | The names it was about: `{kind, name}` (`mock`, `operation`, `user`, `role`, `project`, `setting`, `test`, `checkpoint`, `agent`, `owner`, `item`, `group`, `commit`, `section`, ...) |
| `result` | `status` (a route), `outcome` (`ok`, `error`, `refused`, `asked`: a tool waiting for the user's confirmation), the `reason` and `permission` of a refusal |
| `request_id`, `address` | The `X-Request-Id` and the client's address, as the sign-in throttle sees it |

**Never a value, a body, a header, a password or a token.** A settings change names
its keys, not their values; a sign-in names the user only when one has that name (a
name nobody has may be a password typed in the wrong field); a path parameter that is
a handle rather than a name (an MCP relay session, a consent request) is a short hash.

## What is recorded

- **Every mutating control route**: each `POST`, `PUT`, `PATCH` and `DELETE` under
  `/_rustybin/`, with the names in its path and those its handler adds (the mock a
  `POST /_rustybin/mocks` created, the keys of a settings change, the project a mock
  moved to). A new route is recorded without code. The exceptions, each with its
  reason, are the management MCP transport (each tool call is an entry of its own),
  the harness cluster's node traffic (joins, heartbeats, elections) and the `POST`s
  that compute and change nothing (spec previews, payload and rule checks, harness
  previews and validations).
- **Every management tool that changes something**, and every refused tool call,
  with the agent, the names its arguments give and the result.
- **Sign-ins**: the console's, failed ones too, invitations accepted, sign-outs,
  password changes, [single sign-on](sso.md) (`sso` with the groups that mapped, and
  `sso_token`), the [portal](portal.md)'s sign-in and sign-out.
- **People and access**: invitations, role and permission changes, users disabled
  or deleted, custom roles, [projects](projects.md) created, changed or deleted and
  resources moved between them, agents revoked (`revoke_agent` with the agent and its
  `owner`), portal items opened or closed.
- **The instance as code**: config applies (`apply_config` with the sections the
  document changed), git pulls with the commit they reached (`git_pull`; the
  automatic mode's pulls are `system` entries).
- **Every refusal that names a permission** (`403 permission_required`), reads
  included, and the data plane's (`/health` toggles, the global `/flaky` reset).

Requests without a credential are no entries, except sign-ins and a refusal on an
open control plane, and those are capped at 600 a minute for the whole instance (the
rest are counted in `suppressed`), so nobody outside can flush the log. Reading the
log is never an entry.

## Read the log

`GET /_rustybin/audit` answers the entries newest first, with `next_cursor` for the
page before and `log`, what is kept where:

```hurl
{{#include ../examples/concepts/audit.hurl:change}}
```

```hurl
{{#include ../examples/concepts/audit.hurl:list}}
```

The filters: `actor` (in the actor's kind, user id, name or agent), `action` (in the
route, the tool or the event), `target` (in a target's name; `kind:name` for one
kind, `mock:orders`), `since` and `until` (RFC 3339, a UTC day such as `2026-11-03`,
or an age: `15m`, `2h`, `7d`), `result` (`ok`, `error`, `refused`, `asked`, or a
status code); `cursor` and `limit` (1 to 500, 100 by default). A failed sign-in:

```hurl
{{#include ../examples/concepts/audit.hurl:signin}}
```

A refusal, and who may read the log:

```hurl
{{#include ../examples/concepts/audit.hurl:refused}}
```

```hurl
{{#include ../examples/concepts/audit.hurl:viewer}}
```

`GET /_rustybin/audit/stream` follows the log as Server-Sent Events: an `entry` event
(its `id` the entry's) per new entry that matches the same filters, at most 16
listeners:

```bash
curl -N -H "Authorization: Bearer $TOKEN" \
  "https://mocks.example.com/_rustybin/audit/stream?result=refused"
```

## Where it is kept

The last 10,000 entries are in memory. With `RUSTYBIN_STATE_DIR`, every entry is also
a line of `<state>/audit/audit-YYYY-MM-DD.jsonl` (one file per UTC day, JSON Lines,
mode `0600`), written by a thread of its own: a request never waits for the disk, and
an entry the writer cannot take in time stays in memory only (counted in
`not_written`). At startup the newest entries are read back into memory and the ids
continue. The files are for `grep`, `jq` or a log shipper:

```bash
jq -c 'select(.result.outcome == "refused")' /var/lib/rustybin/audit/audit-2026-11-03.jsonl
```

Two settings bound them, both in the `audit` group (`settings.security` to change, and
a document pulled from [git](source-control.md) cannot):

| Setting | Variable | Default | |
|---|---|---|---|
| `audit.retention_days` | `RUSTYBIN_AUDIT_RETENTION_DAYS` | `30` | UTC days of files kept (1 to 365); older days are deleted at the first entry of a day |
| `audit.max_bytes` | `RUSTYBIN_AUDIT_MAX_BYTES` | 256 MiB | The files' largest total: the oldest days go first, and when the day being written reaches it alone, new entries stay in memory until the next day |

```hurl
{{#include ../examples/concepts/audit.hurl:settings}}
```

The log itself is never exported: `rustybin.yaml` carries the two settings (under
`settings:`), not the entries. The tracing lines (`rustybin::auth`,
`rustybin::manage`, `rustybin::git`, ...) stay as they are.

## On every surface

| Surface | Where |
|---|---|
| API | `GET /_rustybin/audit` (`actor`, `action`, `target`, `since`, `until`, `result`, `cursor`, `limit`), `GET /_rustybin/audit/stream` (SSE, the same filters); `audit.read` |
| Console | **Management, Audit**: the filters, the table (an entry links to the user, the mock, the agent, the saved tests it names), **Older entries**, **Live tail**, the log's state and the **Retention** card (the `audit.*` settings) |
| CLI | `rustybin audit list [--actor A] [--action A] [--target T] [--since S] [--until U] [--result R] [--limit N] [--cursor C]`, `rustybin audit tail` (the same filters, `--max`, `--timeout`); `--json` for the entries as the API answers them ([reference](cli.md#audit)) |
| Management MCP | `list_audit` (`audit.read`): the same filters and `cursor`; names people typed under `untrusted_actor_name`, `untrusted_targets` and `untrusted_request_id` ([agent page](agent-mcp.md#agents-users-and-sign-in-sessions)) |
| `rustybin.yaml` | `audit.retention_days` and `audit.max_bytes` under `settings:` ([instance as code](configuration-as-code.md)); never the log |
