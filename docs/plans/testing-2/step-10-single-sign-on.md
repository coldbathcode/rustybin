# Step 10: Single sign-on with OpenID Connect

Access control, chapter 4 of 7.

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
