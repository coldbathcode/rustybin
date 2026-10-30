# Rustybin

Rustybin is a single-binary mocking and testing service for API gateway, AI gateway
and agent gateway demos. Put it behind any gateway as the upstream and it gives you
something realistic to route to; put a team in front of it and it is the place they
set up, explore and manage what they test against.

- **Describe** what you need: an OpenAPI spec written by your own AI from a sentence,
  example payloads filled in where a spec has none, edits made on request, every
  change shown as a diff before it is applied.
- **Explore** it: a dev portal where testers find what was published and try it, an
  MCP inspector for any MCP server with every auth mode, a mock MCP registry, a live
  view of what the gateway forwarded.
- **Manage** it from wherever you are: the web console, the `rustybin` command line,
  an AI agent over the management MCP server, or a `rustybin.yaml` in git. Every
  capability is on all five surfaces (the API included).
- **Lock** it on day one: the console and the control plane ask for a user name and
  password, AI keys and tokens are sealed at rest, every outbound call goes through
  one allowlist.

Behind the gateway it stays what it was: echo endpoints that show exactly what was
forwarded, auth endpoints that really validate credentials, a mock LLM that speaks
the native wire format of seven AI providers, an MCP server, A2A agents, an identity
provider, GraphQL, SOAP, gRPC, WebSocket, Server-Sent Events and JSON-RPC, failure
modes on demand, and a load client that measures the gateway from both sides. Every
response is deterministic where it matters and every piece of state is bounded, so a
shared instance stays healthy.

## Who it is for

- **Presales and solution engineers** who need a dependable upstream for live demos
  of rate limiting, authentication, transformation, AI proxying, prompt guards,
  caching, retries and circuit breakers, and a portal to hand the audience.
- **Gateway and platform teams** who want a test target for plugin development and
  conformance checks (the mock LLM, MCP server and A2A agents are exercised by the
  official OpenAI, Anthropic, Google Gen AI, MCP and A2A SDKs in CI), with the
  instance's configuration in git.
- **Developers** who want a local fake for an API they are still waiting for, an
  LLM provider, an OAuth provider or a webhook sender, built from a sentence.

## What is inside

**Build and explore**

| Area | Highlights |
|---|---|
| [OpenAPI mocks](mocks/index.md) | Upload an OpenAPI 3.0 / 3.1 or Swagger 2.0 spec and get a mock API at `/mock/<name>`: spec examples or generated data, rules, request validation, deploy checks, captured traffic, mocked API key, basic, bearer, OAuth 2.0 and mTLS security, an [MCP server per mock](mocks/mcp.md) |
| [Build mocks with AI](mocks/ai.md) | A spec from a description (to the OpenAPI version you pick), example payloads for every operation that lacks one with an alert when a spec has none, edits by instruction shown as a diff |
| [Bring your own AI](ai-assist.md) | Anthropic, OpenAI, Gemini or any OpenAI-compatible endpoint with your key, from the environment or the settings; an assist bar in every view, tokens and cost per answer, nothing applied without a click; a built-in mock provider for demos without a key |
| [Dev portal](portal.md) | A read-only portal at `/portal` with only what was published: mock APIs with Try it and downloads in seven formats, the mock MCP servers with connect snippets, the mock LLM, agents and tools; open, behind the sign-in or an API key |
| [MCP inspector](mcp/inspector.md) | Any MCP server over every transport and auth mode (bearer, API key, basic, custom headers, OAuth 2.1 with the discovery walk), a relay for servers the browser cannot reach, fourteen checks as JSON or JUnit, saved connections |
| [MCP registry](mcp/registry.md) | A mock registry in the registry API's shape listing every MCP server of the instance, with failure scenarios on demand, and browsing of real registries |
| [Web console](console.md) | A browser UI at `/ui` for live traffic, mock APIs, request bins, the AI playground, MCP and A2A clients, the harness, the agents and the Management area |

**Manage and lock**

