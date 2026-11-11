# Roadmap

What may come next. Nothing under "Next" or "Under consideration" is available yet;
the pages for the features that exist today describe current behaviour.

## Next

Planned on 2026-11-09 and waiting for a go, in
[the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/core-next/master-plan.md)
(steps 14 to 26): a release of what is built and not released yet; then client ID metadata
documents for the MCP inspector, a Prometheus series for chain calls, three additions to secret
requests, variables for a scenario import and a chain hop over HTTP/3; then, each after a
decision, verified pulls, a pull request through the git host and an image with HTTP/3. The two
largest candidates below (OAuth for visitors, self-service across replicas) are planned too and
recommended against for now.

## Recently built

- [AI gateway scenarios](gateway-scenarios.md#ai-gateway-scenarios) (unreleased, built
  2026-11-11, step 01 of the demo next plan): token limits, model fallback, semantic caching,
  prompt and response guards and load balancing, each proven against this instance's AI mock.
- [MCP security](mcp/security.md) (unreleased, built 2026-11-11, step 02 of the demo next plan):
  attack servers, six MCP gateway scenarios against them and three inspector checks.
- [A mock recorded from a real API](mocks/record.md) (unreleased, built 2026-11-11, step 03 of
  the demo next plan): a recording proxy, a spec inferred from the traffic, the answers as
  examples.
- [Contract drift](mocks/drift.md) (unreleased, built 2026-11-11, step 04 of the demo next
  plan): a mock checked against the real API it imitates, JSON or JUnit.
