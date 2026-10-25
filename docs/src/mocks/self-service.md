# Self-service mocks

On a [public instance](../concepts/sessions.md#public-mode) mocks are normally
read-only: changing one needs the admin token. With self-service on, visitors
create and manage **their own** mocks without an account: drop a spec into the
console, get a URL, point a gateway at it. Other visitors cannot see, change or
delete those mocks, and the instance's own mocks stay read-only for them.

It is made for a hosted demo, so it comes with a time allowance (one hour,
extendable), an optional email step to keep going, a separate domain for the
mocks and restrictions on what a visitor's mock may send. It is **off by
default**, also in public mode.

## Turning it on

```bash
RUSTYBIN_PUBLIC_MODE=true \
RUSTYBIN_MOCKS_SELF_SERVICE=true \
RUSTYBIN_MOCKS_SELF_SERVICE_HOST=rustybin-mock.dev \
RUSTYBIN_LEADS_WEBHOOK_URL=https://hooks.example.com/rustybin-leads \
RUSTYBIN_LEADS_WEBHOOK_TOKEN=change-me \
RUSTYBIN_PRIVACY_URL=https://example.com/privacy \
RUSTYBIN_ADMIN_TOKEN=change-me-too \
rustybin
```

Self-service needs public mode (outside it everyone may already manage mocks) and
a control plane visitors can reach: control auth `open`, or
[shared mode](../concepts/shared-mode.md), where API keys own their mocks. With
`RUSTYBIN_CONTROL_AUTH=token` or `jwt` it is ignored with a warning. Every setting
is in [Configuration](../configuration.md#self-service-mocks).

The examples on this page run against a documentation instance with the mocks
domain `mocks.example`, 10 minute periods (so Extend is open at once) and one
anonymous visit per session.

## A visit and its manage token

The first mock a visitor creates starts a **visit** and answers with a
`manage_token` (`rbm_` and 160 random bits), shown once. The server keeps only
its SHA-256. Every later call that manages the visitor's mocks sends it as
`X-Rustybin-Mock-Token`:

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_create}}
```

- The name is the slug the visitor picked (default: from `info.title`, at most 33
  characters) plus a random six character suffix, so mock names are not guessable
  and nobody can take a name someone else is using.
- More mocks from the same token join the visit, up to three per visitor
  (`RUSTYBIN_MOCKS_PER_OWNER`).
- `stateful` mode is not available to visitors: their mocks answer from the spec
  only, never with data callers sent.
- Visitor mocks cannot be renamed.

Anyone may call the mock's URL (that is the point). Everything else about it
belongs to its owner: for another caller it does not exist.

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_stranger}}
```

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_owner}}
```

The [console](../console.md#self-service) keeps the token in the browser's local
storage, so visitors never handle it. **Copy manage link** gives a URL with the
token in the fragment (`/ui/#/mocks&mock_token=...`, never sent to the server) to
open the mocks in another browser or hand them to a teammate.
`POST /_rustybin/mocks/visit/token` replaces the token (old links stop working).

## Time

The clock belongs to the visit: all of a visitor's mocks share one expiry, and
Extend moves it for all of them.

| Step | Default | Setting |
|---|---|---|
| A visit starts with | 1 hour | `RUSTYBIN_MOCKS_VISIT_MINUTES` (also the length of each extension) |
| Extend, without an email address, up to | 3 hours | `RUSTYBIN_MOCKS_ANON_HOURS` |
| Extend with an email address, up to | 6 hours per UTC day | `RUSTYBIN_MOCKS_DAILY_HOURS` |
| Anonymous visits per session and UTC day | 3 | `RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY` |

- Extend opens 15 minutes before the visit ends, one period at a time.
- The hours of a day count per email address, including the anonymous hours of
  the visit that gave it. An address used up today cannot start another visit
  until midnight UTC.
- When a visit ends, its mocks and their traffic are deleted.
- The session (`X-Rustybin-Session`, else the client IP) limits anonymous visits.
  That is a speed bump, not protection: the instance-wide caps below are.

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_visit}}
```

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_extend}}
```

When the free time is used, Extend answers `403 rustybin_email_required`; the same
happens when a session that used its anonymous visits creates a mock without a
token. The answer carries the privacy notice link and the consent mode for the
form:

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_email_required}}
```

The visitor then sends the address with Extend
(`{"email": "...", "marketing_consent": true}`), or starts a visit with it:

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_email_visit}}
```

