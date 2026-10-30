# Find a task

Pick the outcome you need. Each link takes you to the guide or reference that
covers it. If you have not started an instance yet, begin with the
[quick start](quick-start.md).

## Run and connect

| Task | Guide |
|---|---|
| Start Rustybin locally | [Docker](quick-start.md#docker), [Cargo](quick-start.md#cargo), [Compose](quick-start.md#docker-compose) |
| Sign in to the web console | [First sign-in](quick-start.md#first-sign-in) |
| Use the CLI with an instance | [Sign in and profiles](cli-guide.md#sign-in-and-profiles) |
| Point a gateway at Rustybin | [Gateway recipes](gateway-recipes.md) |
| Find a route and try it | Open `/` or `/docs` on your running instance; [HTTP reference](reference/http-basics.md) |
| Change a port or environment variable | [Configuration](configuration.md#listeners-and-process) |

## Build and share mocks

| Task | Guide |
|---|---|
| Turn an OpenAPI spec into a mock | [Create a mock](mocks/index.md#quick-start) |
| Generate a spec from a description | [Build mocks with AI](mocks/ai.md#describe-an-api) |
| Choose response bodies, errors and rules | [Responses, rules and validation](mocks/responses.md) |
| Add API key, JWT or OAuth security to a mock | [Mocked security](mocks/security.md) |
| Use a mock API as an MCP server | [An MCP server for every mock](mocks/mcp.md) |
| Publish a mock for testers | [Dev portal](portal.md) |
| Download a Postman, Bruno, Hurl or other collection | [Collection exports](exports.md) |
| Keep mocks after a restart | [Persistence](troubleshooting.md#my-changes-disappeared-after-a-restart), [mocks as code](mocks/config.md) |
| Offer temporary mocks to visitors | [Self-service mocks](mocks/self-service.md) |

## Test AI and agents

| Task | Guide |
|---|---|
| Set an SDK or gateway's provider base URL | [Providers and paths](ai/providers.md) |
| Return the received prompt or a scripted response | [Mock LLM behaviour](ai/behaviour.md#modes) |
| Test tool calls and structured output | [Tool calls](ai/behaviour.md#tool-calls), [structured output](ai/behaviour.md#structured-output) |
| Trigger a provider error or delay | [Faults and latency](ai/gateway-features.md) |
| Try semantic caching or moderation | [Embeddings](ai/embeddings.md), [guardrails](ai/guardrails.md) |
| Connect to the mock MCP server | [MCP server](mcp/index.md) |
| Inspect another MCP server and check its auth | [MCP inspector](mcp/inspector.md) |
| Browse or simulate a server registry | [MCP registry](mcp/registry.md) |
| Send a task to an A2A agent | [A2A agents](a2a/index.md) |
| Let an AI agent configure Rustybin | [Management MCP](agent-mcp.md) |
| Connect my own AI provider to the console | [Bring your own AI](ai-assist.md) |

## Observe and break things

| Task | Guide |
|---|---|
| See exactly what reached the upstream | [Request inspector](concepts/inspector.md) |
| Keep my demo traffic separate from other users | [Sessions](concepts/sessions.md) |
| Add a delay or return a chosen HTTP error | [Fault injection](concepts/fault-injection.md) |
| Test retries, health checks or circuit breakers | [Reliability endpoints](reference/reliability.md) |
| Capture callbacks and verify webhook signatures | [Request bin](reference/request-bin.md), [webhooks](reference/webhooks.md) |
| Test OAuth, JWT, API keys or mTLS | [OAuth / OIDC](reference/oidc.md), [authentication](reference/auth.md) |
| Send gRPC, WebSocket or SSE traffic | [gRPC](reference/grpc.md), [WebSocket](reference/websocket.md), [SSE](reference/sse.md) |
| Run load through a gateway | [Load testing](harness.md) |

## Operate and automate

| Task | Guide |
|---|---|
| Change the console password or configure tokens | [Control-plane security](concepts/control-plane-security.md) |
| Save settings and secrets | [State directory, settings and secrets](configuration.md#state-directory-settings-and-secrets) |
| Allow an outbound connection | [Outbound connections](configuration.md#outbound-connections) |
| Export, diff and apply an entire instance | [Instance as code](configuration-as-code.md) |
| Store and sync configuration in git | [Source control](source-control.md) |
| Script a task in CI | [CLI scripts and CI](cli-guide.md#scripts-and-ci) |
| Run a shared instance with API keys and quotas | [Shared mode](concepts/shared-mode.md), [plans and limits](concepts/plans-and-limits.md) |
| Check readiness, metrics and logs | [Security and operations](concepts/control-plane-security.md) |
| Run the examples used by these docs | [How the examples work](examples.md#running-them) |
