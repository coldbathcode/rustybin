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
