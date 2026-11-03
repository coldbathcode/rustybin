# Single sign-on

A team signs in to the console and the control plane with the identity provider
it already has, any OpenID Connect provider, and the provider's **groups decide
the roles**. Nobody has a password on the instance; people are added and removed
in one place, the provider. Single sign-on works with the password sign-in
(`RUSTYBIN_CONTROL_AUTH=password`, the default), where [users](users.md) live; the
admin token stays the break glass.

## Set it up

1. At the provider, create an OpenID Connect client with the authorization code
   flow (a confidential client with a secret, or a public one: the instance
   always uses PKCE) and register `https://<instance>/_rustybin/sso/callback` as
   its redirect URI. Ask for the groups in the ID token: most providers need a
   scope (often `groups`) or a mapper for it.
2. On the instance, set the `sso.*` settings under **Management, Sign-in**,
   **Single sign-on** (they need `settings.security`; the secret also
   `secrets.write`), or from the environment:

| Setting | Variable | Default | |
|---|---|---|---|
| `sso.enabled` | `RUSTYBIN_SSO_ENABLED` | `false` | The "Sign in with" button on the sign-in screen |
| `sso.name` | `RUSTYBIN_SSO_NAME` | the issuer's host | The button says "Sign in with `<name>`" |
| `sso.issuer` | `RUSTYBIN_SSO_ISSUER` | | The issuer URL, exactly as the provider's discovery document says it (https; http only on a loopback address) |
| `sso.client_id` | `RUSTYBIN_SSO_CLIENT_ID` | | The client at the provider |
| `sso.client_secret` (secret) | `RUSTYBIN_SSO_CLIENT_SECRET` | not set | Not set: a public client (PKCE only) |
| `sso.scopes` | `RUSTYBIN_SSO_SCOPES` | `openid profile email` | `openid` is always added |
| `sso.groups_claim` | `RUSTYBIN_SSO_GROUPS_CLAIM` | `groups` | The claim with the groups; a dotted path reads a nested one (`realm_access.roles`) |
| `sso.role_map` | `RUSTYBIN_SSO_ROLE_MAP` | empty | `group=role` entries, several roles separated by spaces (`developers=editor tester`); a person's roles are the union of their groups' |
| `sso.default_roles` | `RUSTYBIN_SSO_DEFAULT_ROLES` | empty | The roles of a person in no mapped group; empty refuses them |
| `sso.allowed_domains` | `RUSTYBIN_SSO_ALLOWED_DOMAINS` | empty | Only emails at these domains sign in (an email the provider says is unverified is refused); empty: any |
| `sso.provision` | `RUSTYBIN_SSO_PROVISION` | `true` | A person becomes a user at their first sign-in; off: only users an admin added |
| `sso.password_sign_in` | `RUSTYBIN_SSO_PASSWORD_SIGN_IN` | `true` | Off: only single sign-on and the admin token sign in (no password sign-in, no invitations) |

3. **Test** (or `rustybin sso test`, the `test_sso` tool) fetches the provider's
   discovery document and keys now and says the redirect URI to register:

```hurl
{{#include ../examples/concepts/sso.hurl:test}}
```

The role map takes `{group, roles}` objects too (an API call, `rustybin.yaml`); it
keeps them as `group=role role` strings. A group name with a comma (an LDAP DN) is
given that way rather than in the comma separated variable:

```hurl
{{#include ../examples/concepts/sso.hurl:role_map}}
```

The examples on this page use Rustybin's own [demo identity provider](reference/oidc.md)
as the provider of a second instance (`alice` is in the groups `users` and
`admins`, `bob` in `users`; its `groups` scope puts the groups in the ID token).

## The sign-in

The sign-in screen asks `GET /_rustybin/sso/status` (open, never a secret) and
shows **Sign in with `<name>`**; with the password sign-in off, the password form
is gone:

```hurl
{{#include ../examples/concepts/sso.hurl:status}}
```

The button opens `GET /_rustybin/sso/start` with the S256 challenge of a verifier
the console keeps in the tab. The instance sends the browser to the provider: the
authorization code flow with its own PKCE verifier, a `state` and a `nonce`:

```hurl
{{#include ../examples/concepts/sso.hurl:start}}
```

The person signs in at the provider, which sends the browser to the callback:

```hurl
{{#include ../examples/concepts/sso.hurl:provider}}
```

`GET /_rustybin/sso/callback` exchanges the code at the token endpoint, verifies
the ID token, maps the groups to roles, links or creates the user, and sends the
browser back to the console with a one-time code (two minutes):

```hurl
{{#include ../examples/concepts/sso.hurl:callback}}
```

The console trades the code and its verifier for a sign-in token, the same 12
hour token a password user gets (the session says `obtained: "sso"`):

```hurl
{{#include ../examples/concepts/sso.hurl:token}}
```

```hurl
{{#include ../examples/concepts/sso.hurl:session}}
```

What the instance checks of the ID token: the algorithm (RS256 or ES256; never
`none` nor a shared secret), the signature with the provider's keys, the issuer,
the audience (the client id; `azp` when it is there or when there are several
audiences), the expiry and `iat` (a minute of leeway), and the nonce of this
sign-in. The provider's tokens (access, refresh, ID) are never kept.

A refusal goes back to the sign-in screen with its reason: `expired` (the sign-in
took over ten minutes), `denied` (the provider refused), `provider_error`,
`invalid_id_token`, `domain_not_allowed`, `email_unverified`, `not_provisioned`,
`name_taken`, `other_identity`, `disabled`, `no_role`, `last_admin`,
`rate_limited`.

## Users from single sign-on

