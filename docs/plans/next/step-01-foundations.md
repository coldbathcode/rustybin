# Step 01: Foundations: settings store, secrets, egress policy, Management area

Master plan: [master-plan.md](master-plan.md). Status: **done 2026-10-29**. Needs: nothing. About 2 days.

## Goal

The pieces every later step uses: a persisted, encrypted instance settings store with an API; one
egress policy module for outbound calls; a Management area in the console that replaces the
settings dialog; the `RUSTYBIN_STATE_DIR` umbrella. Nothing user-visible changes in behaviour yet
except the new Management view and the settings route.

## Read first

- `src/config.rs` (`Config`, `parse_or`, the `get` closure, warnings on invalid values), `src/state.rs`
  (`AppState`, `FromRef`), `src/lib.rs` (`ROUTERS`, `build_app`).
- `src/harness/safety.rs`: the allowlist, forbidden addresses, pinning and verification the egress
  policy generalises. `src/a2a/` push allowlist, `src/mocks/leads.rs` webhook, `src/keys/registry.rs`
  URL loading: the three other places that reach out today.
- `src/mocks/persist.rs` and `src/harness/saved.rs`: how state is written today (atomic rename,
  bounded, loaded at startup).
- `src/control.rs` (`/_rustybin/config`, `/_rustybin/status`), `src/admin.rs`.
- `ui/app.js` (the settings dialog and `VIEWS`), `ui/lib/settings.js` (browser-side settings),
  `ui/lib/auth.js`, `ui/views/overview.js` (configuration card).
- `docs/src/configuration.md`, `docs/src/console.md`, CLAUDE.md "Adding a module".

## Design

### `RUSTYBIN_STATE_DIR`

`Config.state_dir: Option<PathBuf>`. When set: created 0700 if missing; `mocks_state_dir` defaults
to `<dir>/mocks` and `harness_state_dir` to `<dir>/harness` unless their own variables are set;
`<dir>/settings.json`, `<dir>/secrets.key` are this step's. A startup line says the directory and
what lives in it. Unset keeps today's behaviour (memory only) and the settings API answers with
`"persisted": false` so the console can say changes last until restart.

### The settings store (`src/settings.rs`)

```rust
pub struct Settings { inner: RwLock<Doc>, path: Option<PathBuf>, secrets: Secrets, locked: bool }
pub struct Doc { version: u32, values: BTreeMap<String, Value>, secrets: BTreeMap<String, Sealed>, updated_at: String }
```

- Keys are dotted names owned by features: `console.title`, `assist.provider`, `assist.model`,
  `portal.title`, `git.remote`, `agents.<id>.rights`. A feature registers its keys with a schema
  (type, default, whether it is a secret, whether env seeds it and from which variable, whether it
  is locked by `RUSTYBIN_SETTINGS_LOCK`) in a `SettingsSpec` table in its module; `settings::schema()`
  joins them for the API and the console form.
- Resolution: `settings.get("assist.model")` returns the stored value if saved, else the env seed,
  else the default, with `Source::{Stored, Env, Default}`. With `RUSTYBIN_SETTINGS_LOCK=true`,
  `Stored` is ignored for keys marked lockable and writes answer 409.
- Secrets: `Secrets::seal(bytes) -> Sealed { nonce, ciphertext }` with ChaCha20-Poly1305
  (`chacha20poly1305` crate, small, pure Rust, MSRV fine); the key from `RUSTYBIN_SECRETS_KEY`
  (base64, 32 bytes) else `<state>/secrets.key` (random, 0600, generated once; without a state dir
  a process-lifetime key). `settings.secret("assist.openai_key") -> Option<Zeroizing<String>>` for
  the feature that uses it; the API only ever returns `{set, hint}` where `hint` is the last four
  characters. Changing the secrets key makes stored secrets unreadable: the store logs it once, marks
  them `unreadable: true` and the console asks for them again.
- Writes are atomic (temp file plus rename), bounded (at most 256 keys, 64 KiB values, 64 secrets),
  and every change is audited (`rustybin::settings`, key, source, principal, never the value).
- Tests: seal and open round trip, env seed versus stored precedence, lock, the hint, bound checks,
  reload from disk, a wrong key.

### The settings API

