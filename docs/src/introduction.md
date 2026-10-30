<div class="rb-hero">
<p class="rb-eyebrow">Rustybin documentation</p>

<span id="rustybin"></span>

# Your gateway’s<br>testing ground.

<p class="rb-lead">Mock an API. Route an AI request. Watch what your gateway forwards. One binary gives you a predictable place to build, explore and break things.</p>

<div class="rb-actions">
<a class="rb-primary" href="quick-start.md">Run Rustybin →</a>
<a href="tasks.md">Find a task →</a>
</div>
</div>

## Start with what you need

<div class="rb-cards">
<a class="rb-card" href="mocks/index.md"><small>01 / BUILD</small><strong>Give your API a working mock →</strong><span>Upload an OpenAPI spec, get realistic responses, then share it with your team.</span></a>
<a class="rb-card" href="ai/index.md"><small>02 / SIMULATE</small><strong>Test an AI gateway →</strong><span>Seven provider formats, streaming, tool calls and predictable errors. No provider key needed.</span></a>
<a class="rb-card" href="gateway-recipes.md"><small>03 / OBSERVE</small><strong>See what the gateway did →</strong><span>Inspect forwarded headers, demonstrate auth, inject failures and check retries.</span></a>
<a class="rb-card" href="mcp/index.md"><small>04 / CONNECT</small><strong>Exercise agents and MCP →</strong><span>Connect to a mock MCP server, inspect tools or test an A2A agent.</span></a>
</div>

> **New here?** [Start a local instance](quick-start.md#docker), then send your
> [first requests](quick-start.md#first-requests). You can try the built-in API,
> mock LLM and MCP server before configuring anything else.

## Who it is for

Rustybin is a single-binary mocking and testing service for API, AI and agent
gateways. Run it locally, in a container or behind your gateway as the upstream.

- **Demo a gateway:** use reproducible responses and trigger failures on cue.
- **Develop against an API:** turn an OpenAPI spec into a working mock, or
  [describe one with your own AI](mocks/ai.md).
- **Test a platform:** exercise authentication, protocols and load scenarios;
  keep the instance's configuration in git.

## What is inside

| I want to… | Start here |
|---|---|
| Build and edit an API mock | [OpenAPI mocks](mocks/index.md), [responses and rules](mocks/responses.md), [build with AI](mocks/ai.md) |
| Share something testers can try | [Dev portal](portal.md), [collection exports](exports.md) |
| Simulate an AI provider | [Mock LLM](ai/index.md), [provider URLs](ai/providers.md), [guardrails](ai/guardrails.md) |
| Connect an agent or inspect an MCP server | [MCP server](mcp/index.md), [MCP inspector](mcp/inspector.md), [A2A agents](a2a/index.md) |
| Inspect requests or introduce failures | [Request inspector](concepts/inspector.md), [fault injection](concepts/fault-injection.md), [gateway recipes](gateway-recipes.md) |
| Test authentication or another protocol | [Authentication](reference/auth.md), [OAuth / OIDC](reference/oidc.md), [HTTP and protocol reference](reference/http-basics.md) |
| Measure a gateway under load | [Load testing](harness.md) |
| Manage and reproduce an instance | [Web console](console.md), [CLI](cli-guide.md), [management MCP](agent-mcp.md), [instance as code](configuration-as-code.md), [source control](source-control.md) |

**Two AI features, different purposes:** the [mock LLM](ai/index.md) simulates
provider responses without a real model or key. [AI assist](ai-assist.md) connects
your own provider to help build mocks and manage the instance.

## Ports

| Listener | Default port | With the quick-start Docker mapping |
|---|---|---|
| HTTP | `80` | `http://localhost:8080` |
| HTTPS | `443` | `https://localhost:8443` (demo certificate) |
| gRPC | `50051` | `localhost:50051` (plaintext h2c) |

HTTP and HTTPS serve the same routes. HTTP is required; if HTTPS or gRPC cannot
start, Rustybin logs a warning and keeps serving HTTP. Change listeners with
[environment variables](configuration.md#listeners-and-process).

## Where to go next

- **Do something:** [find a task](tasks.md), or follow the [quick start](quick-start.md).
- **Look something up:** [configuration](configuration.md), [CLI reference](cli.md),
  or the endpoint list at `/` on your running instance.
- **Explore the API:** `/docs` is the interactive OpenAPI reference on the instance;
  `/openapi.json` is its machine-readable document.
- **Get unstuck:** [troubleshooting](troubleshooting.md), including sign-in,
  certificates, persistence and outbound connections.

Rustybin is open source under the GNU AGPL-3.0, with a commercial license available.
See [licensing](https://github.com/coldbathcode/rustybin/blob/main/LICENSING.md).
