# Plan: hosted self-service mocks

Status: **built** (phases 0 to 7, 2026-10-18). Decisions in section 15; section 16 lists where the build
differs from this plan. Follows open question 1 of [openapi-mocks.md](openapi-mocks.md). User documentation:
`docs/src/mocks/self-service.md`. What is left for the hosted demo is deployment (DNS and TLS for the mocks
domain, the settings, a lead webhook, a privacy page). Last updated: 2026-10-18.

## 1. Goal

Let a visitor to the hosted demo at rustybin.dev create their own OpenAPI mock without an admin token or
an account: drop a spec into the console, get a URL on `rustybin-mock.dev`, point a gateway at it, watch
the traffic. Other visitors cannot see, change or delete that mock, and nothing one visitor does can hurt
the instance, its other users or the rustybin.dev domain.

The demo is also a lead source: a visitor gets one hour free, can extend to three hours without giving
anything, and gives an email address to keep going, up to six hours of use per day.

Today a public instance is read-only for mocks: every mutation goes through `admin::require_admin`, which
refuses without a token in public mode (section 12 of the mocks plan, "Public mode").

### What a visitor can do

1. Create up to 3 mocks from specs (console wizard, `POST`, or `rustybin mock push --server`). The name
   gets a random suffix and its own host: `petstore` becomes `https://petstore-k3f9q2.rustybin-mock.dev`.
2. Manage only their own mocks: edit, deploy, roll back, switch on and off, delete, read the traffic.
3. Use them for 1 hour, press Extend for more time up to 3 hours, then give an email address to continue,
   up to 6 hours per UTC day (section 7).
4. Move their mocks to another browser or a teammate with a manage link.
5. When the instance is full or their day is used up, get a clear page that says when to come back and
   how to run their own Rustybin (no limits).

### Non-goals (v1)

- Accounts, sign-in or billing. Ownership is a capability (a manage token) or an API key, never a user
  record. The email address is a lead and a usage key, not a login.
- Verifying email addresses (no outbound mail from Rustybin; the marketing tool can do double opt-in).
- Sharing one mock between owners with different rights. A manage link gives full control.
- More than one replica (section 10).
- `stateful` mode for visitor mocks (section 8).
- Anything that loosens the existing rules for admin, file and sample mocks.

## 2. How it fits the existing code

| Existing piece | Reuse |
|---|---|
| `MockStore` (`src/mocks/store.rs`) | Gains an `owner` per entry and per-owner caps. `MockLimits::for_config` already has public-mode caps and an expiry for API-created mocks (`ttl_secs`, `expires_at`); visitor mocks take their expiry from the visit instead. |
| Request bins (`src/request_bin.rs`) | Model for per-owner caps (`max_bins_per_session`) and "a full instance does not evict other people's data". |
| `admin::require_admin` | Still the only path for admin, file and sample mocks. A new `mocks::access` guard adds the owner path next to it. |
| Shared mode (`src/keys.rs`) | The key id is a ready-made owner for signed-in (keyed) users. `keys::control` already lets a data key read its own inspector and usage; mock routes join that list. |
| Secret redaction (`8d7b2f5`) | "Can change this mock" becomes "admin or owner"; the redaction itself does not change. |
| Host routing (`src/mocks/host.rs`) | Serves visitor mocks on `<name>.rustybin-mock.dev` (section 6). |
| Persistence (`src/mocks/persist.rs`) | Saves owned mocks, visits and the daily ledger. |
| Plan limiter (`src/limits.rs`) | Already limits each caller's data-plane traffic; nothing new for calls to a mock. |
| Key ledger (`src/keys/ledger.rs`) | Model for the bounded per-day usage counters of section 7. |

## 3. Ownership

Every mock gets an `owner`:

| Owner | Created by | Who may change it |
|---|---|---|
| `admin` | API or console with admin rights (today's behaviour) | admin |
| `file` | `rustybin.yaml`, `RUSTYBIN_MOCKS_DIR` | nobody through the API (as today: reload replaces it) |
| `sample` | `RUSTYBIN_MOCKS_SAMPLE` | admin |
| `visit:<sha256>` | an anonymous visitor (manage token) | whoever presents the token, and admin |
| `key:<id>` | a data key in shared mode (a signed-in user) | that key (or a control JWT bound to it), and admin |

**Manage token, not the session key.** Bins are owned by the session key (`X-Rustybin-Session`, else the
client IP). That is fine for reading a bin but wrong for changing a mock: everyone behind one NAT shares
an IP, and anyone can send someone else's session header.

- The first mock a visitor creates starts a **visit** and returns a `manage_token` (`rbm_` plus 160 random
  bits, base32), shown once. Later creations from the same token join the same visit.
- The store keeps only the token's SHA-256 and compares in constant time, like the key registry.
- Callers send it as `X-Rustybin-Mock-Token` (not `Authorization: Bearer`, which already carries the admin
  token and control JWTs).
- The console stores it in the browser and sends it on every call, so visitors never handle it unless
  they move to another browser (section 11). `rustybin mock push` prints it.
- A lost token means the visit simply expires.

In shared mode the presented data key owns the mock (`key:<id>`), no manage token is issued, and the time
allowance of section 7 does not apply: the key's own plan limits do.

## 4. API changes

The existing `/_rustybin/mocks*` routes get one more way to authorise, plus three routes for the visit:

- `mocks::access::can_create(headers, config) -> Result<Owner, Response>`: admin as today; otherwise, when
  self-service is on (section 9), the visit of the presented token (a new visit when there is none), or
  `key:<id>` in shared mode. Refused when the visit has expired, its day is used up, or the instance is
  full (section 10).
- `mocks::access::can_manage(headers, config, entry) -> Result<(), Response>`: admin, or the entry's
  owner. Every mutation handler that now calls `admin::require_admin` calls this instead. A wrong or
  missing token on someone else's mock answers `404` (not `403`), so mock names cannot be probed.
- **Reads are filtered.** `GET /_rustybin/mocks` lists admin, file and sample mocks plus the caller's own
  (by token or key). Captured traffic (`GET .../requests*`), the spec, the report and the exports of an
  owned mock are owner-only: captures hold callers' headers and bodies.
- `control_auth::enforce` (modes `token` and `jwt`) still protects the whole control plane. Self-service
  only works where the control plane is reachable without credentials (`open`) or in shared mode, where
  `keys::control` lets a data key through to the mock routes. Turning self-service on with control auth
  `token` or `jwt` logs a warning and changes nothing.
- `POST /_rustybin/mocks/apply`, `reload` and the bulk actions stay admin-only: they act on more than one
  mock. `import` and `.../validate` change nothing and stay open to everyone allowed to read.
- New visit routes (owner only; `visit` becomes a reserved mock name):
  - `GET /_rustybin/mocks/visit`: `expires_at`, `extendable` (and why not), hours used today and the
    allowance, whether an email is on file (`email_given`, never the address), mocks used and the cap.
  - `POST /_rustybin/mocks/visit/extend` with an optional body `{"email": "...", "marketing_consent":
    true}`: adds one hour (section 7). Answers `200` with the new state, `403`
    `{"error": "rustybin_email_required"}` when the next hour needs an email, or `429`
    `{"error": "rustybin_daily_limit", "resets_at": ...}` with `Retry-After` when the day is used up.
  - `POST /_rustybin/mocks/visit/token`: rotates the manage token and returns the new one; the old one
    stops working at once.

## 5. Names

Every visitor mock gets a random suffix: `<slug>-<6 random base32 chars>` (30 bits), readable and too
sparse to guess or squat. The visitor picks the slug (default: slug of `info.title`); the server picks the
suffix and never lets a caller choose it. Admin, file and sample mocks keep their plain names. Slugs that
match one of them, a reserved name (`import`, `export`, `apply`, `reload`, `visit`) or a blocked pattern
(section 12) are refused. A suffix collision retries with a new one. The full name must still fit a DNS
label (63 characters), so slugs are cut to 56.

## 6. The mocks domain and response safety

A mock answers with bodies and headers the visitor controls. That is the main new risk: the demo must
not become free hosting for phishing pages, script injection into the console, or redirects, and a
blocklist entry for abuse on a mock must not take rustybin.dev (site, docs, console) down with it.

**Visitor mocks live on their own registrable domain, `rustybin-mock.dev`** (the GitHub
`githubusercontent.com` pattern). A subdomain of rustybin.dev is not enough: browsers treat it as the same
site (cookies set for `.rustybin.dev` reach it, and it can set them), and reputation systems such as Safe
Browsing can flag the parent domain.

- URLs: `https://<name>.rustybin-mock.dev/<path>`, rewritten to `/mock/<name>/<path>` by `host.rs`
  (`RUSTYBIN_MOCKS_SELF_SERVICE_HOST=rustybin-mock.dev`).
- That domain serves **only** mocks: `/ui/*`, `/_rustybin/*` and every other route answer `404` there
  (the console and control plane stay on rustybin.dev), apart from each mock's own
  `/_rustybin/openapi.json` and `openapi.yaml`. The bare `rustybin-mock.dev` redirects to
  `https://rustybin.dev`.
- On rustybin.dev, `/mock/<name>` for a visitor mock answers `404` with a pointer to its
  `rustybin-mock.dev` URL. Admin, file and sample mocks keep working on both.
- Deployment (operator side, not code): DNS on Cloudflare with a proxied wildcard record
  `*.rustybin-mock.dev` and the apex pointing at the demo instance; Cloudflare's edge certificate covers
  the apex and first-level wildcard; the origin needs a certificate for `*.rustybin-mock.dev` too (an
  origin certificate or ACME DNS-01), with SSL mode "Full (strict)". The self-service docs page (phase 7)
  gets these steps.

Every response from a visitor mock, on any host, also gets:

- `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox; default-src 'none'`, set last,
  so the spec cannot override them;
- dropped headers: `Set-Cookie`, `Location` and `Refresh` pointing at another host,
  `Content-Security-Policy*`, `Access-Control-Allow-Credentials`, `Strict-Transport-Security`,
  `Clear-Site-Data`, plus today's framing list (`RESERVED_RESPONSE_HEADERS`). Each is a deploy warning,
  like other spec header problems;
- a media type allowlist: JSON (`application/json`, `*+json`), XML, `text/plain`, `text/csv`,
  `application/octet-stream`, `application/x-www-form-urlencoded`. `text/html`, `image/svg+xml`,
  JavaScript and anything else is served as `text/plain` with a deploy warning.

The bodies are already bounded (`max_body_bytes`, 64 KB in public mode).

## 7. Time allowance and the email step

The clock belongs to the **visit**, not to each mock: all of a visitor's mocks share one expiry, and
Extend moves it for all of them. "Use" means wall-clock time with a live visit.

| Step | Allowance | What the visitor sees |
|---|---|---|
| Create the first mock | the visit starts with **1 hour** | "Your mocks expire in 60 min" with an Extend button |
| Extend (no email) | **+1 hour** each, up to **3 hours** of visit time | the new expiry |
| The next Extend | needs an email address | the email form (section 8) |
| Extend (email given) | +1 hour each, up to **6 hours of use per UTC day** for that email | the new expiry and "4 of 6 hours used today" |
| Day used up | none until 00:00 UTC | the come-back page: when the allowance resets, and how to run your own Rustybin |

Rules:

- Hours are counted when granted, in a bounded per-day ledger keyed by the email hash (section 8), the
  visit and the session key. The anonymous hours count towards the 6.
- Extend is offered from 15 minutes before expiry (so a visitor cannot bank six hours at once), and the
  expiry never moves more than one hour past now.
- A new visit (new token, cleared browser) gets a fresh anonymous hour, but each session key gets at most
  3 anonymous visits per UTC day; after that the email is asked for before the first mock. This is a
  speed bump, not protection: a client can change its session header, and many demo visitors share an
  office IP. The instance caps of section 10 are the real guard.
- An email already used up today cannot be reused on a new visit the same day.
- When a visit expires, its mocks and their traffic are deleted. The email (hash) and the day's ledger
  row stay until the day ends.
- No email capture configured (section 8): visits stop at 3 hours and the next Extend shows the
  come-back page.
- Signed-in users (shared-mode keys, admin) have no visit clock.

| Variable | Default |
|---|---|
| `RUSTYBIN_MOCKS_VISIT_MINUTES` (first hour and each extension) | 60 |
| `RUSTYBIN_MOCKS_ANON_HOURS` | 3 |
| `RUSTYBIN_MOCKS_DAILY_HOURS` | 6 |
| `RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY` (per session key) | 3 |

## 8. Email capture (leads)

The email form appears on the Extend past the anonymous hours (and before the first mock for a session
key that used its anonymous visits).

- **Form**: one email field; a separate, unticked "Send me product news about Rustybin" checkbox; a line
  saying what the address is used for, with a link to the privacy notice (`RUSTYBIN_PRIVACY_URL`,
  required to turn capture on). Giving the address is what unlocks more time; marketing consent is the
  checkbox, so the operator can mail people who opted in. This keeps the demo usable under GDPR and
  PECR style consent rules; the operator should still confirm the wording with whoever handles their
  legal side. `RUSTYBIN_LEADS_CONSENT=checkbox` (default) or `notice` (no checkbox, consent implied by
  the notice, for markets where that is enough).
- **Validation**: trimmed, at most 254 characters, one `@`, a domain with a dot, lowercased domain. No
  verification (non-goal); the marketing tool can send a double opt-in.
- **Delivery** (vendor neutral, the operator picks the tool): `RUSTYBIN_LEADS_WEBHOOK_URL` with
  `RUSTYBIN_LEADS_WEBHOOK_TOKEN` (sent as `Authorization: Bearer`). Rustybin `POST`s
  `{"email", "marketing_consent", "source": "mock_extend", "captured_at", "visit_hours", "mocks"}` (mock
  slugs only). Any automation or CRM endpoint can take it. A bounded in-memory queue (1,000 leads) retries
  with backoff for an hour; overflow and final failures are counted and logged without the address.
  Optionally `RUSTYBIN_LEADS_FILE` appends the same JSON lines to a file as well.
- **Privacy**: the address is personal data. It is never logged, never captured in traffic or the
  inspector, never in metrics or exports. The ledger keeps only `SHA-256(salt + address)` with a per-
  instance salt (`RUSTYBIN_LEADS_SALT`, generated at startup when unset), and only until the day ends.
  `GET /_rustybin/mocks/visit` returns `email_given: true`, never the address.
- Capture is on only when `RUSTYBIN_LEADS_WEBHOOK_URL` (or `RUSTYBIN_LEADS_FILE`) and
  `RUSTYBIN_PRIVACY_URL` are set: Rustybin never collects an address it cannot deliver or explain.

## 9. Configuration

| Variable | Default | Meaning |
|---|---|---|
| `RUSTYBIN_MOCKS_SELF_SERVICE` | `false` | Let callers without the admin token create and manage their own mocks. Public or shared mode only; ignored with a warning in normal mode (everyone is already allowed) and with control auth `token` or `jwt`. **Off by default, also in public mode**: turning it on is a decision about abuse handling and the mocks domain. |
| `RUSTYBIN_MOCKS_SELF_SERVICE_HOST` | unset | The mocks domain (`rustybin-mock.dev`). Unset: visitor mocks are served under `/mock/{name}` with the safety headers (fine for testing, not for the public demo). |
| `RUSTYBIN_MOCKS_PER_OWNER` | 3 | Mocks per visit or key. |
| `RUSTYBIN_MOCKS_SELF_SERVICE_MAX` | 200 | Visitor mocks on the instance (`max_mocks` rises to 250 when on). |
| `RUSTYBIN_MOCKS_CREATE_PER_HOUR` | 10 | Creations per session key per hour. |
| the time allowance | | section 7 |
| the leads settings | | section 8 |

Fixed (no variable): at most 30 creations per minute on the instance, and stateful mode off for visitor
mocks. The rest of `MockLimits` stays at its public-mode values.

**No `stateful` mode for visitor mocks.** `stateful` is a per-mock setting (`mode: stateful`,
`src/mocks/state.rs`) that stores what callers send (`POST /pets` then `GET /pets/{id}` returns it). It is
the closest thing in this feature to free file hosting, so visitor mocks answer from the spec only. It can
come later with tight caps (for example 50 items, 64 KB, text only).

## 10. One instance, and when it is full

- The demo runs as **one instance**. The store is in memory; scaling out later needs every request for a
  mock (its `rustybin-mock.dev` host) routed to the instance that holds it, or a shared store. Neither is
  part of v1.
- **Full instance**: when `RUSTYBIN_MOCKS_SELF_SERVICE_MAX` visitor mocks exist, or the creation rate
  cap is hit, creating answers `503` `{"error": "rustybin_mocks_busy"}` with `Retry-After` (seconds until
  the next visit expires, at least 60). The console shows the come-back page (section 11). Existing
  visits are never evicted to make room.
- With `RUSTYBIN_MOCKS_STATE_DIR`, owned mocks, visits (expiry, token hash) and the day's ledger are saved,
  so a restart or deploy does not cut visits short; expired files are deleted on load and by the sweep.
  Captured traffic and email addresses are never persisted (leads only ever leave through section 8).

## 11. Console

- **Create** stays the same wizard. On a public instance without an admin token it shows "1 hour, extend
  up to 3, more with your email" and "1 of 3 mocks". After the first creation it saves the manage token
  and offers "Copy manage link" (`/ui/#/mocks?mock_token=...`, a fragment, so it never reaches server
  logs). Opening a manage link stores the token and removes it from the address bar, like `#token=` in
  `ui/lib/auth.js`.
- Tokens live in `localStorage` (one key, a short list), so a closed tab keeps the visit; every
  `/_rustybin/mocks*` call sends them as `X-Rustybin-Mock-Token`.
- **Visit bar** on the mocks pages: time left (counting down), hours used today, and Extend (enabled from
  15 minutes before expiry). Extend opens the email form when the API answers `rustybin_email_required`.
- **Come-back page** (daily limit, busy instance, expired visit): what happened, when to come back (local
  time of `resets_at` or `Retry-After`), and "Run your own Rustybin" with the `docker run` one-liner and a
  link to the quick start.
- The mocks list splits into "Your mocks" and "Instance mocks" (admin, file, sample: read-only there).
  Mock detail shows the `rustybin-mock.dev` URL, Rotate token and "Forget on this browser".
- No new dependencies: vanilla ES modules; `conformance/ui/run.sh` gets a public instance with
  self-service on (fake clock through a test-only hook, so the hour steps run in seconds).

## 12. Operator tools

- `GET /_rustybin/mocks?owner=all` (admin): every mock with owner kind, a short owner fingerprint,
  creation time, expiry and request counts.
- Admin can delete any mock or end a visit, and block slug patterns (`RUSTYBIN_MOCKS_BLOCKED_NAMES`,
  comma separated globs, checked on create).
- `RUSTYBIN_MOCKS_SELF_SERVICE=false` plus SIGHUP (or a restart) stops new visits at once; running visits
  keep their time unless `RUSTYBIN_MOCKS_SELF_SERVICE_PURGE=true`.
- Metrics (bounded labels): `rustybin_mock_visits` (gauge), `rustybin_mock_visit_events_total{event}`
  (started, extended, email_required, daily_limit, busy, expired) and
  `rustybin_mock_leads_total{result}` (captured, delivered, retried, dropped). Per-mock request series
  stay off in public mode.
- Logs: one JSON line per visit start, extend, end and lead delivery result, with the owner fingerprint
  and session key, never the token or the address.

## 13. Phases

| Phase | Scope | Done when |
|---|---|---|
| 0. Ownership | `Owner` on entries, visits with manage token hashing, per-owner and instance caps, name suffixes, `mocks::access` guards, filtered reads, `404` for other owners. Settings in `Config`. | Unit tests in `store.rs` and `tests.rs`: two visits cannot see or touch each other's mocks; admin sees all; caps and `503`. |
| 1. Response safety | Safety headers, header denylist, media type allowlist, deploy warnings. | A test per dropped header and media type; admin mocks unchanged. |
| 2. Time allowance | Visit expiry, `/visit` and `/visit/extend` without email, anonymous cap, per-session-key visits, daily ledger, come-back errors, persistence of visits and the ledger. | Tests on a fake clock for every row of section 7; persist and reload round trip in a `tempfile` directory. |
| 3. Email capture | The email step, validation, salted hashes, webhook queue with retries, optional file, metrics, privacy rules. | Tests with a local webhook receiver: delivered, retried, dropped; no address in logs, captures or `/visit`. |
| 4. Mocks domain | `RUSTYBIN_MOCKS_SELF_SERVICE_HOST`, mocks-only routing on that host, apex redirect, pointer `404` on the main host. | Integration test with `Host` headers. |
| 5. Console | Token storage, manage links, visit bar, email form, come-back page, split list, Rotate. | `conformance/ui/run.sh` public self-service instance passes in light and dark. |
| 6. Signed-in users | `key:<id>` owners in shared mode, mock routes through `keys::control`, no visit clock. | Integration test with two keys on a shared instance. |
| 7. Operator tools and docs | `owner=all`, end visit, blocked names, purge, metrics, logs. Turn the roadmap entry into a "Self-service mocks" page in `docs/src/mocks/`, configuration rows, the Cloudflare and origin certificate steps, a `selfservice` instance in `docs/examples/run.sh` with Hurl examples (create, manage, `404` for strangers, extend, email step, caps). | Docs examples and `check_includes.py` pass. |

## 14. Testing

- Every rule in sections 4 to 10 has a unit test; time is injected (no sleeps), tokens and salts are
  fixed strings in tests (never generated RSA keys, per CLAUDE.md).
- The integration test starts a public instance with self-service and a local lead receiver, and runs two
  visitors (two tokens, two session headers) through create, list, change, read traffic, extend, the
  email step, the daily limit and delete.
- A test asserts that no response of a visitor mock carries `Set-Cookie`, a cross-host `Location` or an
  HTML media type, whatever the spec says.
- A test asserts that an email address never appears in logs, captures, metrics, exports or `/visit`.

## 15. Decisions (2026-10-18)

1. **Ownership**: a manage token per visit (section 3), not the session key; shared-mode keys own their
   mocks directly.
2. **Domain**: visitor mocks on `rustybin-mock.dev` (registered on Cloudflare), one host per mock, the
   domain serving nothing but mocks (section 6).
3. **Time**: 1 hour, extendable in 1 hour steps to 3 hours without an email; an email to continue; at most
   6 hours of use per UTC day (section 7). The email is a marketing lead, with a separate consent
   checkbox (section 8).
4. **Default**: self-service off, also in public mode (section 9).
5. **Scale**: one instance at launch; a busy instance shows a come-back page that points to running your
   own Rustybin (section 10). Scale out later.
6. **Stateful mode**: left out for visitor mocks (section 9).

## 16. As built (differences from the plan)

- **Code layout**: `src/mocks/visits.rs` (owners, visits, ledger, rates), `selfservice.rs` (`Caller`, the
  `readable` / `manageable` guards, visit and operator routes), `restrict.rs` (response rules), `leads.rs`
  (delivery); tests in `selfservice_tests.rs`, the end-to-end test with a real webhook in
  `tests/integration.rs`, the visitor flow in `conformance/ui/run.sh` (a third, self-service instance).
- **Names**: mock names stay at most 40 characters (`valid_name`), so slugs are cut to 33, not 56. Slugs that
  equal an instance mock's name are allowed: the suffix already makes the name different, and refusing them
  would refuse the most common upload (`petstore`). Reserved names and `RUSTYBIN_MOCKS_BLOCKED_NAMES` are refused.
- **Operator switch**: environment variables cannot change in a running process, so "set
  `RUSTYBIN_MOCKS_SELF_SERVICE=false` and SIGHUP" became `POST /_rustybin/mocks/visits/pause` and `/resume`, and
  the purge became `DELETE /_rustybin/mocks/visits` (no `RUSTYBIN_MOCKS_SELF_SERVICE_PURGE`). Also added:
  `DELETE /_rustybin/mocks/visits/{id}`, `POST /_rustybin/mocks/visit` (start a visit, with the email step up
  front) and `DELETE /_rustybin/mocks/visit` (the visitor ends their visit).
- **Caps**: visitor mocks are counted on their own (`RUSTYBIN_MOCKS_SELF_SERVICE_MAX`) next to `max_mocks` for
  the instance's mocks, instead of raising `max_mocks` to 250. Running visits are capped at twice that.
- **Extend**: the new expiry is the old one (or now, if later) plus one period; as Extend opens 15 minutes before
  the end, a visit is never more than one period and 15 minutes ahead.
- **Mocks domain**: `RUSTYBIN_MOCKS_SELF_SERVICE_HOME` sets where the bare domain redirects (unset: `404`),
  instead of a fixed redirect to rustybin.dev. The main host's pointer always names `https://<name>.<domain>`.
- **Response rules**: also `X-Frame-Options: DENY` and `frame-ancestors 'none'`, `Content-Location` treated like
  `Location`, and YAML and NDJSON added to the inert media types. Headers configured in settings that would be
  dropped are refused when saving (not only reported).
- **Keys**: key-owned mocks have no visit clock and keep the public-mode lifetime of API-created mocks (24 hours).
- **Leads**: the payload also carries `consent_mode`; an address given when starting a visit is a lead with
  `source: "mock_start"`. Metrics are `rustybin_mock_visits`, `rustybin_mock_visit_events_total{event}` and
  `rustybin_mock_leads_total{result}`.
- **Visitors' own mocks**: their secrets (API keys, tokens) are shown to their owner; the "export all" YAML gives a
  visitor the instance's mocks and their own, with secrets hidden.
- **Console test**: no test-only clock hook. The browser check runs an instance with 5 minute periods and no
  anonymous visits, so Extend is always open and the email step comes first; Extend with the email step is
  covered by the Rust tests on a manual clock.
