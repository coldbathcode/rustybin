# Step 08: Public instances: bins, sessions, visits, the demo IdP

## Goal

On a public instance one visitor cannot read another's traffic, host active content on the console's
origin, exhaust the shared visit budget, or use the demo IdP as an open redirect.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| high | `src/request_bin.rs:455-500,733-740` | an anonymous bin may answer `text/html` and `set-cookie` with no CSP or `nosniff`, on the console's origin: a fake sign-in page, cookie planting on the main domain | the `restrict::harden` rules: sandbox CSP, `nosniff`, active media types as `text/plain`, no cookie or CSP headers |
| medium | `src/ai/mod.rs:553-562`, `src/session.rs` | public mode refuses `?session=ip:...` but `X-Rustybin-Session: ip:203.0.113.7` is accepted: anyone who knows a gateway's egress IP lists its AI exchanges with prompts | refuse `ip:` session header values in `session::session_header` (in public mode at least); check the inspector and bins readers too |
| medium | `src/mocks/visits.rs:503-517,713-753`, `src/limits.rs:682-706` | anonymous visit and creation ledgers and session plan limits are keyed by the client's session header: rotating it fills `MAX_LEDGER` (100k) so new visitors get `Busy` until midnight; each start saves the whole ledger under the lock | key on the client IP (or IP plus session); a per-IP rate on visit starts; save off the request path |
| medium | `src/mocks/host.rs:29-32,118-140`, `src/session.rs:305-330` | host matching is case and trailing-dot sensitive: `rustybin-mock.dev./ui` serves the console on the visitor domain | lowercase and trim one trailing dot in `split_host_port` |
| medium | `src/oidc/clients.rs:56-61`, `src/oidc.rs:996-1001,1170-1186` | demo clients accept any redirect URI and validation errors 303 there before login: a phishing redirect from the demo domain | in public mode, demo clients redirect to loopback or same-origin only; an interstitial for others (owner decision, master plan section 6) |
| medium | `src/oidc/users.rs:99-101`, `src/oidc.rs:589,1229` | PBKDF2 (600k to 10M iterations) on the executor with no attempt limit; unknown users answer at once (user enumeration) | `spawn_blocking` behind a semaphore, a dummy hash for unknown users, a per-IP failure limiter |
| low | `src/oidc.rs:1422-1426` | forged tokens (the HS256 demo secret is public) flush the revocation store; `exp + 60` overflows | clamp the stored expiry with `saturating_add` to now plus the token TTL |
| low | `src/oidc.rs:819-822,868-873` | token exchange accepts HS256 subject tokens, so anyone gets an RS256 IdP token for any subject and groups | accept only RS256 tokens this IdP issued |
| low | `src/ai/auth.rs:237` | AI key compared with `!=` | `types::constant_time_eq` |
| low | `src/ai/store.rs:80-86` | the newest record with a client-chosen request id is found before the session check: another caller can hide a victim's record | filter by session inside the search |
| low | `src/mocks/handlers.rs:294-298` | `sub_path` computed from the decoded name's length on the raw URI | strip the raw first segment |
| low | `src/mocks/service.rs:40` | `ai` missing from `RESERVED_NAMES` (the static AI route shadows a mock named `ai`) | add it |

## Tests

A bin with `text/html` answers `text/plain` with the sandbox CSP; an `ip:` session header is
refused in public mode; rotating session headers from one IP hits the per-IP limit; the trailing-dot
host reaches the mock domain's 404; the demo IdP error path does not redirect off-origin in public
mode; a wrong password for a known and an unknown user take comparable time.

## Done when

The tests pass and `docs/src/mocks/self-service.md` and the public-mode page list the new limits.

## Build notes (2026-11-02)

- Every finding is fixed, except the trailing-dot host, which was already handled
  (`session::split_host_port` lowercases and drops one trailing dot, the domains are
  normalised at startup) and has its regression test
  (`the_mocks_domain_serves_nothing_else_with_a_trailing_dot`).
- Section 6's decision on the demo IdP: on a public instance a demo client (any
  redirect URI) is sent on at once only to loopback or the request's own host; any
  other URI gets a page with the link (`send_back`), for errors, denials and codes.
  Registered clients are unchanged.
- Bins go through `mocks::restrict::harden` on every instance.
- `session::session_header` ignores `ip:` values everywhere (the gRPC limiter too).
- `Caller.ledger` (`ip:<address>/<session>`) keys the visit and creation ledgers, and
  an address gets at most `SESSIONS_PER_ADDRESS` (20) entries a day before its new
  sessions share one (`Visits::ledger_entry`): keying on the address alone would give
  everyone behind one NAT a single anonymous visit (the docs examples caught it). On a public
  instance the session-scoped plan limits count per address
  (`LimitsConfig.by_address`). The visits file is serialised under the lock and
  written after it is released (`save_unlock`, through `write_private`); a background
  write lost the state across a quick restart, so it stays on the request.
- Demo IdP sign-in: `OidcState::authenticate` runs PBKDF2 on the blocking pool under
  4 permits, counts failures per address (10 a minute), and an unknown name checks a
  dummy hash when a users file with hashes is installed. The password grant checks
  before `token_inner` (which takes the result).
- Revocations clamp their expiry to now plus the access token's life; token exchange
  takes only RS256 subject and actor tokens (the catalogue example now shows the
  refusal of the HS256 sample).
- AI records: `RequestStore::get_visible` filters inside the search; the mock key is
  compared in constant time; the mock sub-path comes from the raw first segment;
  `ai` is a reserved mock name.
