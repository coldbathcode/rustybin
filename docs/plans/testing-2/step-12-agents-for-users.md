# Step 12: Agents act for a user

Access control, chapter 6 of 8.

## Goal

Every AI agent on the management MCP server works on someone's behalf and can never do more than
that person: the AI agents view says whose agent it is, and removing a person removes their
agents.

## Model

- `agents::Credential` gains `User { id }`: a sign-in token (password or SSO) and a token of the
  built-in OAuth server whose consent a user gave. An agent's permissions are the user's (at every
  call, step 07), intersected with the agent's own limits (the ceiling, the tool allow and deny
  lists, the `agents.default_ceiling` setting, the `agents:` rules of `rustybin.yaml`).
- **Ceilings become roles.** An agent's ceiling may name any role (built-in or custom); the four
  0.3 ceilings map to roles (`read_only` to `viewer`, `traffic` to `viewer` plus `traffic.read`,
  `manage` to `editor`, `full` to the user's own). `agents.default_ceiling` per role is a setting
  (`agents.default_ceiling_for`), so an admin's agents can default to `editor`.
- **Consent shows what it grants.** The OAuth consent page lists the permissions the agent will
  hold (the user's, capped by the requested scope), never more than the approving user's.
- **Lifecycle.** Disabling or deleting a user revokes their agents and ends their MCP sessions;
  `users.manage` callers see every user's agents, others only their own; a user may limit and
  revoke their own agents (`agents.manage` is only for other people's).
- **The admin token and JWTs** stay credentials of their own (no user), shown as such.

## Surfaces

The AI agents view gets an Owner column and a filter; `list_agents` and `rustybin agents list`
answer the owner; `set_agent_rights` takes a role as the ceiling; the consent page's permission
list; `rustybin.yaml`'s `agents:` rules may match a user or a role.

## Tests

An agent never exceeds its user (a table across roles and ceilings); a role change of the user
applies to the agent's next call; disabling the user revokes the agent and ends its session;
own-agent management without `agents.manage`; the consent page caps; the 0.3 ceilings map as
above.

## Build notes (2026-11-03)

- **Where it is.** The model in `src/mcp/manage/agents/mod.rs` (`Credential::User { id, client }`,
  `Ceiling::Role`, `Policy` with the user store, `default_for`, `owner_view` / `owner_label` /
  `owner_allows`, `Match.user` / `Match.role`, `visible_to`, `may_read`, `may_manage`, `Action`,
  `check_rights`, `ceiling_roles`, `revoke_owned`, `end_owned_sessions`, `adopt_users`), the routes in
  `agents/api.rs`, the tools in `agents/tools.rs`, the credential in `src/mcp/manage/access.rs` (from
  the resolved caller; `ToolNeed::Own`), the user-bound OAuth tokens in `src/control_oauth.rs`
  (`token_user`, `resolve_token`, `may_grant`, the consent answer's `grants`, `permissions` and
  `approver`), the hooks in `src/rbac/service.rs` (`ended_user` ends the agents' MCP sessions,
  `removed_user` revokes them), the narrowed password change in `src/settings.rs`
  (`after_sign_in_change`) and `src/login.rs` (`first_admin_password`). Tests:
  `src/mcp/manage/agents/user_tests.rs` (the table across five roles and seven ceilings, a role
  change, disable and delete, own agents, role ceilings and the defaults per role, rules by user and
  role, a pulled document, the 0.4 records), `src/control_oauth/tests/users.rs` (the consent caps,
  refresh, role change and disable of an OAuth agent, the compromised first admin),
  `docs/examples/agent/agents_for_users.hurl` (the password instance), `run_agents_for_users` in
  `conformance/ui/console_e2e.py`.
- **`Credential::User { id, client }`** covers both kinds the plan names: `client` empty for a
  sign-in token, the OAuth client's subject (`client:<fingerprint>`) for a token a user approved. They
  stay separate credentials (fingerprints `user:<id>` and `user:<id>:oauth:<sub>`), so revoking a
  user's OAuth client never signs their console out, and revoking their sign-in agent works as the
  0.4 console agent did. `kind` is `user` with `via` (`sign-in` or `oauth`); a rule's
  `match.credential` takes `user` (any), `console` (sign-in) and `oauth` (a user's or not), so 0.4
  rules keep matching. `Credential::Console { user }` stays readable for old `agents.json` records.
- **0.4 records move at startup** (`Agents::adopt_users`, from `AppState`, password sign-in only, once
  for a file of version 1; the file is written as version 2 since): a console record to the user of
  that name, an OAuth record to the first admin (0.4 consents were the console user's), re-keyed with
  their rights, notes, revocations and totals. An OAuth record written since (approved by the admin
  token) is never moved.
- **OAuth tokens carry the user**: `rb_uid` (the approving user's id, or `null` when a credential that
  is no user approved) and `rb_ugen` (the user's generation). Verification refuses a token whose user
  is gone, disabled or of another generation, access and refresh tokens alike. A token without
  `rb_uid` (issued before this step) is the first admin's with the password sign-in (`rb_ugen` absent
  counts as generation 1, as for sign-in tokens), nobody's elsewhere. The token's caller is the user
  (`Principal::User`) with the user's permissions intersected with the scope's, resolved at every call.
- **Decision: what a user may grant.** The plan says the consent lists "the user's, capped by the
  requested scope". A user grants a scope that gives at least one permission they hold (an editor may
  grant `admin`: their agent then holds the editor's permissions); a credential that is no user is
  capped by its grant as in 0.4. The consent answer adds `approver`, `acts_for_user`, `grants` (per
  grantable scope, the permissions it gives) and `permissions` (for every grantable scope together).
- **Deviation: the 0.3 ceilings keep their tools.** The plan maps `read_only` to `viewer`, `traffic`
  to `viewer` plus `traffic.read`, `manage` to `editor`. As permission sets they would change what
  stored ceilings allow (`read-only` lets through read-only tools needing `assist.use`, `mcp.inspect`
  or `users.read`, which the viewer lacks; no permission set reproduces `read-only`, since a read-only
  tool and a write tool share permissions), and `ceilings_keep_their_tool_counts` guards that. So the
  four names keep their 0.4 rule, the nearest role is shown beside each (`ceiling_role` in the rights
  view, the console's select), and any role is a ceiling too (`Ceiling::Role`: a tool whose
  permission the role grants; an unknown role allows nothing). A 0.3 name wins over a custom role of
  the same name. `full` is the user's own.
- **`agents.default_ceiling_for`** is a list setting of `role=ceiling` entries (the first whose role
  the user holds applies; checked at save), between the rules and `agents.default_ceiling`; the
  effective rights say `source: role`. No environment variable (the plan names none). The global
  `agents.default_ceiling` stays one of the four 0.3 ceilings (a `Choice`, and
  `RUSTYBIN_AGENTS_DEFAULT_CEILING`); `rustybin.yaml`'s `default_ceiling` and rule ceilings may name a
  role (an unknown one is reported in the apply's `problems`).
- **Decision: who sees everyone's agents.** The plan says `users.manage`; the permission table has
  `agents.read` for it (the viewer and the `console` scope already read the agents since step 08), so
  everyone's agents need `agents.read` and changing them `agents.manage`; a user without them sees
  their own (`GET /_rustybin/agents` answers `owner` forced to them, `?owner=` of someone else is
  `403 agents.read`, the live feed is filtered to their agents). With the built-in roles only a
  `portal_reader` or a custom role sees only their own.
- **Own agents** (`may_manage`): limit (the change must not let a tool through that the agent's
  rights refuse now, so a reset back to wider rules is `403 agents.manage`), revoke, end sessions,
  and restore one they revoked themselves (`Revocation.by_user`; one the operator revoked, or one
  revoked by the user's disable, needs `agents.manage`). The management tools' existing rule stays: an
  agent never changes the rights of, or restores, an agent of its own credential.
- **Who can call what.** The seven `/_rustybin/agents*` routes and methods are `.authenticated()`
  (the handlers check `agents.read` / `agents.manage` for everyone but a user's own: two new
  `BODY_CHECKS` rows); 25 `DELIBERATE` rows in `src/rbac/access_table.rs` (the `inspector`,
  `harness` and anonymous columns reach every agents route, the `console` column the four changes).
  Those callers are no user, so the handlers still need the permissions and what they can do is
  unchanged. A user without `agents.manage` asking for an unknown agent gets `404` (it could be
  theirs). The six agents tools take `ToolNeed::Own(p)` (listed for any user; the ceilings read `p`
  as before, so `ceilings_keep_their_tool_counts` holds). No new route and no new tool: 132 for an
  admin, unchanged.
- **Lifecycle.** Disabling or deleting a user (`rbac::service::removed_user`, every path through the
  service: REST, tools, documents) revokes every agent acting for them (`revoked.by` says "user
  removed") and ends their sessions; the answers carry `agents_revoked`. Enabling the user again
  leaves the agents revoked. A sign out everywhere and a password change of a user end their agents'
  MCP sessions (`ended_user`) without revoking them; their tokens die with the generation.
- **The first admin's password change is narrowed** (step 09's ask): `Settings::after_sign_in_change`
  moves the first admin's generation instead of the instance's (and forgets their listed sessions),
  so their sign-in, portal and agents' tokens end, 0.4 OAuth tokens included, and other users stay
  signed in. `a_compromised_first_admins_agent_token_dies_on_the_password_change` proves the agent
  token, its refresh token and a token without `rb_uid` die, a viewer stays in, and an OAuth agent
  approved with the admin token outlives it until `DELETE /_rustybin/sessions`. Without a first
  admin, or when the user store cannot write, every session ends as before.
- **Answers.** Each agent: `owner` (`user_id`, `name`, `display_name`, `roles`, `status`: active,
  disabled, invited or deleted; `null` for a credential that acts for nobody), `owner_label`,
  `credential.via`, `rights.ceiling_kind` / `ceiling_role` / `ceiling_known`, `revoked.by_owner`;
  `available_tools[].allowed` and `tools_allowed` count only what the user may also do
  (`rights_allow` is the rights alone). The list adds `owner`, `you` and `roles` (the role names a
  ceiling may take: custom ones with `users.read` or `agents.manage`). `get_instance`'s caller adds
  `user` and `via`.
- **Documents.** A pulled document's `agents:` section applies as before: whatever it says, an agent
  acting for a user never holds more than the user (a ceiling only takes away;
  `a_pulled_document_cannot_raise_an_agent_above_its_user`).
- **Console.** AI agents: the Owner column (`[data-col=owner]`, a `you` badge, the user's status),
  the owner filter (`#agents-owner-filter`: everyone, mine, each owner, no user), "Acts for" in the
  detail, the ceiling select with the 0.3 ceilings and their nearest roles plus the roles, the
  rights' source `role default`, the defaults per role in the policy card, and a user's own agents
  keep their controls without `agents.manage` (the server decides). The consent page lists the
  permissions the agent will hold for the scope chosen (`#authorize-permissions`).
- **Left for later.** An agent of a user who is disabled stays revoked when the user is enabled
  again (the plan is silent; restoring is an operator act). Step 14's audit log is the place for who
  revoked whose agent. Projects (step 11) will need the ceilings and `may_manage` to read project
  roles.