| Route | | |
|---|---|---|
| `GET /_rustybin/settings` | admin | Every registered key with value (secrets as `{set, hint}`), source, whether locked, the schema, `persisted` |
| `PATCH /_rustybin/settings` | admin | `{ "values": {key: value}, "secrets": {key: value} }`; validated by the schema; answers the new state; 409 when locked |
| `DELETE /_rustybin/settings/{key}` | admin | Back to the env seed or default |

Catalogue, OpenAPI, Hurl examples (`docs/examples/concepts/settings.hurl`), README tables. The
routes are control plane, so control auth protects them; on an open control plane the admin token
guards writes like `/_rustybin/config` mutations.

### The egress policy (`src/egress.rs`)

```rust
pub struct Egress { allow: Allowlist, loopback: bool, audit: bool }
pub enum Purpose { Assist, McpRelay, Registry, Git, A2aPush, Leads, KeyRegistry, Verify }
pub async fn resolve(&self, url: &Url, purpose: Purpose) -> Result<Pinned, Refusal>
pub fn client(&self, purpose: Purpose) -> reqwest::Client   // timeouts, size caps, pinned resolver, no redirects across hosts
```

- `RUSTYBIN_EGRESS_ALLOW`: the same grammar as `RUSTYBIN_HARNESS_TARGETS` (reuse `safety::Allowlist`,
  moved into `egress` with the harness importing it). Per-purpose defaults added when the purpose is
  configured: the AI provider hosts for `Assist` once a key is set (step 3 registers them), the git
  remote host for `Git`, `registry.modelcontextprotocol.io` for `Registry`. `McpRelay` has no default:
  the operator names the servers the inspector may relay to.
- Forbidden always: link-local, metadata, multicast, unspecified, the instance's own listeners
  (`Verify` excepted, as the harness does today), loopback unless `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true`
  (local demos: a mock MCP server on the same machine).
- Every resolution logs `purpose`, host, pinned address, allowed or refused and why.
- `GET /_rustybin/egress` (admin): the policy as the instance sees it and the last 100 decisions
  (bounded ring). Shown in Management, Network.
- Migration: A2A push, leads and the key registry URL keep their variables and semantics, but call
  `egress.resolve` under their purpose; their existing tests stay green. The harness keeps its own
  list and verification; `safety.rs` shrinks to what is harness-specific.

### Management area in the console

A new view `manage` ("Management", icon `settings`, section "Instance", last) with a left tab rail:
**General** (console title, theme, gateway URL and the admin token the browser holds, moved from the
dialog), **Network** (the egress policy and recent decisions), and empty slots the later steps fill:
Sign-in (02), AI (03), Portal (07), Agents (08), Source control (09), Export (09). The topbar's gear
opens the view instead of the dialog. Each tab is a form generated from the settings schema
(`ui/lib/settingsform.js`, the pattern of `scenarioform.js`): a secret renders as a password field
with "set, ends in k3Qx" and a Replace button; a locked field is read-only with the reason; the
source (environment, stored, default) is shown under each field. Save is per tab with a diff of what
changes and a toast.

### CLI and MCP (for later steps)

Note for step 10: `rustybin settings get|set|unset`, `rustybin egress check <url>`. For step 11:
`get_settings`, `update_settings` (values only; secrets are refused through MCP by design, see the
master plan), `check_egress`.

## Tests

- Unit: settings store, secrets, egress decisions (allow, refuse, pinning, loopback switch,
  per-purpose defaults), the migration of the three existing allowlists.
- Full app: the settings routes with and without control auth, the lock, persistence across a
  restart (`start_with_state` twice on the same temp state dir).
- Console check: the Management view renders every schema field, saves a value, shows a secret as
  set and masked.

## Docs

`docs/src/configuration.md`: `RUSTYBIN_STATE_DIR`, `RUSTYBIN_SETTINGS_LOCK`, `RUSTYBIN_SECRETS_KEY`,
`RUSTYBIN_EGRESS_ALLOW`, `RUSTYBIN_EGRESS_ALLOW_LOOPBACK`, a "Settings and secrets" section with the
precedence rule and a "Outbound connections" section listing every purpose. `docs/src/console.md`:
the Management view. `docs/src/concepts/control-plane-security.md`: secrets at rest. Changelog.

## Done when

The settings store persists across a restart with secrets unreadable on disk; every existing
outbound feature resolves through the egress policy with its tests green; the Management view
replaces the dialog; CLAUDE.md's layout lists `settings.rs`, `egress.rs` and the view; all checks
pass; committed and pushed.

