# Master plan: the hosted demo

Planned 2026-11-06, after 0.5.0, when the owner said go on the next step and no plan was queued.
The theme comes from `docs/src/roadmap.md`: the self-service code was built on 2026-10-18 and only
deployment was thought to be left. Built only when the owner says go, one step at a time, each
pushed as it lands. Steps 05 and 06 change the private `rustybin-platform` repository and live
infrastructure, so they are the owner's to start.

## 1. What this milestone is

Put the public demo online on the current release: visitors create their own mocks at
`demo.rustybin.dev`, served at `<name>.rustybin-mock.dev`. The self-service code predates the
console sign-in, the hardening and access control, and nobody has run the demo's configuration
since. This milestone runs it, fixes what that finds, then deploys it.

## 2. Principles

- **Run the real configuration**, the environment of `infra/oracle/compose.yaml`, not a test double.
- **A failing test first**, as in the last plans; a fix is seen to fail without it.
- **The threat model is the specification**: `docs/src/concepts/public-instances.md` says what a
  stranger cannot do, and each finding is measured against it.
- **Scope**: one instance at launch and no stateful mode for visitor mocks (the owner's decisions of
  2026-10-18), so neither replicas nor visitor state are part of this plan.

## 3. What the first run found

The demo's configuration against `v0.5.0` (`f018ef7`), 2026-11-06:

| Finding | Severity | Where it goes |
|---|---|---|
| Any caller read another visitor's captures on an instance mock, with `Authorization`, `Cookie` and the query string; the session id in them also opens that visitor's inspector entries | high | step 01 |
| `infra/oracle/compose.yaml` does not boot: public mode with the default password is fatal, and self-service is ignored under the default `password` control auth | blocks launch | step 05 |
| `RUSTYBIN_STATE_DIR` is not set in the compose file: settings, the audit log and sessions stay in memory | medium | steps 02, 05 |
| `POST /_rustybin/mcp-checks` is open to anonymous callers and its 50 saved runs are one shared list: a visitor sees other visitors' targets and can evict them | low | step 03 |
| `GET /_rustybin/status` names the host and `/_rustybin/metrics` is open to anonymous callers | low, to judge | step 03 |
| The free plan's rate limit answered `429` to a burst on the control plane; whether a console load meets it is unmeasured | unknown | step 02 |
| `demo.rustybin.dev` and `rustybin-mock.dev` do not resolve; `https://rustybin.dev/legal/privacy/` answers `200` | expected | step 06 |

What steps 02 and 03 found, running the image at the demo's limits:

| Finding | Severity | Where it goes |
|---|---|---|
| One anonymous `POST` of 160 KiB held 645 MiB and ended the container; about 100 ordinary large mocks did too | high | step 02, done |
| With `RUSTYBIN_TRUST_FORWARD_HEADER` unset a visitor names its own address, past every per-address limit; through Cloudflare `X-Forwarded-For` would name the edge | high | step 03 warns; the fix is step 05 |
| `rustybin-mock.dev` was not registered | blocks launch | registered 2026-11-07 at Cloudflare; its records are step 06 |
| Self-service is ignored under `password` control auth, so the owner's sign-in and visitor mocks cannot run together | blocks launch | step 04b |

Checked and sound: creating a mock, calling it on its own host, a restart keeping it, `https` URLs
behind the proxy. Of 209 control route and method pairs called without a credential, 106 answered
`401` and one `403`; the answers of the open ones were not all read in full, which is part of step
03. `mcp-relay`, `mcp-relay/fetch` and
`mcp-checks` refused every target, metadata and internal addresses included, because the
`mcp_relay` purpose has no default.

## 4. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Needs | Status |
|---|---|---|---|
| [01](step-01-traffic-isolation.md) | One visitor never reads another's traffic | | done 2026-11-06 |
| [02](step-02-demo-limits.md) | The signed image under the demo's limits: read-only root, tmpfs, 512 MiB, arm64, a console load on the free plan | | done 2026-11-06 |
| [03](step-03-anonymous-surface.md) | The rest of the anonymous surface: shared check runs, status and metrics | | done 2026-11-06 |
| 04 | Release 0.5.1 (the release recipe of the last plans; the digest is in the changelog) | 01 to 03 | done 2026-11-06 |
| [04b](step-04b-operator-sign-in.md) | The owner's sign-in on a self-service instance: `password` control auth with visitors still anonymous (section 6), released as 0.5.2 | 04 | done 2026-11-07 |
| 05 | `rustybin-platform`: `RUSTYBIN_CONTROL_AUTH=password`, `RUSTYBIN_CONSOLE_USER=oli`, `RUSTYBIN_CONSOLE_PASSWORD_HASH` from the VM's `.env`, `RUSTYBIN_STATE_DIR`, `RUSTYBIN_TRUST_FORWARD_HEADER=x-real-ip` with `header_up X-Real-IP {client_ip}` on both demo sites of the Caddyfile, `hostname: demo`, the 0.5.2 digest | 04b | not started, needs the owner's go |
| 06 | Go live: `mocks_zone` set so the apex and wildcard of `rustybin-mock.dev` exist, DNS and TLS for both hosts, the lead webhook, signing in as `oli`, a walk through the live demo | 05 | not started, needs the owner's go |

## 5. How a session works

As in the testing plan: read this file, the step file and CLAUDE.md; tests first, then the code,
then the docs; run fmt, clippy (also with `rustup run stable`), the full suite,
`scripts/check-no-dashes.sh`, the docs examples and, when the console changed, `conformance/ui`.
Add the step's build notes, set the row to done with the date, push.

## 6. Decisions for the owner

- **0.5.1 before step 02 or after step 03** (decided 2026-11-06): nothing of the owner's runs
  Rustybin in public, so it stayed after step 03.
- **Anonymous MCP checks on the demo** (decided in step 03): answered and not kept.
- **The owner's sign-in** (asked 2026-11-07, decided: build it): the demo has one user, `oli`, and
  nobody else signs in online. The password is never written here, in the compose file or in a tool
  input: this repository is public, so only its argon2id hash goes in the VM's `.env`, single quoted
  because the hash holds `$`. Today self-service works only with `open` control auth and the sign-in
  only with `password`, so this needs step 04b: the sign-in on a self-service instance, anonymous
  callers keeping exactly what they have under `open`, and its throttle depending on the address of
  step 05. Without building it there are two other shapes: stay on `open` and sign in with the admin
  token (no user name or password), or a private instance with self-service off (no visitors).
- **The default session on a shared address**: a caller without `X-Rustybin-Session` is its address,
  so people behind one NAT share a view of their calls to an instance mock. The inspector avoids
  this by asking for a session; the plan keeps the address so the sample works without setup.
