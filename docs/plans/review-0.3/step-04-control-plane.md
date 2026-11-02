# Step 04: Control plane: CORS, sign-in, keys, agents

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | high | The default CORS (`*`, methods and headers mirrored) covers `/_rustybin/login`: any web page the operator opens can sign in with the default password and read the admin token | `src/lib.rs:357`, `src/config.rs:571`, `src/login.rs:942` | no cross-origin answers on `/_rustybin/*` unless origins are listed (decision in the master plan); the data plane keeps `*`; the configuration page says so |
| 2 | medium | The sign-in throttle is per exact address: IPv6 rotation or a spoofed forward header bypasses it | `src/login.rs:636` | key IPv6 on the /64; a global failure budget |
| 3 | medium | The OAuth and console signing key is one SHA-256 of the admin token, and anonymous client registration returns a MAC under it: a short admin token can be cracked offline | `src/control_oauth.rs:118`, `:478`, `:803` | a random per-instance key in the state directory (decision in the master plan); a minimum admin token length |
| 4 | medium | `PATCH /_rustybin/settings` sets `console.password_hash` (and `DELETE` unsets it) without the current password and without signing sessions out | `src/settings.rs:1068`, `:1194` | the current password for those keys, or only through `/password`; rotate the generation |
| 5 | medium (verify first) | Behind a proxy without `RUSTYBIN_TRUST_FORWARD`, one client locks every user out of sign-in and shares all limits | `src/session.rs:214`, `src/login.rs:965`, `src/limits.rs:1261` | a startup warning; block by user name and address |
| 6 | medium | An agent can lift its own deny list or ceiling with `set_agent_rights` or `restore_agent` on its own id | `src/mcp/manage/agents/tools.rs:112`, `:229` | refuse on the caller's own agent and credential fingerprint |
| 7 | medium (verify first) | `update_settings` changes `egress.*`, `agents.*`, `console.*` and `git.*` without confirmation | `src/mcp/manage/settings.rs:98` | confirm when those groups change |
| 8 | medium (verify first) | The assist rate limit keys token callers on the caller-chosen `X-Rustybin-Session` | `src/mcp/manage/access.rs:283`, `src/assist/mod.rs:737` | the credential fingerprint as the principal |
| 9 | low (verify first) | `sessions.json` saves race and can lose a revocation | `src/login.rs:457` | one writer, the snapshot under it |
| 10 | low | The console scope's "read-only" relay and checks POSTs send `tools/call` to external MCP servers | `src/control_auth.rs:212` | admin for `tools/call` through the relay and checks, or list methods only |
| 11 | low | Log forging through the sign-in user name and the management audit target | `src/login.rs:903`, `src/mcp/manage/tools.rs:1121` | escape control characters |
| 12 | low | `168.63.129.16` (Azure WireServer) is not a forbidden egress address | `src/egress.rs:433` | add it |
| 13 | low | A few addresses fill the pending OAuth consents (and the per-address tracker) | `src/control_oauth.rs:634`, `:1001` | a global authorize rate; evict per client id |

## Build notes (2026-11-02)

Every finding is fixed. Of the "verify first" findings, 5, 7 and 8 reproduced (their tests
failed on the old code); 9 did not (see below) and was fixed anyway. Tests are in the module of
each fix; docs in `docs/src/configuration.md`, `docs/src/concepts/control-plane.md`,
`docs/src/concepts/control-plane-security.md`, `docs/src/agent-mcp.md` and
`docs/src/mcp/inspector.md`, with the Hurl examples `concepts/control.hurl`
(`cors_control_plane`) and `concepts/login.hurl` (`reset_needs_password`).