## Build notes

Built 2026-10-29 in one session. Where the build differs from the design above, and why:

- **Specs carry a seed function, not a variable name.** `Spec.env` names the variable for the
  API and the console, and `Spec.seed: fn(&Config) -> Option<Value>` reads the already-parsed
  configuration field, so a setting is validated once (in `config.rs`) and tests build a store
  from a `Config` without touching the process environment. A `Spec.validate` hook adds a check
  beyond the kind (the egress allowlist grammar).
- **Secrets this step registers: one.** `leads.webhook_token` (seeded by
  `RUSTYBIN_LEADS_WEBHOOK_TOKEN`) is the first production secret, so the sealing, the hint, the
  Replace and Clear controls and the browser check have something real to exercise. The leads
  delivery reads it from the store at post time. The AI key and the git token come with their
  steps. The hint is given only for secrets of eight characters or more.
- **A rotated secrets key.** Rather than a stored key fingerprint, every stored secret is opened
  once at startup; the ones that fail are listed in `unreadable_secrets`, logged once, and the
  environment seed is used meanwhile. Setting the secret again clears the flag.
- **Console title and back link are settings.** `console.title` and `console.backlink` (seeded by
  `RUSTYBIN_CONSOLE_TITLE` and `RUSTYBIN_CONSOLE_BACKLINK`) are read by the console's `<meta>`
  tags and `/_rustybin/status` from the store, so the General tab's "Console" card changes
  what everyone sees. Everything else in General (session, gateway URL, admin token, theme) is
  browser-side as before, in a "This browser" card, with the agent panel below.
- **The rail has two tabs.** General and Network. The later tabs (Sign-in, AI, Portal, Agents,
  Source control, Export) are not rendered as empty placeholders; `TABS` in `ui/views/manage.js`
  is the array they append to, and `settingsForm(doc, group)` renders a group's card.
- **Egress: `resolve` then `client`.** `Egress::resolve(url, purpose)` parses, checks the merged
  list (the global `egress.allow` plus the purpose's `Rule`), resolves and pins; `Egress::client
  (&pinned, timeout)` builds a reqwest client whose connector resolves the host to the pinned
  addresses only, with no redirects and the public TLS roots (`egress::tls_config`, which
  `a2a::push::tls_config` re-exports for the modules that build their own clients: the harness
  join, the mocks CLI, `mcp-stdio`). Callers resolve per delivery or poll, so a list change
  applies at once and a name is never re-resolved by the connector. A `Rule` has `any` (the
  A2A `ALLOW_ALL`, never in public mode) and `loopback` (the three legacy purposes keep reaching
  loopback when their own list names it); the new purposes follow `egress.allow_loopback`.
- **`Purpose::Verify` was dropped.** The harness keeps its own allowlist and verification
  (`safety::pin` is a one-line wrapper over `egress::pin_with` naming
  `RUSTYBIN_HARNESS_TARGETS`), so no purpose is needed for it. `safety.rs` keeps the kill switch,
  the lease and the join-URL check, and re-exports the moved items.
- **A2A push behaviour changed in one way.** `RUSTYBIN_A2A_PUSH_ALLOW_ALL` no longer reaches
  link-local or cloud metadata addresses (the policy's always-refused list applies). The
  allowlist variables moved into `Config` (`a2a_push_allowlist`, `a2a_push_allow_all`) and
  `PushPolicy::from_config` replaces `from_env`.
- **`admin_check` answers 403 to a valid control JWT without the admin scope** (401 before).
  The Management view calls admin-only routes, and a 401 on a control URL signs the console out;
  an authenticated caller that is not allowed is a 403. One control-auth test changed with it.
- **Egress decisions keep the origin only** (`scheme://host:port`), never the path or query:
  a webhook or registry URL may carry a token.
- **Bounds**: 256 keys, 64 KiB per value, 64 secrets, 4 KiB per secret, 100 decisions.
- **Left out**: nothing from the design. The CLI and MCP notes stand for steps 10 and 11.

Checks run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (935 tests),
`docs/examples/run.sh` (the settings, control and console examples), the console check
`conformance/ui/run.sh` (a new Management step: every registered setting has a control, the
secret shows "ends in" and never its value, the allowlist is validated and shown in the policy
card, the console title reaches the header after a reload), `mdbook build docs`,
`check_includes.py`, `check-no-dashes.sh`.
