# Step 02: Console sign-in with username and password

Master plan: [master-plan.md](master-plan.md). Status: **done 2026-10-29**. Needs: 01. About 1.5 days.

## Goal

The console and the control plane are locked by default behind a username and password,
`rustybin` / `rustybin` out of the box, set by environment variables at startup and changeable in
Management, Sign-in. Signing in gives the browser the same control token that agents, the CLI and
the MCP server use, so nothing else in the credential model changes.

## Read first

- `src/control_auth.rs`: `ControlAuth` (`open`, `token`, `jwt`), `Grant`, `enforce`, `verify_jwt`,
  `presented_token`, the exemptions (`/_rustybin/ready`, the OAuth sign-in paths, `/ui/*` static).
- `src/control_oauth.rs`: how it mints HS256 tokens bound to the instance (`sign`, `issue`, the
  secret derivation), the consent view, `audit`.
- `ui/lib/auth.js` (the sign-in screen, `#token=` fragment, sessionStorage), `ui/lib/http.js`
  (bearer on `/_rustybin/*`), `ui/index.html` meta tags set by `src/ui.rs`.
- `src/admin.rs`, `src/limits.rs` (rate limiting primitives and the injectable clock),
  `src/session.rs` (client IP).
- Step 01's settings store and Management area.
- `conformance/ui/console_e2e.py` `run_jwt` (sign-in flow checks to extend).

## Design

### Mode and credentials

- `RUSTYBIN_CONTROL_AUTH` gains `password` and it becomes the **default** when the variable is
  unset. `open`, `token` and `jwt` keep their meaning. In `password` mode the sign-in endpoint is on
  and admin tokens and control JWTs (`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`) are accepted as before, so
  platforms that mint JWTs lose nothing.
- `RUSTYBIN_CONSOLE_USER` (default `rustybin`) and `RUSTYBIN_CONSOLE_PASSWORD` (default `rustybin`),
  or `RUSTYBIN_CONSOLE_PASSWORD_HASH` (an argon2id PHC string, preferred for deployments; the
  `argon2` crate, pure Rust). A password changed in the console is stored as a hash in the settings
  store (`console.password_hash`, secret) and wins over the environment unless locked. The user name
  is `console.user`.
