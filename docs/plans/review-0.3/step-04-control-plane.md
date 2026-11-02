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
