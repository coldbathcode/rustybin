# Plan: hosted self-service mocks

Status: draft, nothing built (2026-10-18). Follows open question 1 of
[openapi-mocks.md](openapi-mocks.md). Decisions marked **Recommendation** need a yes or no before phase 0.

## 1. Goal

Let a visitor to a shared, internet-facing instance (the free demo at rustybin.dev, or a shared-mode
cluster) create their own OpenAPI mock without an admin token: drop a spec into the console, get a URL,
point a gateway at it, watch the traffic. Other visitors cannot see, change or delete that mock, and
nothing one visitor does can hurt the instance, its other users or the domain it is served from.

Today a public instance is read-only for mocks: every mutation goes through `admin::require_admin`, which
refuses without a token in public mode (section 12 of the mocks plan, "Public mode").

### What a visitor can do

1. Create a mock from a spec (console wizard, `POST`, or `rustybin mock push --server`), within per-owner
   caps. The name gets a random suffix: `petstore` becomes `petstore-k3f9q2`.
2. Manage only their own mocks: edit, deploy, roll back, switch on and off, delete, read the traffic.
3. Keep a mock for a limited time (default 24 hours, extendable up to 7 days), with the expiry shown
   everywhere.
4. Move a mock to another browser or a teammate with a manage link.

### Non-goals (v1)

- Accounts, sign-in or billing. Ownership is a capability (a secret) or an API key, never a user record.
- Sharing one mock between several owners with different rights. A manage link gives full control.
- Mocks that span replicas. v1 runs on one instance or behind routing that keeps a mock on one
  instance (section 9).
- Anything that loosens the existing rules for admin, file and sample mocks.

## 2. How it fits the existing code

| Existing piece | Reuse |
|---|---|
| `MockStore` (`src/mocks/store.rs`) | Gains an `owner` per entry and per-owner caps. `MockLimits::for_config` already has public-mode caps and a 24 hour TTL for API-created mocks (`ttl_secs`, `expires_at`). |
| Request bins (`src/request_bin.rs`) | Model for per-owner caps (`max_bins_per_session`) and "a full instance does not evict other people's data". |
| `admin::require_admin` | Still the only path for admin, file and sample mocks. A new `mocks::access` guard adds the owner path next to it. |
| Shared mode (`src/keys.rs`) | The key id is a ready-made owner. `keys::control` already lets a data key read its own inspector and usage; mock routes join that list. |
| Secret redaction (`8d7b2f5`) | "Can change this mock" becomes "admin or owner"; the redaction itself does not change. |
| Host routing (`src/mocks/host.rs`) | Serves self-service mocks from a separate domain (section 6). |
| Persistence (`src/mocks/persist.rs`) | Saves owned mocks with their owner hash and expiry. |
| Plan limiter (`src/limits.rs`) | Already limits each caller's data-plane traffic; nothing new for calls to a mock. |

## 3. Ownership

Every mock gets an `owner`:

| Owner | Created by | Who may change it |
|---|---|---|
| `admin` | API or console with admin rights (today's behaviour) | admin |
| `file` | `rustybin.yaml`, `RUSTYBIN_MOCKS_DIR` | nobody through the API (as today: reload replaces it) |
| `sample` | `RUSTYBIN_MOCKS_SAMPLE` | admin |
| `token:<sha256>` | a visitor on a public instance | whoever presents the token, and admin |
| `key:<id>` | a data key in shared mode | that key (or a control JWT bound to it), and admin |

**Recommendation: a manage token, not the session key.** Bins are owned by the session key
(`X-Rustybin-Session`, else the client IP). That is fine for reading a bin but wrong for changing a mock:
everyone behind one NAT shares an IP, and anyone can send someone else's session header. Instead
`POST /_rustybin/mocks` answers with a `manage_token` (`rbm_` plus 160 random bits, base32), shown once.
The store keeps only its SHA-256 and compares in constant time, like the key registry. The caller sends it
as `X-Rustybin-Mock-Token` (not `Authorization: Bearer`, which already carries the admin token and
control JWTs). Losing the token means the mock expires on its own.

In shared mode the presented data key owns the mock (`key:<id>`) and no manage token is issued: the key
already authenticates every call.

## 4. API changes

No new routes. The existing `/_rustybin/mocks*` routes get one more way to authorise:

- `mocks::access::can_create(headers, config) -> Result<Owner, Response>`: admin as today; otherwise,
  when self-service is on (section 8), a new `token:` owner, or `key:<id>` in shared mode.
- `mocks::access::can_manage(headers, config, entry) -> Result<(), Response>`: admin, or the entry's
  owner. Every mutation handler that now calls `admin::require_admin` calls this instead. A wrong or
  missing token on someone else's mock answers `404` (not `403`), so mock names cannot be probed.
- **Reads are filtered.** `GET /_rustybin/mocks` lists admin, file and sample mocks plus the mocks owned
  by the tokens presented (`X-Rustybin-Mock-Token` may repeat, at most 20 values) or by the caller's key.
  `GET .../requests*` (captured traffic) needs `can_manage`: captures hold the caller's headers and
  bodies. The spec, report and exports of an owned mock are owner-only too.
- `control_auth::enforce` (modes `token` and `jwt`) still protects the whole control plane. Self-service
  only works where the control plane is reachable without credentials (`open`) or in shared mode, where
  `keys::control` lets a data key through to the mock routes. Turning on self-service on a `token` or
  `jwt` instance logs a warning and changes nothing.
- `POST /_rustybin/mocks/apply`, `reload` and the bulk actions stay admin-only: they act on more than one
  mock. `import` and `.../validate` change nothing and stay open to everyone allowed to read.
- New `POST /_rustybin/mocks/{name}/extend` (owner or admin): moves `expires_at` to now plus the TTL,
  never past `created_at` plus the maximum lifetime.
- `POST /_rustybin/mocks/{name}/token` (owner or admin): rotates the manage token and returns the new one;
  the old one stops working at once.

## 5. Names

**Recommendation: a random suffix for every self-service mock.** The name is
`<slug>-<6 random base32 chars>` (30 bits): readable, and too sparse to guess or squat. The visitor picks
the slug (default: slug of `info.title`); the server picks the suffix and never lets a caller choose it.
Admin, file and sample mocks keep their plain names, and slugs that match one of them, or a reserved name
(`import`, `export`, `apply`, `reload`), are refused. A collision on the suffix retries with a new one.

## 6. Response safety

A mock answers with bodies and headers the visitor controls, from a domain the operator owns. That is the
main new risk: a public instance must not become free hosting for phishing pages, script injection into
the console, or redirects.

**Recommendation: serve self-service mocks from their own registrable domain.** A subdomain such as
`mock.rustybin.dev` is not enough: it shares a site (and cookies set for `.rustybin.dev`) with the
console. Use a separate domain (for example `rustybin-mocks.dev`, with
`<name>.rustybin-mocks.dev` through `host.rs`) and, when self-service is on and that domain is set, answer
`/mock/{name}` for owned mocks on the main host with `404` plus a pointer to the right URL. Admin and file
mocks keep working on both.

Whatever the domain, every response from a self-service mock gets:

- `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox; default-src 'none'`, set last,
  so the spec cannot override them;
- refused or dropped headers: `Set-Cookie`, `Location` (and `Refresh`) with an absolute URL to another
  host, `Content-Security-Policy*`, `Access-Control-Allow-Credentials`, `Strict-Transport-Security`,
  `Clear-Site-Data`, plus today's framing list (`RESERVED_RESPONSE_HEADERS`). Dropped headers are deploy
  warnings, like other spec header problems;
- a media type allowlist: JSON (`application/json`, `*+json`), XML, `text/plain`, `text/csv`,
  `application/octet-stream`, `application/x-www-form-urlencoded`. `text/html`, `image/svg+xml`,
  JavaScript and anything else is served as `text/plain` with a deploy warning.

The bodies themselves are already bounded (`max_body_bytes`, 64 KB in public mode).

## 7. Limits and lifetime

Every cap applies on top of `MockLimits` for public mode:

| What | Default | Variable |
|---|---|---|
| Self-service mocks per owner | 3 | `RUSTYBIN_MOCKS_PER_OWNER` |
| Self-service mocks on the instance | 200 (of `max_mocks`, which rises to 250 when on) | `RUSTYBIN_MOCKS_SELF_SERVICE_MAX` |
| Creations per session key per hour | 10 | `RUSTYBIN_MOCKS_CREATE_PER_HOUR` |
| Creations on the instance per minute | 30 | none (fixed; a scripted flood) |
| Lifetime after creation or last extend | 24 h | `RUSTYBIN_MOCKS_TTL_HOURS` |
| Maximum lifetime | 7 days | `RUSTYBIN_MOCKS_MAX_LIFETIME_HOURS` |
| Spec size, operations, rules, revisions, captures | public mode values | as today |
| Stateful mode | off for self-service mocks | |

Creations per session key are a speed bump, not protection: a client can change its session header. The
instance-wide rate and cap are the real guard. A full instance answers `429` (`"error": "rustybin_mocks_full"`)
and never evicts someone else's mock, like bins in public mode.

**Recommendation: no stateful mode for self-service in v1.** It adds per-mock writable storage that
others can fill through the data plane. It can come later with its own per-mock byte cap.

Shared mode can take per-key caps from the registry later (an optional `mocks` member next to the key's
limits); v1 uses the instance defaults for every key.

## 8. Configuration

| Variable | Default | Meaning |
|---|---|---|
| `RUSTYBIN_MOCKS_SELF_SERVICE` | `false` | Let callers without the admin token create and manage their own mocks (public or shared mode only; ignored with a warning in normal mode, where everyone is already allowed, and with control auth `token` or `jwt`). |
| `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` | unset | Domain for self-service mocks (section 6). Unset: they are served under `/mock/{name}` with the safety headers. |
| the limits of section 7 | | |

**Recommendation: off by default, even in public mode.** Turning it on is a decision about abuse
handling and the second domain, so it should be explicit.

## 9. Persistence and replicas

- With `RUSTYBIN_MOCKS_STATE_DIR`, owned mocks are saved with their owner (`token:<sha256>` or
  `key:<id>`), `created_at` and `expires_at`; expired files are deleted on load and by the sweep. Captured
  traffic is never persisted (as today).
- The store is in memory per instance. A hosted demo with more than one replica needs routing that sends
  every request for one mock to the same replica (consistent hashing on the name, or the host label with
  the separate domain). A shared store (the PostgreSQL quota store is the obvious place) is a later step,
  not v1. The docs must say this plainly.

## 10. Console

- **Create** stays the same wizard. On a public instance without an admin token it shows the lifetime,
  the per-owner cap ("2 of 3 used") and, after creation, the manage token once with Copy and a manage link
  (`/ui/#/mocks/<name>?mock_token=...`, a fragment, so it never reaches the server logs).
- The console keeps manage tokens in `localStorage` under one key (a list of `{name, token}`), so mocks
  survive a closed tab; every `/_rustybin/mocks*` call sends them as `X-Rustybin-Mock-Token`. Opening a
  manage link adds the token and removes it from the address bar, like `#token=` in `ui/lib/auth.js`.
- The mocks list splits into "Your mocks" and "Instance mocks" (admin, file, sample: read-only there).
  Each owned mock shows its expiry with an Extend button, and "Forget on this browser".
- Mock detail adds Rotate token and shows the self-service URL (the separate domain when set).
- No new dependencies: still vanilla ES modules, with the browser checks in `conformance/ui/run.sh` getting
  a public instance with self-service on.

## 11. Abuse handling for the operator

- `GET /_rustybin/mocks?owner=all` (admin) lists every mock with owner kind, a short owner fingerprint,
  creation time, expiry and request counts.
- Admin can delete any mock and block a slug pattern (`RUSTYBIN_MOCKS_BLOCKED_NAMES`, comma separated
  globs, checked on create and rename).
- `RUSTYBIN_MOCKS_SELF_SERVICE=false` plus SIGHUP (or a restart) stops new creations at once; existing
  self-service mocks keep running until they expire, unless `RUSTYBIN_MOCKS_SELF_SERVICE_PURGE=true`.
- Metrics: `rustybin_mocks_owned` (gauge by owner kind) and `rustybin_mock_creations_total{result}`
  (created, owner_cap, instance_cap, rate_limited). Per-mock request series stay off in public mode.
- Logs: one JSON line per creation and deletion with the name, owner fingerprint and session key, never
  the token.

## 12. Phases

| Phase | Scope | Done when |
|---|---|---|
| 0. Ownership | `Owner` on entries, per-owner and instance caps, name suffixes, manage token hashing, `mocks::access` guards, filtered reads, `404` for other owners. Self-service flag and limits in `Config`. | Unit tests in `store.rs` and `tests.rs`: two owners cannot see or touch each other's mocks; admin sees all; caps and `429`. |
| 1. Response safety | Safety headers, header denylist, media type allowlist, deploy warnings for each. | Tests for every dropped header and media type; existing admin mocks unchanged. |
| 2. Lifetime | TTL, `extend`, maximum lifetime, token rotation, persistence of owners and expiry, creation rate limits. | Persist and reload round trip in a `tempfile` directory; expiry sweep test. |
| 3. Shared mode | `key:<id>` owners, mock routes through `keys::control`, no manage token. | Integration test with two keys on a shared instance. |
| 4. Separate domain | `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` through `host.rs`, main-host `404` pointer. | Integration test with `Host` headers. |
| 5. Console | Token storage, manage links, split list, expiry and Extend, Rotate. | `conformance/ui/run.sh` public instance passes in light and dark. |
| 6. Operator tools and docs | `owner=all`, blocked names, purge, metrics, logs. A "Self-service mocks" page in `docs/src/mocks/`, configuration rows, a `docs/examples/run.sh` instance with self-service on and Hurl examples for create, manage, `404` for strangers, extend and caps. | Docs examples and `check_includes.py` pass. |

## 13. Testing

- Every rule in sections 4 to 7 has a unit test; tokens are short fixed strings in tests (never generated
  RSA keys, per CLAUDE.md).
- The integration test starts a public instance with self-service on and runs two "visitors" (two tokens,
  two session headers) through create, list, change, read traffic, delete and caps.
- A test asserts that no response of a self-service mock carries `Set-Cookie`, a cross-host `Location` or
  an HTML media type, whatever the spec says.
- Hurl examples run on a new documentation instance (`selfservice` in `docs/examples/run.sh`).

## 14. Open questions

1. **Manage token or session key** for ownership (section 3). Recommendation: token.
2. **Separate domain**: is buying and pointing a second domain acceptable for the hosted demo (section 6)?
   Without it, the safety headers still apply but the mocks share a site with the console.
3. **Default lifetime and caps** (section 7): 24 hours, extendable to 7 days, 3 mocks per owner?
4. **Default for public mode**: off (recommended) or on (section 8).
5. **Replicas**: is one instance enough for the demo at launch, or does phase 0 need routing by mock name
   (section 9)?
6. **Stateful mode** for self-service mocks: leave out of v1 (recommended)?