| Answer | When |
|---|---|
| `409 rustybin_extend_too_early` | More than 15 minutes left (`extendable_at` says when) |
| `403 rustybin_email_required` | The next period needs an email address |
| `429 rustybin_daily_limit` | The address used today's hours (`resets_at`, `Retry-After`) |
| `429 rustybin_visit_limit` | The free time is used and email capture is off |
| `409 rustybin_mock_cap` | The visitor already has the most mocks allowed |
| `503 rustybin_mocks_busy` | The instance holds as many visits or visitor mocks as it may (`Retry-After`) |
| `429 rustybin_mocks_rate_limited` | Mocks created too fast (`Retry-After`) |
| `503 rustybin_self_service_paused` | An operator paused self-service |

The come-back answers carry `self_host_url`, a link to running your own Rustybin,
which has none of these limits.

## The email step and leads

Email capture is on when Rustybin has somewhere to deliver the address
(`RUSTYBIN_LEADS_WEBHOOK_URL` and/or `RUSTYBIN_LEADS_FILE`) **and** a privacy
notice to link (`RUSTYBIN_PRIVACY_URL`). Without it a visit simply ends after the
anonymous hours.

- **The form** (console) has one email field, a separate unticked "Send me product
  news" checkbox and a line with the privacy notice. Giving the address unlocks
  more time; the checkbox is the marketing consent. `RUSTYBIN_LEADS_CONSENT=notice`
  drops the checkbox where a notice is enough; confirm the wording with whoever
  handles your legal side.
- **Delivery**: Rustybin `POST`s each address to the webhook as JSON, with
  `Authorization: Bearer $RUSTYBIN_LEADS_WEBHOOK_TOKEN` when set, so any
  automation tool or CRM endpoint can take it:

  ```json
  {"email": "ada@example.com", "marketing_consent": true, "consent_mode": "checkbox",
   "source": "mock_extend", "captured_at": "2026-10-18T10:00:00Z",
   "visit_minutes": 240, "mocks": ["orders-k3f9q2"]}
  ```

  Failed deliveries are retried with backoff for an hour from a bounded queue
  (1,000 leads); `RUSTYBIN_LEADS_FILE` appends the same JSON, one line per lead.
- **Privacy**: the address is never logged, captured in traffic or the inspector,
  exported or counted with a label. The daily ledger keeps only a salted hash
  (`RUSTYBIN_LEADS_SALT`; generated at startup when unset, so set it to keep daily
  counts across restarts), and `GET /_rustybin/mocks/visit` only says
  `email_given`.
- Addresses are not verified; your mailing tool's double opt-in can do that.

## The mocks domain

A visitor controls the bodies and headers of their mock, so visitor mocks should
not share a site with the console or your main domain. Set
`RUSTYBIN_MOCKS_SELF_SERVICE_HOST` to a **separate registrable domain** (not a
subdomain of your site, which browsers treat as the same site and reputation
systems such as Safe Browsing can flag together with it):

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_call}}
```

- `<name>.<domain>/<path>` is served by that mock; the served spec names that URL
  as its server.
- The domain serves nothing else: the console, the control plane and every other
  route answer `404` there. The bare domain redirects to
  `RUSTYBIN_MOCKS_SELF_SERVICE_HOME` (else `404`).
- On the main host, `/mock/<name>` of a visitor mock answers `404` with the right
  URL. Instance mocks keep working on both.

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_main_host}}
```

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_only_mocks}}
```

**DNS and TLS** (for example on Cloudflare): a proxied wildcard record
`*.rustybin-mock.dev` and the apex pointing at the instance; Cloudflare's edge
certificate covers the apex and the first-level wildcard; give the origin a
certificate for `*.rustybin-mock.dev` too (an origin certificate or ACME DNS-01)
and use SSL mode "Full (strict)". Set `RUSTYBIN_TRUST_FORWARD=true` behind the
proxy so sessions see client addresses.

Without a mocks domain, visitor mocks are served under `/mock/<name>` on the main
host with the same restrictions: fine for trying it out, not for a public demo.

## What visitor mocks cannot send

Whatever the spec or the settings say, every response of a visitor mock:

- carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY` and
  `Content-Security-Policy: sandbox; default-src 'none'; frame-ancestors 'none'`;
