# Shared mode and API keys

Shared mode runs one Rustybin deployment for many users, each with an API key:
every data-plane request carries a key, each key has its own limits and daily
quotas, and the request inspector and usage counters are scoped by key, so two keys
never see each other's traffic. It is how a hosted offering can give out cheap keys on
a multi-tenant cluster instead of one instance per user.

A normal instance can also simply require a key on its data plane
(`RUSTYBIN_REQUIRE_KEY`, [below](#a-key-on-a-normal-instance)), for example a
dedicated instance that should only answer its owner's clients.

Both are off by default.

## Turning it on

```bash
RUSTYBIN_SHARED_MODE=true \
RUSTYBIN_KEYS_FILE=/etc/rustybin/keys.json \
RUSTYBIN_ADMIN_TOKEN=change-me \
rustybin
```

`RUSTYBIN_SHARED_MODE=true` implies [public mode](sessions.md) (tighter caps,
instance-wide mutations forbidden without the admin token) and
[session-scoped limits](plans-and-limits.md) with daily quotas, the session being the
key id. The full list of variables is in
[Configuration](../configuration.md#api-keys-and-shared-mode).

## The key registry

Keys come from a JSON document, the registry, read from a file or fetched from a URL:

- `RUSTYBIN_KEYS_FILE=/path/keys.json`: read at startup and reloaded when the file
  changes (checked every 5 seconds).
- `RUSTYBIN_KEYS_URL=https://...` with `RUSTYBIN_KEYS_TOKEN`: fetched at startup and
  then every `RUSTYBIN_KEYS_POLL_SECS` (default 30) with
  `Authorization: Bearer <token>` and `If-None-Match` (the last `ETag`, else the
  document's `version`), so an unchanged registry costs a `304`.

On errors (unreachable URL, invalid JSON) the last good registry stays in use and a
warning is logged. A registry with more than `RUSTYBIN_KEYS_MAX` keys (default
200,000) is refused as a whole.

The registry never contains a key, only the SHA-256 of the whole key in lowercase hex:

```bash
{{#include ../../examples/concepts/_make-key-registry.sh:hash}}
```

```bash
{{#include ../../examples/concepts/_make-key-registry.sh:registry}}
```

- `id`: the public key id, `[A-Za-z0-9._:-]`, at most 128 characters. It names the
  key in the inspector, in usage counters and in logs; the secret never appears there.
- `hash`: SHA-256 of the presented key. Keys are matched by this hash only (in
  constant time), so any format works; `rbk_<id>_<secret>` with at least 160 random
  bits is a good one.
- `label`: free text echoed in the `X-Rustybin-Plan` response header (printable
  ASCII, at most 64 characters). Without a label the instance plan name is used.
- `status`: only `active` (or no status) keys are accepted. Any other value,
  `revoked` or `disabled` for example, behaves exactly like an unknown key, so
  revoking a key takes effect at the next reload.
- `limits`: optional per-key limits replacing the [instance limits](plans-and-limits.md)
  for that key; an absent field keeps the instance value and `0` means unlimited:
  `rps`, `burst`, `concurrency`, `streams`, `stream_secs`, `requests` (per UTC day),
  `egress_bytes` (response bytes per UTC day), `inspector_entries` and
  `inspector_ttl_secs`.

Until the registry was loaded once, data-plane requests get
`503 {"error":"rustybin_keys_unavailable"}` and `/_rustybin/ready` answers `503`, so
an orchestrator does not route traffic to a replica that cannot check keys yet:

```hurl
{{#include ../../examples/concepts/shared.hurl:ready}}
```

## Sending a key

Every data-plane route needs a key in the `X-Rustybin-Key` header, else in the
`rb_key` query parameter (for clients that cannot set headers). The landing page `/`,
`GET /_rustybin/ready` and the console's static files under `/ui` need none.

```hurl
{{#include ../../examples/concepts/shared.hurl:key_required}}
```

```hurl
{{#include ../../examples/concepts/shared.hurl:key_header}}
```

```hurl
{{#include ../../examples/concepts/shared.hurl:key_query}}
```

```hurl
{{#include ../../examples/concepts/shared.hurl:key_invalid}}
```

gRPC calls send the key as `x-rustybin-key` metadata (`UNAUTHENTICATED` without a
valid key, `RESOURCE_EXHAUSTED` over a quota).

## Limits and daily quotas

The key's limits replace the instance limits: the token bucket, requests in flight
and open streams are counted per key id, in the same bounded map as session counters
(at most 10,000 sets, idle ones dropped first). Over the rate limit a request gets the
usual [plan limit 429](plans-and-limits.md#responses) with `Retry-After`.

The daily quotas (`requests`, `egress_bytes`) reset at 00:00 UTC and never roll over.
Their counters are bounded too: at most three times `RUSTYBIN_KEYS_MAX` (key, day)
pairs, at least 1,024. Past that cap a key without a counter yet is let through
uncounted for the day and a warning is logged once, which only happens when
`RUSTYBIN_KEYS_MAX` is set far below the number of keys actually in use.
A key over a quota gets `429` until midnight UTC, with `Retry-After` in seconds and a
body naming the reset time and, when `RUSTYBIN_QUOTA_UPGRADE_URL` is set, an upgrade
link (`null` otherwise):

```hurl
{{#include ../../examples/concepts/shared.hurl:quota}}
```

As with plan quotas, a response in flight is never cut, so egress can end slightly
above the quota, and requests rejected by the rate limit do not use the quota.

### Several replicas

Each replica counts locally. To share one daily quota between replicas, point them
at a PostgreSQL database with `RUSTYBIN_QUOTA_STORE=postgres://...` (the binary must
be built with the cargo feature `quota-postgres`, which is off by default):

```bash
cargo build --release --features quota-postgres
RUSTYBIN_QUOTA_STORE=postgres://rustybin:secret@quota-db:5432/quota rustybin
```

Every `RUSTYBIN_QUOTA_FLUSH_MS` (default 2,000) each replica adds its new counts with
one statement and gets the day's totals across all replicas back:

```sql
insert into key_usage (key_id, day, requests, egress_bytes) values ($1, $2, $3, $4), ...
on conflict (key_id, day) do update set
  requests = key_usage.requests + excluded.requests,
  egress_bytes = key_usage.egress_bytes + excluded.egress_bytes
returning key_id, day, requests, egress_bytes
```

The table `key_usage (key_id text, day date, requests bigint, egress_bytes bigint,
primary key (key_id, day))` is created on startup when missing, and rows older than 35
days are deleted once a day. A quota can be overshot by about the flush interval
(plus the store's round trip) times the key's rate on each replica (0.4% for 100
requests per second, 100,000 a day and two replicas). Rate limits stay per replica, so route a key to one replica where
you can (consistent hashing on the key or a per-key hostname).

If the store is unreachable, replicas keep enforcing with their local counters, retry
with backoff (up to one minute) and report `rustybin_quota_store_up 0` in the
[metrics](control-plane-security.md#metrics); requests never fail because of the
store. TLS is used when the server offers it (`sslmode=disable` turns it off); the
certificate is verified against the public web roots plus the PEM file in
`RUSTYBIN_QUOTA_STORE_CA`.

## Inspector and usage per key

Captured requests are tagged with the key id (`key_id`). In shared mode every keyed
request is captured, each key keeps at most its `inspector_entries` entries for
`inspector_ttl_secs` (inside the instance's `RUSTYBIN_INSPECTOR_CAPACITY`), and the key
itself is the credential to read them: `GET /_rustybin/requests`,
`/_rustybin/requests/{id}` and the live feed `/_rustybin/requests/stream` accept the
key (header or `rb_key`) and only return that key's entries.

`X-Rustybin-Key` and the `rb_key` query value are redacted to `rbk_...` plus the last
4 characters, in the inspector, in [request bins](../reference/request-bin.md), in
the traffic of OpenAPI mocks and in [AI request records](../ai/gateway-features.md),
which are also scoped by key: a key only reads the `/ai/requests` records it sent. They
never appear in the logs (request lines log the path, not the query string). Echo
responses still return what the caller sent, since it is the caller's own key.

```hurl
{{#include ../../examples/concepts/shared.hurl:inspector}}
```

`GET /_rustybin/usage` with a key reports that key's limits and today's counters:

```hurl
{{#include ../../examples/concepts/shared.hurl:usage}}
```

A platform that proxies its own portal to the instance can use a
[control-plane JWT](control-plane-security.md) (`RUSTYBIN_CONTROL_AUTH=jwt`) with
scope `inspector` and the claim `rb_key=<key id>` instead of the key; it reads the same
routes for that key only. The example token is minted with
`_mint-control-jwt.sh <key.pem> <audience> inspector 900 kdocsalpha`:

```hurl
{{#include ../../examples/concepts/shared.hurl:jwt_bound}}
```

## The rest of the control plane

Everything else under `/_rustybin/*` needs the admin token (`RUSTYBIN_ADMIN_TOKEN`, or
a control JWT with the `admin` scope); without an admin token configured it is
forbidden. That includes the web console's API calls, so in shared mode the console is
an admin tool. Instance-wide mutations (health toggles, clearing all captured
requests) stay forbidden without the admin token, as in public mode.

```hurl
{{#include ../../examples/concepts/shared.hurl:admin_only}}
```

## Per-key usage for a collector

`GET /_rustybin/keys/usage?day=YYYY-MM-DD&after=<key id>&limit=1000` (admin) returns
one UTC day of per-key totals, sorted by key id, for billing or analytics collectors.
`day` defaults to today, `limit` to 1,000 (the maximum); continue with
`after=<next_after>` until `next_after` is `null`. With a quota store the totals come
from it (every replica); without one they are this replica's counters, kept for today
and the two previous days.

```hurl
{{#include ../../examples/concepts/shared.hurl:keys_usage}}
```

The [metrics](control-plane-security.md#metrics) add `rustybin_keys_active` (keys in the
registry) and `rustybin_keys_last_sync_timestamp_seconds` (last successful registry
check, useful to alert on a stale registry), plus `rustybin_quota_store_up` with a quota
store. Quota rejections count in `rustybin_limit_rejections_total{dimension="requests"}`
and `{dimension="egress"}`.

## A key on a normal instance

`RUSTYBIN_REQUIRE_KEY=true` uses the same registry sources on a normal (not shared)
instance: the data plane needs a valid key, nothing else changes. There are no per-key
limits (the instance plan applies), and control-plane authentication stays whatever
`RUSTYBIN_CONTROL_AUTH` says. Per-key request counts still feed
`/_rustybin/keys/usage`.

```hurl
{{#include ../../examples/concepts/require_key.hurl:require_key}}
```