- Startup: in `password` mode with the default password, one WARN line
  ("the console uses the default password; set RUSTYBIN_CONSOLE_PASSWORD or change it in
  Management, Sign-in") and `GET /_rustybin/status` carries `"default_password": true`, which the
  console shows as a banner until it is changed. Public mode with the default password refuses to
  start (a public demo must set one or choose `open` on purpose).

### Sign-in flow

| Route | | |
|---|---|---|
| `POST /_rustybin/login` | open (never behind `enforce`) | `{username, password}`; answers `{token, expires_at, scopes}`; a control JWT (HS256, the OAuth signing secret, `rb_use: "console"`, scope `admin`, 12 h, `jti`) |
| `POST /_rustybin/logout` | bearer | Revokes the token's `jti` for its remaining life (a bounded denylist in memory, at most 4096 ids, expiring with the token) |
| `GET /_rustybin/session` | bearer | Who the token is: user, scopes, expiry, how it was obtained |

- Constant-time comparison; argon2id verification off the async runtime (`spawn_blocking`).
- Rate limit: 5 failed attempts per client IP per minute, then 429 with `Retry-After`, exponential
  up to 10 minutes; audited (`rustybin::auth`, user name, IP, outcome; never the password).
- The console: `ui/lib/auth.js` gains the username and password form on the existing sign-in screen
  (shown when `/_rustybin/status` says `control_auth: password`), keeps the token in sessionStorage
  as today, signs out through `/_rustybin/logout`, and shows "Signed in as rustybin, expires in 11 h"
  in the topbar. The `#token=` fragment still works for platform-issued tokens. "Remember me" keeps
  the token in localStorage for 12 h at most.
- Management, Sign-in: change the user name and password (current password required), see the
  active sessions (bounded list of issued `jti` with issue time, IP, last use) and end them, and
  "Sign out everywhere" (rotates a per-instance token generation counter that every console token
  carries, so older tokens fail).
- The CLI (step 10) signs in the same way (`rustybin login <url>`), the MCP server accepts the token
  like any control JWT (the `admin` scope), and `claude mcp add` snippets in the console offer the
  signed-in user's token.

### What stays open

`GET /` and `GET /_rustybin/ready` (liveness and readiness), `/ui/*` static files (the SPA shows
the sign-in screen), the OAuth sign-in routes, the data plane, the portal (step 7, its own setting).

## Tests

- Unit: password verification (plain env, hash env, stored hash precedence, lock), token mint and
  verify, `jti` denylist bounds and expiry, rate limiting on the injectable clock, generation
  rotation.
- Full app: default mode locks `/_rustybin/config`, sign-in with the defaults works and is flagged,
  wrong password is refused and rate limited, a signed-in token reaches the MCP server and the
  harness routes, logout revokes, public mode with the default password refuses to start, `open`
  keeps the old behaviour, existing `token` and `jwt` tests unchanged.
- Console check: the password sign-in screen, the default-password banner, change the password,
  sign out, "sign out everywhere".
- Hurl: `docs/examples/concepts/login.hurl` against a new examples instance in password mode.

## Docs

`docs/src/concepts/control-plane-security.md` (the new mode first, since it is the default; the
migration note), `docs/src/configuration.md` (variables), `docs/src/quick-start.md` (the first
sign-in with `rustybin` / `rustybin` and the "change it" step), `docs/src/console.md`,
`docs/src/agent-mcp.md` (the signed-in token as an agent credential), README quick start, the
changelog under "Changed" with the migration line, `docker-compose.yml` and `fly.toml` comments.

## Done when

A fresh `docker run` asks for `rustybin` / `rustybin`, warns in the log and in the console until
the password is changed; every surface accepts the signed-in token; all checks pass; committed and
pushed.

## Build notes

Built 2026-10-29 in one session. Where the build differs from the design above, and why:

- **One module, `src/login.rs`.** The credentials, the settings specs, the token mint and
  verification, the revocation state and the seven routes live together; `control_auth.rs`
  gained the `Password` variant (the `Default`), the open sign-in path and one line in
  `verify_jwt_claims` that hands HS256 tokens to `login::verify` in password mode. That one
  line is what makes every existing caller (the enforce layer, `admin_check`, the key layer's
  redaction, the settings audit principal, the management MCP's grant) accept a sign-in token
  without changes.
- **The runtime state lives in `Config`.** `Config.console_tokens: Arc<login::Tokens>` holds
  the generation, the revoked ids, the listed sessions and the failed attempts. It is not a
  setting; it is there because every credential check in the code base takes `&Config`
  (`admin_check` alone has 34 call sites), so a revoked token is refused on the data plane's
  admin guard too, not only on `/_rustybin/*`. Tests swap in a `Tokens::with_clock(ManualClock)`.
- **The signing key is the OAuth key's `console` subkey**, not the `jwt` one: the same
  instance key (`RUSTYBIN_CONTROL_OAUTH_KEY`, else derived from the admin token, else random),
  a separate HMAC label. A sign-in token is told apart from an OAuth access token by its
  payload (`rb_use: "console"`, read before verifying), so `control_oauth::authorize_manage`
  leaves it to control auth on `/_rustybin/mcp`. The audience is `RUSTYBIN_CONTROL_JWT_AUDIENCE`
  (the instance id by default), so with nothing set a restart signs everyone out; documented.
- **Precedence of the password**: the stored hash (unless locked), else
  `RUSTYBIN_CONSOLE_PASSWORD_HASH`, else `RUSTYBIN_CONSOLE_PASSWORD`, else `rustybin`. The
  hash secret validates as an argon2 PHC string, which needed a one-line addition to the
  settings store: `Spec.validate` now runs for secrets too. The user name is `console.user`
  (group `signin`); `default_password` is true when no hash exists and the plain password is
  absent or literally `rustybin`.
- **A password change ends every other session.** Instead of leaving older tokens alive,
  `PUT /_rustybin/password` rotates the generation and mints a fresh token for the caller
  when it signed in with one (the admin token gets none). The new password must be 8 to 256
  characters and not the default. `DELETE /_rustybin/settings/console.password_hash` with the
  admin token is the documented recovery of a forgotten password.
- **Shared mode keeps `open`** as its default control auth (the key layer authenticates
  the control plane and the console signs in for a key with `#token=`); the sign-in routes
  answer 404 there. Hosted mode keeps `jwt`. **Self-service needs `open` on purpose** now,
  since visitors reach `/_rustybin/mocks/visit*` without credentials: the config warning says
  so, and the hosted demo's documentation and scripts set it.
- **The default changed what the checks and CI assume.** The example runner, the console
  check and the compose cluster set `RUSTYBIN_CONTROL_AUTH=open` on the instances that read
  the control plane without a token, and each gained a new instance with nothing set
  (`password_url`, `RUSTYBIN_PASSWORD_URL`). The Docker smoke tests in both workflows now
  sign in with `rustybin` / `rustybin` and assert the 401 before it and the startup warning.
- **The sessions list is a convenience, not the truth.** It holds the last 256 issued
  tokens; revocation by id and the generation are what the verification checks. `GET
  /_rustybin/sessions` says `current` so the console marks the caller's own row. The
  management routes use their own admin guard (`credentials_grant` with the admin scope),
  not `admin::require_admin`, whose open-mode leniency does not apply to them.
- **Console**: the sign-in screen's password form with Remember me (`localStorage`, dropped
  when the token expires or on sign out), the token form folded under "Use a token instead",
  the banner under the top bar, the "signed in as" chip with the expiry, and the Sign-in tab
  (status, change the password, sessions with End and Sign out everywhere). The tab renders
  in every mode so the Management check finds a control per registered setting (`console.user`
  on the user name field, `console.password_hash` on the new password field).
- **Bounds**: 4096 revoked ids, 256 listed sessions, 1024 client addresses in the attempt
  table (5 failures per minute, then 60 s doubling to 600 s), 12 h tokens.
- **Left out**: nothing from the design. The CLI sign-in (`rustybin login <url>`) is step 10's.
  `lib.rs` sets `recursion_limit = "256"`: `Config::public_view` outgrew the `json!` macro.

Checks run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (948 tests,
with `password_sign_in_end_to_end` in `tests/integration.rs`), `docs/examples/run.sh` (every
example, the new `concepts/login.hurl` on the eighth instance), the console check
`conformance/ui/run.sh` (the new `password` section: the screen, the banner, the change, sign
out everywhere, Remember me, the pasted token; and the Sign-in tab on the open instance),
`mdbook build docs`, `check_includes.py`, `check-no-dashes.sh`.