- never sets `Set-Cookie`, Content-Security-Policy, HSTS, `Clear-Site-Data` or
  `Access-Control-Allow-Credentials`, and never redirects to another host
  (`Location`, `Refresh` and `Content-Location` with an absolute URL are dropped);
- serves only inert media types: JSON (`*+json`), XML, YAML, CSV, plain text,
  `application/octet-stream` and form data. HTML, SVG, JavaScript and anything else
  go out as `text/plain`.

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_html_as_text}}
```

Dropped headers and media types show up as warnings in the
[deploy report](responses.md#deploy-checks); configured headers that would be
dropped are refused when saving.

## Limits

| What | Default | Setting |
|---|---|---|
| Mocks per visitor (or API key) | 3 | `RUSTYBIN_MOCKS_PER_OWNER` |
| Visitor mocks on the instance | 200 | `RUSTYBIN_MOCKS_SELF_SERVICE_MAX` |
| Running visits | twice the visitor mock cap | |
| Creations per session per hour | 10 | `RUSTYBIN_MOCKS_CREATE_PER_HOUR` |
| Creations on the instance per minute | 30 | |
| Blocked names | none | `RUSTYBIN_MOCKS_BLOCKED_NAMES` (comma separated globs, `*paypal*,login*`) |

Spec size, operations, rules, revisions and captured calls follow the
[public mode limits](../configuration.md#limits-that-are-not-configurable). A full
instance never evicts someone else's mocks.

## Operators

With admin rights (`RUSTYBIN_ADMIN_TOKEN`):

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_operator}}
```

| Route | |
|---|---|
| `GET /_rustybin/mocks?owner=all` | Every mock, with `owner_fingerprint` for visitor mocks (without `owner=all`, admins see the instance's mocks and their own) |
| `GET /_rustybin/mocks/visits` | Running visits: id, fingerprint, times, minutes granted, whether an email was given, mocks |
| `DELETE /_rustybin/mocks/visits/{id}` | End one visit and delete its mocks |
| `DELETE /_rustybin/mocks/visits` | End every visit |
| `POST /_rustybin/mocks/visits/pause` and `/resume` | Stop (or allow again) new visits and visitor mocks; running visits keep their time |

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_pause}}
```

Admins can change or delete any mock. The [metrics](../concepts/control-plane-security.md#metrics)
include `rustybin_mock_visits`, `rustybin_mock_visit_events_total{event}` and
`rustybin_mock_leads_total{result}` (never an address); per-mock request series
stay off in public mode. Visit starts, extensions and endings are logged with the
owner fingerprint, never the token or the address.

A visitor can end their own visit (and delete its mocks) at once:

```hurl
{{#include ../../examples/mocks/self_service.hurl:ss_end}}
```

## Shared mode

In [shared mode](../concepts/shared-mode.md) a mock created with an API key
belongs to that key: no manage token, no visit clock (the key's plan limits
apply), the same per-owner cap and response restrictions. Calls to the mock need
a key, like every data-plane request in shared mode.

## Restarts and replicas

With `RUSTYBIN_MOCKS_STATE_DIR`, visitor mocks are saved with their owner, and
visits and the day's ledger in `_visits.json`, so a restart keeps visits running;
mocks of visits that ended meanwhile are deleted on load. Captured traffic and
email addresses are never saved: the ledger keeps salted hashes of addresses. It
does keep each visit's session key, which is the client IP when the visitor sends no
`X-Rustybin-Session`, to count anonymous visits per day.

Visits live in memory on one instance. Run a self-service demo as a single
instance, or route every request for a mock (its host on the mocks domain) to the
same replica.

## Routes

| Route | Who | |
|---|---|---|
| `POST /_rustybin/mocks` | anyone | Create a mock (starts a visit without a token) |
| `GET /_rustybin/mocks/visit` | the visitor | The visit: `expires_at`, `extendable`, `next_extension` (`free`, `email`, `allowance` or `none`), minutes used and allowed, `email_given`, mocks |
| `POST /_rustybin/mocks/visit` | anyone | Start a visit, optionally with `{"email", "marketing_consent"}` |
| `POST /_rustybin/mocks/visit/extend` | the visitor | One more period (optionally with the email step) |
| `POST /_rustybin/mocks/visit/token` | the visitor | A new manage token |
| `DELETE /_rustybin/mocks/visit` | the visitor | End the visit, delete its mocks |
| Every `/_rustybin/mocks/{name}*` route | the owner, admins | As for any mock |