- The last of the roadmap's candidates ([unreleased](changelog.md#unreleased-the-last-of-the-roadmaps-candidates),
  built 2026-11-09): for source control a [diverged branch](source-control.md#a-diverged-branch)
  resolved by a person, [branches per environment](source-control.md#branches) and
  [signed commits](source-control.md#signed-commits); [a secret at an agent's request](agent-mcp.md#a-secret-at-an-agents-request)
  and [an agent's sign-in from the command line](agent-mcp.md#connect-an-agent); and for the
  harness [import from other tools](harness.md#importing-a-postman-collection-or-an-artillery-script),
  [HTTP/3 targets](harness.md#http3-targets) and [server-to-server chains](harness.md#server-to-server-chains).

- [The hosted demo](https://demo.rustybin.dev) (2026-11-07): an instance on the current release,
  private for now, with its mocks at `<name>.rustybin-mock.dev`.

- [Rustybin 0.6, more from an instance](changelog.md#060-2026-11-07-more-from-an-instance)
  (2026-11-07): [specs from a URL](mocks/index.md#a-spec-from-a-url), a
  [push webhook](source-control.md#a-webhook-instead-of-polling), OAuth on a
  [mock's MCP server](mocks/mcp.md#security), and for the harness a leader that
  [steps aside](harness.md#failover), [recordings](harness.md#replaying-a-recording) as a
  source and [OpenTelemetry](harness.md#opentelemetry).

- [Rustybin 0.5.2, an operator's sign-in](changelog.md#052-2026-11-07-an-operators-sign-in-next-to-anonymous-visitors)
  (2026-11-07): [self-service next to the password sign-in](mocks/self-service.md#an-operator-who-signs-in),
  and a harness failover that keeps to its line of succession.

- [Rustybin 0.5.1, safe for a public demo](changelog.md#051-2026-11-06-safe-for-a-public-demo)
  (2026-11-06): no new features; a visitor reads only their own calls of a shared mock,
  a spec's memory is bounded however it is written, and a public instance warns when a
  client can name its own address.

- [Rustybin 0.5, access control](changelog.md#050-2026-11-03-access-control)
  (2026-11-03): [users and invitations](users.md), roles made of named permissions,
  [single sign-on](sso.md) with any OpenID Connect provider, [projects](projects.md)
  so teams share an instance, [AI agents that act for a user](agent-mcp.md#agents-act-for-a-user),
  [open and closed APIs](portal.md#open-and-closed-items) on the dev portal, and an
  [audit log](audit-log.md); a security review of the whole build fixed before the
  release.

- [Rustybin 0.4, testing workflows](changelog.md#040-2026-11-03-testing-workflows)
  (2026-11-03): a [history of saved-test runs](saved-tests.md#history), saved tests
  [on a schedule](saved-tests.md#schedules) with a failure webhook, four more
  [gateway scenarios](gateway-scenarios.md) (circuit breakers, request size limits, CORS,
  header transformation), [checkpoints in the configuration repository](checkpoints.md#in-the-configuration-repository)
  and a [compare view](mocks/index.md#comparing-a-refused-edit) for an edit refused as stale.
- [Rustybin 0.3.1, the 0.3 review](changelog.md#031-2026-11-02-the-03-review)
  (2026-11-02): no new features; what a review of 0.3.0 found, fixed in eight steps:
  control-plane CORS only for listed origins, a random signing key per instance, the
  sign-in changed only with the current password, admin-only saved tests, prunes that
  leave visitors', keys' and scenarios' mocks alone, deploys that name their revision,
  and the [threat model](concepts/public-instances.md) updated.
- [Rustybin 0.3, reliability and testing workflows](changelog.md#030-2026-11-02-reliability-and-testing-workflows)
  (2026-11-02): applies that never prune what they cannot load, previews that validate
  like an apply, git documents read whole; then [diagnostics](troubleshooting.md#ask-the-instance-first),
  [gateway scenarios](gateway-scenarios.md), [saved tests](saved-tests.md) that run in CI,
  [checkpoints](checkpoints.md) and [conflict protection](mocks/index.md#two-people-editing-one-mock)
  for mock edits.
- [Rustybin 0.2.2, the hardening release](changelog.md#022-2026-11-02-the-hardening-release)
  (2026-11-02): no new features; what a full code review found for public instances, fixed
  in ten steps, and the [threat model](concepts/public-instances.md) it now meets.
- [Rustybin 0.2, the AI-assisted mocking service](changelog.md#020-2026-10-30-the-ai-assisted-mocking-service)
  (2026-10-29 to 2026-10-30): the console locked with a user name and password out of
  the box; a settings store with secrets at rest and one outbound policy; your own AI
  key (Anthropic, OpenAI, Gemini or any OpenAI-compatible endpoint) behind an assist
  bar that writes mock specs from a description, fills in missing example payloads and
  edits specs on request; an [MCP inspector](mcp/inspector.md) for any server with
  every auth mode, a relay and test runs; a [mock MCP registry](mcp/registry.md) and
  browsing of real ones; a [dev portal](portal.md) where testers explore and try what
  was published; [AI agents](agent-mcp.md#managing-agents) with rights and revocation;
  the [whole instance as a `rustybin.yaml`](configuration-as-code.md) in
  [git](source-control.md); the [`rustybin` CLI](cli-guide.md) for remote instances;
  and a [management MCP](agent-mcp.md) with a tool for every control route.
- [Load testing through a gateway](harness.md): Rustybin as the load client, with run
  patterns, rate-limit tests, mock-driven traffic and identities, faults and protocol
  sources, clusters with failover, and a conformance run of the compose cluster.
- [An MCP server for every OpenAPI mock](mocks/mcp.md), [OAuth sign-in for
  agents](agent-mcp.md#sign-in-with-oauth), and [self-service mocks](mocks/self-service.md)
  for the hosted demo.

Planned on 2026-11-11 as a second milestone, in
[the demo next plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/demo-next/master-plan.md)
(steps 01 to 04 are built: the AI gateway scenarios, MCP security, recording and drift): demo storyboards and customer demo packs; Arazzo
workflows as multi-step tests; realtime and batch AI APIs; event-driven mocks from AsyncAPI.

## Under consideration

From the build notes of the earlier steps and the harness plan, in no order; each has a step in
the plan named under "Next", except the platform authorization server:

- **MCP inspector**: client ID metadata documents.
- **Source control**: verifying the signatures of pulled commits against allowed
  signers; a pull request opened through the git host's API.
- **Agents**: OAuth sign-in for self-service visitors and shared-mode key holders.
- **Harness follow-ups**: variables given to an import by the caller; a chain over
  HTTP/3; a Prometheus series for chain calls; an image that includes `harness-http3`.
- **Self-service across replicas**: visits and visitor mocks live in memory on one
  instance today. A shared store would let a hosted demo run several replicas
  without routing each mock to one of them.
- **A platform authorization server for hosted instances**: Rustybin already
  points its OAuth metadata at an external issuer (`RUSTYBIN_CONTROL_OAUTH_ISSUER`);
  the platform side that issues control tokens to agents is not built.