1. **CORS on the control plane.** A middleware outside the CORS layer (`control_cors` in
   `src/lib.rs`) drops every `Access-Control-*` header of a `/_rustybin/*` answer unless the
   request's `Origin` is named in `RUSTYBIN_CORS_ORIGINS` (`*` never counts there). So a page
   on another origin cannot read `POST /_rustybin/login` (the default password's token) and its
   preflights fail; same-origin console calls need no CORS. The data plane keeps its answer
   (`*` by default). Decision on the OAuth endpoints: `/_rustybin/oauth/{register,authorize,token}`
   follow the control-plane rule too, because a browser MCP client that finishes the OAuth flow
   still needs CORS on `/_rustybin/mcp` (also control plane) to use its token: exempting only the
   OAuth endpoints would buy nothing, so such a client lists its origin. The discovery documents
   under `/.well-known/` (public metadata) keep the data plane's policy. The management MCP
   server's `Origin` check is unchanged (listed origins, else any). Test:
   `control_auth::tests::the_control_plane_answers_cross_origin_only_to_listed_origins`.
2. **Sign-in throttle.** Attempts are keyed by the address bucket (IPv4 as is, IPv6 by its /64,
   `session::address_bucket`) and the user name tried (lower case), and a global budget of 100
   failures per minute from every address makes every sign-in wait for the minute's end.
   Tests: `an_ipv6_host_rotating_in_its_slash_64_is_one_address`,
   `failures_from_every_address_share_a_budget`.
3. **The signing key.** `OAuthKey::from_settings(explicit, state_dir)`:
   `RUSTYBIN_CONTROL_OAUTH_KEY`, else 32 random bytes in `<state>/control-oauth.key` (0600,
   through `settings::load_or_create_key_file`), else random per process. The admin token is
   no longer an input (`KeySource::AdminToken` is gone, `StateFile` is new). An admin token
   under 24 characters (`config::MIN_ADMIN_TOKEN_CHARS`) warns at startup; it never refuses.
   Tests: `the_key_is_random_per_instance_never_the_admin_token`, `a_short_admin_token_warns`.
4. **The sign-in through the settings.** The rule sits in the store, so every path meets it:
   in password mode `Settings::update` and `unset` refuse a change of `console.user` (when the
   value in force changes) or `console.password_hash` (set, or a stored one cleared);
   `update_sign_in` / `unset_sign_in` are the paths that may. `PUT /_rustybin/password` uses
   `update_sign_in`; `PATCH /_rustybin/settings` does when the body carries `current_password`
   (checked and throttled like a sign-in; `403 current_password_required` or
   `invalid_credentials`) or the caller presents `RUSTYBIN_ADMIN_TOKEN` itself;
   `DELETE /_rustybin/settings/{key}` resets the sign-in only with the admin token (the
   documented recovery of a forgotten password). Any change of the user name or the hash
   rotates the token generation from the store (the password route counts the sessions first,
   so `signed_out` stays right). The management MCP and documents (`rustybin.yaml`) can no
   longer change the sign-in in password mode; outside password mode nothing is guarded.
   Tests: `the_sign_in_changes_through_the_settings_only_with_the_current_password`,
   `the_admin_token_resets_a_forgotten_password`,
   `a_plain_settings_update_never_changes_the_sign_in_in_password_mode`.
5. **One client behind a proxy (verified: it reproduced).** Five failures under another user
   name from the shared address locked every user out. With finding 2's key the real user name
   is unaffected; the instance warns once when a sign-in carries a forwarding header it does
   not trust, and a public instance without `RUSTYBIN_TRUST_FORWARD` warns at startup
   (`login::warn_untrusted_forwarding`). Test:
   `one_client_trying_other_names_does_not_lock_the_user_out`.
6. **Agents lifting their own limits.** `set_agent_rights`, `restore_agent` and
   `end_agent_sessions` refuse the caller's own agent and any agent whose credential has the
   caller's fingerprint (`Agents::credential_of`, `Credential::fingerprint` now `pub(crate)`).
   The REST routes (the operator) are unchanged; `revoke_agent` keeps its own rule. Tests:
   `agents::tests::an_agent_cannot_lift_its_own_limits`, `tests::agent_tools` (reworked).
7. **`update_settings` confirmation (verified: it reproduced).** A change (set or unset) of a
   key under `egress.`, `agents.`, `console.` or `git.` goes through `confirm`; the ToolDef has
   `confirm: true` and a `confirm` property. Test: `settings_and_egress_tools`.
8. **Assist principal (verified: it reproduced).** `Access::principal` names a token caller by
   its credential fingerprint (`jwt:<sub>`, `oauth:<sub>`, `console:<user>`), never
   `session:<X-Rustybin-Session>`; the assist caps and the audit lines follow. Open callers keep
   the session label. Test: `access::tests::a_token_caller_is_its_credential_whatever_the_session_header`.
9. **`sessions.json` saves (verified: did not reproduce).** The test
   (`concurrent_revocations_all_reach_the_file`, 32 threads) passed on the old code: the window
   between the snapshot and the rename is too narrow to hit. The race is real on paper (an
   older snapshot renamed last), so `Tokens::save` now takes the snapshot and writes under one
   writer lock; the test stays as the regression check.
10. **Console scope and `tools/call`.** Without admin rights a server on another host only
   gets read-only methods (`inspector::READ_ONLY_METHODS`: initialize, server/discover, ping,
   the lists, resources/read, prompts/get) and notifications: the relay answers
   `403 admin_required`, a check run skips `tools_call` and `bad_arguments`
   (`RunSpec.call_tools`), and the `mcp_call` tool refuses. This instance's own servers take
   any method. Tests: `inspector::tests::the_console_scope_sends_only_list_methods_to_another_host`,
   `tests::the_new_tools_follow_the_control_scopes` (an `mcp_call` assertion).
11. **Log forging.** `logging::printable` escapes control characters; the sign-in audit's user
   name and the management audit's target use it.
12. **`168.63.129.16`** is a forbidden egress address (the cloud metadata reason), IPv4-mapped
   too.
13. **OAuth consents.** Sign-in requests are limited per address bucket (an IPv6 /64) and for
   every address together (the pending store's capacity over its 10 minute life: 100 a
   minute, 20 in public mode), and one client id keeps at most 3 pending requests (a new one
   evicts its oldest). Tests: `sign_in_requests_have_a_global_rate_and_ipv6_counts_by_slash_64`,
   `a_client_keeps_a_few_pending_sign_ins`.

Docs examples changed: `docs/examples/concepts/control.hurl` (new anchor
`cors_control_plane`), `docs/examples/concepts/login.hurl` (the cleanup no longer deletes the
hash with a sign-in token: the refusal, then a PATCH with `current_password`, anchor
`reset_needs_password`). The other examples that call the changed routes (`concepts/settings.hurl`,
`mcp/inspector.hurl`, `assist/assist.hurl`, `portal/portal.hurl`, `mcp/registry.hurl`,
`agent/manage_build.hurl`) run on the open instance or send read-only methods, so they are unchanged.

### For the changelog

- The control plane (`/_rustybin/*`, the sign-in and the OAuth endpoints included) answers
  cross-origin only to origins named in `RUSTYBIN_CORS_ORIGINS`; `*` covers the data plane
  only. A browser app on another origin that calls the control API lists its origin.
- The console and OAuth signing key is a random per-instance key in
  `<state>/control-oauth.key` (or per process without a state directory) unless
  `RUSTYBIN_CONTROL_OAUTH_KEY` is set; it is never derived from the admin token. Sign-in and
  OAuth tokens issued before the upgrade stop working once (sign in again, agents too);
  replicas that relied on a shared admin token to share sessions set
  `RUSTYBIN_CONTROL_OAUTH_KEY` or share the state directory.
- An admin token shorter than 24 characters warns at startup.
- In password mode `console.user` and `console.password_hash` change only with the current
  password (`PUT /_rustybin/password`, or `current_password` in `PATCH /_rustybin/settings`)
  or with the admin token itself; the management MCP and `rustybin.yaml` no longer change
  them. Every change of them signs every session out.
- The sign-in throttle counts per address (an IPv6 /64) and user name, with a budget of 100
  failures a minute from every address together; a warning names a proxy that is not trusted.
- Agents cannot change rights, restore or end the sessions of their own agent or of an agent
  of their own credential.
- `update_settings` asks the user to confirm changes to `egress.*`, `agents.*`, `console.*`
  and `git.*`.
- AI assist caps of token callers follow the credential, not `X-Rustybin-Session`.
- Without admin rights the MCP relay, the check runs and `mcp_call` send only read-only
  methods (no `tools/call`) to servers on other hosts.
- `168.63.129.16` is never an egress target.
- OAuth sign-in requests have a global rate, and a client keeps at most 3 pending consents.
