# Step 10: Single sign-on with OpenID Connect

Access control, chapter 4 of 8.

## Goal

A team signs in with the identity provider it already has (any OpenID Connect provider), and its
groups decide the role: no passwords on the instance, people added and removed in one place.

## Model

- **Settings** (group `sso`, all `settings.security`): `sso.enabled`, `sso.issuer`,
  `sso.client_id`, the secret `sso.client_secret` (or none, for a public client with PKCE),
  `sso.scopes` (default `openid profile email`), `sso.groups_claim` (default `groups`),
  `sso.role_map` (a list of `{group, roles}`; a user's roles are the union), `sso.default_roles`
  (for a member of no mapped group: empty means refused), `sso.allowed_domains` (email domains),
  `sso.provision` (create users on first sign-in, default on), `sso.password_sign_in` (default on;
  off leaves only SSO and the admin token). Environment seeds `RUSTYBIN_SSO_*`.
- **The flow.** The sign-in screen shows "Sign in with <name>"; the authorization code flow with
  PKCE and `state` and `nonce`, the redirect at `/_rustybin/sso/callback` (open, rate limited),
  discovery and the JWKS fetched under a new egress purpose `sso` (the issuer's host is permitted
  when SSO is set up, as `assist` permits its provider) and cached for an hour, the ID token
  verified (issuer, audience, expiry, nonce; RS256 and ES256), then a Rustybin sign-in token as
  for a password user. The provider's tokens are never kept.
- **Users from SSO** have `source: sso` and no password; their roles are recomputed from the groups
  at every sign-in (a group removed in the provider demotes them at the next sign-in; a session
  lasts at most 12 hours, as today); an admin may pin extra roles on them. Disabling one on the
  instance refuses their next sign-in whatever the provider says.
- **The command line.** `rustybin login --sso --url <url>` opens the browser on a loopback redirect
  (the same rules the MCP OAuth sign-in accepts) and keeps the token in the profile.
- **Agents.** The management MCP's OAuth sign-in (`control_oauth.rs`) asks the console for consent;
  with SSO the console session behind the consent is the SSO user, so an agent's token is that
  user's (step 12).

## Vendor neutrality and testing

Generic OpenID Connect only, no provider-specific code or names in the product. The tests and the
docs example use Rustybin's own demo identity provider (`src/oidc.rs`) as the issuer on a second
instance, which needs a `groups` claim option there (a small change to the demo provider, tested
on its own). `conformance/oauth` gains an SSO run in the browser.

## Surfaces

API (the `sso.*` settings through `/_rustybin/settings`, `GET /_rustybin/sso/status` for the
sign-in screen, `/_rustybin/sso/start` and `/callback`), console (Management, Sign-in: an SSO card
from the settings schema with a Test button that fetches discovery), management MCP (the settings
tools; `test_sso` reads discovery), CLI (`login --sso`, `rustybin sso test`), `rustybin.yaml` (the
`sso.*` settings; the client secret as a reference only; refused from a pulled document like the
other `settings.security` keys).

## Tests

Discovery and JWKS through the egress policy (refused host, pinning); every ID token check
(wrong issuer, audience, expired, nonce, algorithm `none`, an unknown key id refetches the JWKS
once); the role map and the default roles; provisioning off; allowed domains; password sign-in
off with the admin token still working; the CLI's loopback flow; the browser run.

## Build notes (2026-11-03)

- **Where it is.** `src/sso/`: `mod.rs` (the twelve `sso.*` settings and their `RUSTYBIN_SSO_*` seeds, read
  into `Config.sso_env` and validated with the settings' own rules, an invalid one warning and seeding
  nothing; `setup()`, `password_sign_in()`; the `Sso` state in `AppState.sso`; the role map, the person read
  out of the claims, the sign-in name, the allowed domains; the diagnostics check `sso`), `provider.rs`
  (discovery, the JWKS and the token endpoint through `egress::Egress` under the new `Purpose::Sso`, answers
  read up to 256 KiB in 10 s; `verify_id_token`), `api.rs` (the routes, catalogue, OpenAPI), `tests.rs` (a
  fake provider on a loopback port signing with the shared RS256 test key and a P-256 key made once, plus a
  second instance whose provider is Rustybin's own demo one). The user side is in `src/rbac/users.rs`
  (`User.sso` with the issuer, subject and the roles the groups gave last; `User.pinned_roles`;
  `Users::sso_sign_in`, `add_sso`; the refusals `SSO_USER`, `NOT_SSO` and the sign-in reasons), the token in
  `login::sso_session` (`rb_via: "sso"`), the tool `test_sso` in `src/mcp/manage/settings.rs`, the CLI in
  `src/cli/commands.rs`, the console in `ui/lib/auth.js`, `ui/app.js`, `ui/views/manage.js` (an SSO card on
  Management, Sign-in), `ui/views/manage/users.js` and `ui/views/account.js`.
- **The flow, two PKCEs.** `GET /_rustybin/sso/start?return_to=console|<loopback URL>&challenge=<S256>`: the
  client (the console's tab, the CLI) keeps a verifier; the instance keeps its own verifier, a `state` and a
  `nonce` for the provider (pending: 256 in all, 10 per client address bucket, ten minutes) and answers `302`
  to the authorization endpoint. `GET /_rustybin/sso/callback` takes the pending sign-in once (whatever the
  outcome), exchanges the code (HTTP Basic for a confidential client unless discovery lists only
  `client_secret_post`; `client_id` alone for a public client), verifies the ID token, maps the groups and
  links or provisions the user, then sends the browser back with a one-time code `rbs_...` (256, two
  minutes): `/ui/#/sso/<code>` for the console, `?code=` on the CLI's loopback address. `POST
  /_rustybin/sso/token` trades the code and the client's verifier for the sign-in token (the code is spent on
  the first try, right or wrong). Decision beyond the plan: the extra token route and the client's PKCE, so a
  code that leaks (address bar, history, another local process on the loopback port) is useless, and the
  provider's tokens and ours never sit in a URL.
- **Checks of the ID token**: the header's algorithm must be RS256 or ES256 (`none`, HS256 and the rest are
  refused before any key is chosen); the key by `kid` among the JWKS keys of that family (one key without
  `kid` is used when it is the only one); issuer (the discovery document's, which must equal `sso.issuer`
  trailing slash aside), audience = the client id, `azp` = the client id when present and required when
  there are several audiences, `exp` and `iat` required with 60 s of leeway, `nbf` when present, the nonce in
  constant time, a non-empty subject of at most 255 characters. An unknown `kid` fetches the JWKS once more,
  forced; still unknown is a refusal.
- **Caches and backoff.** Discovery and the keys are cached an hour per issuer and `jwks_uri`. Decision: a
  discovery failure is answered again for 10 s without fetching, since the start route is open and would
  otherwise make the instance call a provider that is down at every request.
- **Egress.** `Sso::sync_egress` permits only the issuer's host and port (no loopback of its own: a provider
  on the same machine needs `RUSTYBIN_EGRESS_ALLOW_LOOPBACK`, as the tests and examples set). A `jwks_uri` or
  token endpoint on another host needs `egress.allow`; the test checks the refusal is recorded.
- **Deviation: the role map is a list of strings.** The settings store has no structured kind and the
  console's settings form renders lists, so `sso.role_map` keeps `group=role role` entries (the last `=`
  splits, so a group name may hold one; roles separated by spaces or `+`); a `normalize` turns `{group,
  roles}` objects (the plan's shape) into that, so the API and `rustybin.yaml` take either. A group name
  with a comma is given as an object or an array item, not in the comma separated variable.
- **Added beyond the plan: `sso.name`**, the button's name ("Sign in with <name>"; empty: the issuer's host),
  since the plan's screen names the provider and nothing else says what to call it.
- **Users from single sign-on.** Identity is (issuer, subject). The first sign-in links an unlinked single
  sign-on user of the same name (one an admin added ahead, or one linked to an issuer the instance no longer
  uses), else provisions one (`sso.provision`), else `not_provisioned`. The sign-in name is
  `preferred_username`, else the email, else a cleaned-up form of either (or the subject) fitting the user
  name rules; it never changes afterwards (display name and email follow the provider at every sign-in).
  Decisions: **an identity never takes over a password account** (`name_taken`: an admin deletes the password
  user first), and another subject of the same issuer is refused a linked name (`other_identity`). Roles are
  the mapped ones (the role map's union, roles the instance has only; else the default roles) plus the
  pinned ones; none at all is `no_role`; the store's guard rail applies, so the provider cannot drop the
  last enabled admin (`last_admin`, the sign-in refused and nothing changed). A disabled user is refused
  whatever the provider says. Unknown role names in the map are ignored with a warning and listed by the test.
- **Pinned roles (how "an admin may pin extra roles").** `PATCH /_rustybin/users/{id}` takes `pinned_roles`
  for a single sign-on user (the same rules as roles: nobody changes their own, the caller grants only what
  it holds, the tool confirms a privileged one); `roles` on one is refused (`409 sso_user`); `pinned_roles` on
  a password user is refused (`409 not_sso`). `roles` is always the union, recomputed at once.
- **Step 09's leftover, done.** A single sign-on user never gets an invitation, a reset or a stored password:
  `Users::invite_new` (with `source: Sso`), `invite`, `accept_invitation`, `invitation_user`, `set_password`
  and `add` with a hash all refuse in the store, and `PUT /_rustybin/me/password` answers `409 sso_user`. No
  password signs them in (they have no hash; the check takes the same time). Added ahead:
  `POST /_rustybin/users` with `"sso": true` (no link; the roles given are pinned; at least one).
- **The password sign-in off** (`sso.password_sign_in: false`): `POST /_rustybin/login` and `POST
  /_rustybin/join` answer `403 password_sign_in_off`; the admin token is unaffected. Decision: it applies only
  while single sign-on is set up (enabled with the issuer and the client id), so a half-configured provider
  never locks everyone out; sessions signed in with a password before keep working until they end (a sign out
  everywhere ends them). The diagnostics warn when single sign-on is the only way in without
  `RUSTYBIN_ADMIN_TOKEN`.
- **Throttles.** The callback and the token route use the sign-in's `Tokens::check` under the key `(sso)`:
  an unknown state, a refused token or a wrong code is a failure (5 per address and minute, 100 from every
  address together); a provider error, `busy` and `not_configured` are not counted. The callback's answer
  with nowhere to go back to (an unknown state) is an HTML page with the message escaped and a strict CSP.
- **Surfaces.** API: the five routes (`status`, `start`, `callback`, `token` open; `test` `settings.write`,
  like `POST /_rustybin/assist/test`), all `404` outside the password sign-in, listed in `NEW_SINCE_0_4`;
  the open ones are in `sso::is_public_path` beside the sign-in's. Console: the sign-in screen's button and
  the `#/sso*` hand-off (a refusal stays on the screen with its text), Management, Sign-in, Single sign-on
  (the settings form and Test), the users table's badge, pinned roles and Invite's "Single sign-on user", My
  account without a password card for them. MCP: `test_sso` at the end of `TOOLS` (`settings.write`,
  read-only; 133 tools for an admin, 114 without the harness ones); `update_settings` now confirms a change
  of `sso.*`; `invite_user` takes `sso`, `update_user` `pinned_roles`; the start, callback and token routes
  are `Excluded` in `COVERAGE` (a browser round trip). CLI: `login --sso [--no-browser] [--wait S]` (a
  loopback listener on 127.0.0.1, the system's opener, `Env.browser` in tests), `sso status`, `sso test`
  (exit 1 when the provider cannot be read), `users invite --sso`, `users update --pin-role`; the callback is
  `UNCOVERED`. `rustybin.yaml`: the `sso.*` settings under `settings:` and the secret as a reference; `sso:
  true` on a `users:` entry (its `roles` are the pinned ones; a document never changes how an existing user
  signs in).
- **From git** (`ApplyOptions.untrusted`): `configdoc::REPOSITORY_REFUSED_PREFIXES` (`sso.`) beside
  `REPOSITORY_REFUSED`: every `sso.*` setting is reported and kept, and a prune leaves them alone. Decision:
  all of them, not only the issuer and the role map (the default roles, the groups claim, the allowed
  domains, provisioning and the password switch all decide who gets in). The client secret from git follows
  the 0.3.1 rule: only its own variable's reference.
- **The demo provider** (`src/oidc.rs`): the `groups` scope puts the user's groups in the ID token (and
  `scopes_supported` lists it), tested on its own (`the_groups_scope_puts_the_groups_in_the_id_token`).
- **Who can call what.** No existing route's reach changed (no `DELIBERATE` row). New: the four open routes,
  and `POST /_rustybin/sso/test` for `settings.write`; `pinned_roles` and `sso` on the users routes need
  `users.manage` like the rest of those routes.
- **Tests.** `src/sso/tests.rs` (18: the flow with the form, Basic auth and the PKCE pair checked; every ID
  token check, eleven cases; ES256 and the single refetch of an unknown key id; the role map, the default
  roles, a string groups claim, a nested claim, pinned roles; provisioning off, added ahead,
  `other_identity`, `name_taken`; allowed domains, an unverified email, a disabled user; the last admin; no
  password nor invitation; the password sign-in off with the admin token; the egress policy (loopback
  refused, keys on another host, a discovery for another issuer, the backoff, the token endpoint's refusal);
  the test route; the bounds and expiries on a manual clock; the return addresses and the escaped page; the
  settings' checks; the environment seeds; a document from git; the demo provider on a second instance), the
  CLI's loopback flow (`login_sso_takes_the_browser_back_on_a_loopback_address`: alice signed in, bob refused
  with exit 3), `test_sso_reads_the_providers_discovery_and_keys` (the tool, and the secret refused from
  `update_settings`), `docs/examples/concepts/sso.hurl` on a ninth instance of `docs/examples/run.sh`, and
  `run_sso` in `conformance/ui/console_e2e.py` (a sixth instance of `conformance/ui/run.sh` whose provider is
  the first instance's demo one: the button, the provider's page, back signed in; the settings form, Test,
  the users table; bob refused with the reason). Deviation: the plan's `conformance/oauth` SSO run is this
  browser run instead (`conformance/oauth` uses fixed ports).
- **For steps 11 to 14**: step 12's agents act for the console session's user, which is the single sign-on
  user after an SSO sign-in (nothing to do here); step 13's portal sign-in could offer the same button
  (`Sso::begin` with another `ReturnTo`); step 14's audit log should take the `rustybin::auth` lines `sso`
  and `sso_token` (user, address, outcome; never a token).