A person the provider signs in becomes a user with `source: sso` and no password
(with `sso.provision` on; off, an admin adds them first, below). Their sign-in
name is the ID token's `preferred_username`, else the email, made to fit the
[user name rules](users.md). From then on the provider's subject is their identity:
a later sign-in finds them whatever name the provider gives.

- **Roles follow the groups at every sign-in**: a group removed at the provider
  demotes them at their next sign-in, and a session lasts at most 12 hours. Signing
  them out everywhere (`rustybin users sessions <user> --end`) applies it at once.
- **An admin pins roles** on top of the groups' (`pinned_roles`); a change of their
  `roles` is refused (`sso_user`: the provider gives those):

```hurl
{{#include ../examples/concepts/sso.hurl:pin}}
```

- **No password, no invitation**: an invitation, a password reset and a password
  change are refused (`sso_user`), and no password signs them in:

```hurl
{{#include ../examples/concepts/sso.hurl:user}}
```

- **Disabled on the instance** refuses their next sign-in, whatever the provider
  says. The guard rails of [users](users.md) apply: the provider cannot drop the
  last enabled admin (the sign-in is refused with `last_admin`).
- **An identity never takes over a password account**: a provider user whose name
  is a password user's is refused (`name_taken`), and another subject of the same
  provider with a linked user's name too (`other_identity`).
- **Added ahead** (`sso.provision` off): `POST /_rustybin/users` with `"sso": true`
  (`rustybin users invite <name> --sso --role viewer`, the `invite_user` tool with
  `sso: true`, the console's Invite with **Single sign-on user**) creates the user
  with no link; the roles given are pinned; their first sign-in links them by name.

## Only single sign-on

`sso.password_sign_in` off leaves single sign-on and the admin token: `POST
/_rustybin/login` and `POST /_rustybin/join` answer `403 password_sign_in_off`. It
applies only while single sign-on is set up (enabled, with the issuer and the
client id), so a half-configured provider never locks everyone out; a session
signed in with a password before keeps working until it ends. Keep
`RUSTYBIN_ADMIN_TOKEN` set as the break glass (the diagnostics warn without one):

```hurl
{{#include ../examples/concepts/sso.hurl:password_off}}
```

## The command line and agents

`rustybin login --sso --url https://<instance>` listens on a loopback address of
this machine, opens the browser on the instance's start (`return_to` is that
loopback address, the rule the management server's OAuth sign-in applies to
native clients), takes the one-time code back and keeps the token in the profile
(`--no-browser` prints the address instead; `--wait` seconds, default 300):

```bash
rustybin login --sso --url https://mocks.example.com
rustybin whoami        # obtained: sso
rustybin sso status
rustybin sso test      # exits 1 when the provider cannot be read
```

An agent connecting to the [management MCP server](agent-mcp.md) with OAuth asks
the console for consent: the console session behind the consent is the single
sign-on user's.

## Security

- **The network**: discovery, the keys and the token endpoint go out through the
  [outbound policy](configuration.md#outbound-connections) under
  the purpose `sso`, whose rule permits the issuer's host once single sign-on is
  set up (keys or a token endpoint on another host need `egress.allow`). Each
  answer is read up to 256 KiB, in 10 seconds. Discovery and the keys are cached
  for an hour; a key id the cache does not know fetches the keys once more.
- **The flow**: `state` (32 random bytes, one use), the `nonce` checked in the ID
  token, PKCE between the instance and the provider, and a second PKCE between the
  instance and the console (or the CLI): the one-time code works only with the
  verifier whose challenge started the sign-in, so a code that leaks from the
  address bar is useless.
- **Bounds**: at most 256 sign-ins under way (10 per client address), each for ten
  minutes; at most 256 one-time codes, each for two minutes. The callback and the
  code exchange are throttled like the password sign-in (5 failures per client
  address and minute, 100 from every address together).
- **The client secret** is a secret setting: sealed at rest, shown as `{set,
  hint}`, never a tool input nor returned.
- **From git**: a document [from the repository](source-control.md) never changes
  an `sso.*` setting (a pusher would repoint the issuer or map their group to
  `admin`); such an entry is reported and kept as it is.

## On every surface

| Surface | Where |
|---|---|
| API | The `sso.*` settings through `GET`, `PATCH /_rustybin/settings`; `GET /_rustybin/sso/status`, `GET /_rustybin/sso/start`, `GET /_rustybin/sso/callback`, `POST /_rustybin/sso/token` (open); `POST /_rustybin/sso/test` (`settings.write`); `"sso": true` on `POST /_rustybin/users`, `pinned_roles` on `PATCH /_rustybin/users/{id}` |
| Console | The sign-in screen's **Sign in with**; **Management, Sign-in, Single sign-on** (a form from the settings schema and **Test**); **Management, Users and roles** (the single sign-on badge, pinned roles, Invite's **Single sign-on user**) |
| CLI | `rustybin login --sso`; `rustybin sso status`, `rustybin sso test`; `rustybin users invite --sso`, `rustybin users update --pin-role` ([reference](cli.md#sso)) |
| Management MCP | `test_sso`; the `sso.*` settings with `update_settings` (confirmed; never the client secret); `invite_user` with `sso`, `update_user` with `pinned_roles` ([agent page](agent-mcp.md#agents-users-and-sign-in-sessions)) |
| `rustybin.yaml` | The `sso.*` settings under `settings:`, the client secret under `secrets:` as a reference only (`${RUSTYBIN_SSO_CLIENT_SECRET}`); `sso: true` on a `users:` entry (its `roles` are the pinned ones). Refused from a document pulled from git ([instance as code](configuration-as-code.md)) |
