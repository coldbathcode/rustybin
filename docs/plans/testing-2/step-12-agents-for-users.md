# Step 12: Agents act for a user

Access control, chapter 6 of 7.

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