| Area | Highlights |
|---|---|
| [The command line](cli-guide.md) | `rustybin login` then every noun of the control plane against a remote instance: mocks, settings, portal, config, git, agents, harness; tables for people, `--json` for scripts |
| [Configure from an AI agent](agent-mcp.md) | A management MCP server at `/_rustybin/mcp` with a tool for every control route, prompts for the common jobs (build an instance, mock an API, publish the portal, back up to git), OAuth sign-in for agents, and a view of who talks to the instance with rights and revocation |
| [The whole instance as code](configuration-as-code.md) | Export, diff and apply the instance as a `rustybin.yaml` (settings, secrets as references, mocks, registry, portal, agents, harness, health), load it at startup |
| [Source control](source-control.md) | Commit and push the document to a git repository with a token or a deploy key, pull and apply, resolve a diverged branch, start from the repository, automatic push or pull |
| [Console sign-in](concepts/control-plane-security.md) | A user name and password out of the box (`rustybin` / `rustybin`, change it first), sessions, control tokens, Ed25519 JWTs with scopes for platforms, OAuth for agents |
| [Settings, secrets and egress](configuration.md#state-directory-settings-and-secrets) | Instance settings seeded by the environment and saved in one state directory, secrets sealed at rest and never shown again, one outbound allowlist with pinned addresses and an audit of every decision |

**The upstream**

| Area | Highlights |
|---|---|
| [Mock LLM](ai/index.md) | OpenAI (Chat Completions, Responses, embeddings, moderations, images, audio), Azure OpenAI, Anthropic, Gemini, AWS Bedrock, Ollama and Cohere wire formats; streaming; tool calls and structured output from your JSON schema; deterministic tokens; native errors and rate-limit headers on demand; credential checks that prove what the gateway injected |
| [Guardrails](ai/guardrails.md) | Fake Azure AI Content Safety, Bedrock ApplyGuardrail and generic PII / jailbreak detectors for guardrail plugins |
| [MCP server](mcp/index.md) | Streamable HTTP for protocol 2026-07-28 (stateless) and the 2025 handshake versions, legacy HTTP+SSE, OAuth 2.1 protected and API key variants, named servers with tool subsets |
| [A2A agents](a2a/index.md) | Seven demo agents speaking A2A v1.0 and v0.3 over JSON-RPC and HTTP+JSON, streaming, multi-turn, auth-required, push notifications |
| [Identity provider](reference/oidc.md) | OAuth 2.0 / OpenID Connect with authorization code + PKCE, client credentials, password, refresh, token exchange, introspection, revocation, dynamic client registration and your own users |
| [Auth checks](reference/auth.md) | Basic, API key, JWT (HS256 and RS256), HMAC signatures, mTLS |
| [Protocols](reference/graphql.md) | GraphQL (with subscriptions and persisted queries), SOAP 1.1 / 1.2, gRPC (with reflection, health and gRPC-Web), WebSocket, SSE, JSON-RPC |
| [HTTP toolbox](reference/http-basics.md) | Echo, any status code, delays, caching, compression, ranges, redirects, cookies, request bins, webhook signatures |
| [Chaos](concepts/fault-injection.md) | `X-Rustybin-Delay` and `X-Rustybin-Fail` on every route, flaky endpoints, a health toggle |
| [Observability](concepts/inspector.md) | A request inspector with a live feed, per-session scoping, and an [AI request inspector](ai/gateway-features.md#request-inspection) |
| [Load testing](harness.md) | Rustybin as the load client too: scenarios with run patterns, rate-limit tests, traffic from mocks, identities and faults, on one node or a cluster, with a report that separates the gateway from the upstream |

## Ports

| Listener | Default port | Environment variable |
|---|---|---|
| HTTP | 80 | `RUSTYBIN_HTTP_PORT` |
| HTTPS (self-signed demo certificate, optional client certificates) | 443 | `RUSTYBIN_HTTPS_PORT` |
| gRPC (h2c, plus gRPC-Web over HTTP/1.1) | 50051 | `RUSTYBIN_GRPC_PORT` |

The HTTP and HTTPS listeners serve the same routes. Only the HTTP listener is
required: if HTTPS or gRPC cannot start, Rustybin logs a warning and keeps serving.

## Where to go next

- [Quick start](quick-start.md): run it with Docker, Docker Compose, Cargo or on Fly.io,
  sign in, add an AI key, describe a mock and publish it in five commands.
- [Configuration](configuration.md): every environment variable and every setting.
- [Gateway recipes](gateway-recipes.md): what to configure in a gateway and what to
  look at in Rustybin for each capability.
- `GET /` on a running instance lists every endpoint with a runnable example, and
  `/openapi.json` and `/docs` describe the HTTP API.

Rustybin is open source under the GNU AGPL-3.0, with a commercial license for
organisations that cannot use AGPL software (see `LICENSING.md` in the
repository).
