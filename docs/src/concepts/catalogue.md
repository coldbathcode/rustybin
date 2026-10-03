# Route catalogue

Every HTTP route Rustybin serves is described once, in a route catalogue
(`src/catalog.rs` plus a `catalog()` function in each module): path, methods,
category, summary, an optional description and runnable examples. Everything
user-facing is generated from it:

| Generated view | Where |
|---|---|
| Landing page with a runnable example per endpoint | `GET /` |
| Collection exports (Postman, Insomnia, Bruno, curl, `.http`, Hurl, k6, HAR) | `GET /export/*`, see [Collection exports](../exports.md) |
| Endpoint tables in `README.md` | `cargo run -- --print-endpoints-markdown` |
| The web console's endpoint list | `GET /_rustybin/catalog` (see [Web console](../console.md)) <!-- TODO(console): verify against ui/ after merge --> |

The OpenAPI document (`/openapi.json`, `/openapi.yaml`, rendered at `/docs`) is
assembled from per-module fragments, and tests keep it aligned with the catalogue.

Consistency tests fail the build when:

- a `.route("...")` registered in the source is missing from the catalogue;
- a catalogue path (or one of its methods) is missing from the OpenAPI document;
- a catalogue example returns 404 or 405 against the full application (examples
  that legitimately answer another status declare it).

So the landing page and exports never advertise a route that does not exist, and
no route exists without documentation. The gRPC service is not an HTTP route: it is
described in [gRPC](../reference/grpc.md) and by server reflection.

Categories, in display order: Echo & Reflection, Status Codes, Response Shaping,
Streaming (SSE), Redirects & Cookies, Info & Random, Auth: Basic & API Key, Auth:
HMAC, Auth: JWT, Auth: OIDC Provider, Auth: mTLS, AI: OpenAI-compatible, AI:
Anthropic-compatible, AI: Mock LLM, AI: Guardrails, MCP Server, A2A Agents, GraphQL,
Orchestration, SOAP / XML, WebSocket, Reliability Testing, Request Bin & Webhooks,
Health & Identity, Control Plane, Docs & Exports.
