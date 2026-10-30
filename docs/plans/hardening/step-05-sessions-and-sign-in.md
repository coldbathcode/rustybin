# Step 05: Sign-in: revocations that survive, rate limits that hold

## Goal

Sign-out, "sign out everywhere", a password change and an agent revocation stay in force across a
restart and across each other. The login limit holds under concurrency and behind any proxy.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| medium | `src/login.rs:314,353` | the generation starts at 1 and the denylist is in memory; with a stable signing key (`RUSTYBIN_ADMIN_TOKEN` or `RUSTYBIN_CONTROL_OAUTH_KEY`) a stolen 12 h token works again after a restart | persist the generation and the denylist (`<state>/sessions.json`, 0600), or seed the generation from startup time so it only grows |
| medium | `src/login.rs:375` (`rotate`) | `rotate()` clears one denylist that also holds agent revocations (`agents/mod.rs:1150`); `Agents::load` re-adds OAuth subjects only (`:834`) | a separate agent revocation set `rotate()` never clears, rebuilt from the agent records at load |
| medium | `src/control_oauth.rs:353-390` | OAuth access and refresh tokens carry no generation: a 30-day refresh token outlives a password change | `rb_gen` in OAuth tokens, checked in `verify` |
| medium | `src/login.rs:779,794` | `check(ip)` runs before the credential check and `failed(ip)` after it: hundreds of parallel guesses pass; each runs argon2id (19 MiB) on `spawn_blocking` with no concurrency cap, so about 512 parallel logins is about 10 GiB without credentials | count the attempt as pending in `check`; a global semaphore (4 permits) around argon2 answering 429 when full |
| medium | `src/session.rs:228-257` | with `RUSTYBIN_TRUST_FORWARD` the first of `Fly-Client-IP`, `Forwarded`, `X-Forwarded-For`, `X-Real-IP` wins; behind any other proxy a fresh `Fly-Client-IP` per request defeats every per-IP limit | a setting naming the one trusted header (`fly`, `forwarded`, `xff` rightmost hop), on all five surfaces |
| low | `src/admin.rs:50-72` | with no admin token configured, `admin_check` returns `Ok` for a valid non-admin JWT: a console-scope JWT reads settings, the config export, git status and the agents list | return 403 whenever `valid_jwt` is set and the scope lacks admin, whatever `admin_token` is |
| low | `src/control_oauth.rs:950` | unauthenticated register plus authorize in a loop evicts a real user's pending consent from the shared store (1000, 200 in public mode; `BoundedStore::insert_until` drops the entry that expires first, `src/oidc/store.rs:65-76`) | per-address authorize limit, or a small quota per client id |

## Tests

A restart (two `Tokens` built over the same state dir) keeps a revoked token revoked; `rotate()`
keeps an agent revocation; an OAuth token minted before a password change is refused; 50 parallel
wrong logins from one address count as 50; a spoofed `Fly-Client-IP` behind `xff` mode is ignored;
`admin_check` with a console JWT and no admin token is 403.

## Done when

The tests pass and `docs/src/security.md` (or the sign-in page) states what survives a restart and
which forwarded header is trusted.
