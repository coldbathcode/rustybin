# Gateway recipes

Rustybin is designed to sit behind any API gateway, AI gateway, service mesh or
load balancer. This page lists, per capability, what to configure in the gateway and
what to look at in Rustybin to prove it worked. The first part is gateway-agnostic;
a few vendor snippets follow at the end.

General setup, whatever the gateway:

- Upstream: `http://<rustybin>:80` (HTTP and WebSocket), `https://<rustybin>:443`
  (TLS upstream, demo CA from `/auth/mtls/get-ca-cert`), `<rustybin>:50051` (gRPC, h2c).
- Set `RUSTYBIN_TRUST_FORWARD=true` when the gateway sends `X-Forwarded-*` or
  `Forwarded`, so `/echo`, `/ip`, the OIDC issuer, Agent Cards and MCP metadata
  describe the client-facing URL. Rustybin uses the rightmost non-private address
  of `X-Forwarded-For`, so place it behind proxies you control.
- Tag demo traffic with `X-Rustybin-Session` (a header the gateway can add per
  consumer) and keep the [web console](console.md)'s Live traffic view open: it
  shows every request exactly as the gateway forwarded it.
- Use `GET /` for the gateway's own liveness probe of Rustybin, and `/health` only
  for failover demos.

## HTTP API gateway capabilities

| Capability | Configure in the gateway | Point it at | Observe |
|---|---|---|---|
| Routing, path rewriting | A route with a prefix strip or rewrite | `/anything` | `path`, `url` in the echo |
| Request transformation | Add, remove or rename headers, query parameters, body fields | `/echo` | `headers`, `query_params`, `body` in the echo |
| Response transformation | Add or remove response headers, rewrite the body | `/response-headers?...`, `/echo` | The response the client receives |
| Rate limiting | A limit per consumer or IP | `/echo` | `429` and the gateway's rate-limit headers; the console's load generator draws the histogram |
| Key authentication | Key auth, then strip or forward the key | `/anything` | Whether the key header reached the upstream |
| Credential injection | The gateway adds `apikey: my-key` or Basic credentials | `/auth/api-key`, `/auth/basic-auth` | `200` only when the gateway injected them |
| JWT validation | JWKS from `/oauth/jwks` (RS256) or the HS256 demo secret | `/auth/jwt/decode` | The forwarded token's claims; `/auth/jwt` re-validates |
| OIDC / OAuth 2.0 | Issuer `http://<rustybin>` (discovery), client `rustybin` / `secret`, users `demo` / `demo` | any route | The `Authorization` header or identity headers in `/echo`; introspection at `/oauth/introspect` |
| HMAC signing | The gateway signs requests to the upstream | `/auth/hmac` | `200`, plus the signing string the server computed |
| mTLS to the upstream | Client certificate from `/auth/mtls/get-client-cert`, trust `/auth/mtls/get-ca-cert` | `https://<rustybin>:443/auth/mtls` | `auth_type: mtls`, `source: tls` |
| mTLS termination | Terminate client mTLS, forward the certificate in a header | `/auth/mtls` with `RUSTYBIN_MTLS_IN_HEADER` | `source: header`, the client's subject |
| Caching | Proxy cache honouring `Cache-Control` | `/cache/60` | The gateway's cache status header; Rustybin's inspector shows only the first request |
| Retries | Retry on 5xx | `/flaky/recover/2` or `X-Rustybin-Fail: 503:50` | The client gets `200`; the inspector shows the retried attempts |
| Circuit breaking | Outlier detection / breaker | `/flaky/after/5` | The gateway stops calling after the failures |
| Timeouts | Upstream timeout of a few seconds | `/delay/5s` or `X-Rustybin-Delay` | The gateway's `504` |
| Load balancing, failover | Several Rustybin instances with different `RUSTYBIN_INSTANCE_ID`, active health checks on `/health` | `/identity` | `instance_id` distribution; `POST /health/unhealthy` takes one out |
| CORS | The gateway's CORS policy, with `RUSTYBIN_CORS_ORIGINS=off` | any route | Only the gateway's `Access-Control-*` headers |
| Request size limits | A body size limit below Rustybin's 1 MiB | `POST /echo` | The gateway's `413` |
| IP restriction | Allow or deny lists | `/ip` | The client IP the gateway forwarded |
| Webhook validation | Signature verification plugin with the `demo` secret | sign with `/webhooks/sign`, deliver to `/webhooks/receive/demo` | `204` vs `401` |
| HTTP logging, mirroring | Log or mirror to an HTTP endpoint | a [request bin](reference/request-bin.md) URL | The captured log entries |
| WebSocket | WebSocket proxying | `/ws`, `/ws/time` | Echoed frames, subprotocol pass-through |
| gRPC, gRPC-Web | h2c upstream; gRPC-Web translation | `EchoService` on `:50051` | `metadata` in the reply; `Fail` for status mapping |
| GraphQL | Query depth / cost limits, persisted queries | `/graphql` | `400` from the gateway before Rustybin's own limits |
| SOAP / XML | SOAP routing, XML to JSON | `/soap`, `/soap?wsdl` | The SOAP response and faults |
| Request chaining | Orchestrate steps 1 to 4 | `/orchestration/step/*` | A completed `transaction` only when every header was passed on |

## AI gateway capabilities

| Capability | Configure in the gateway | Point it at | Observe |
|---|---|---|---|
| AI proxy, provider abstraction | Upstream provider URLs from [Providers](ai/providers.md) | `/ai/<provider>/...` | `X-Rustybin-Provider`, native responses |
| Credential injection | The provider key from the gateway's vault | any provider, with `X-Rustybin-Require-Auth: true` | `X-Rustybin-Credential: bearer ****abcd` |
| Token rate limiting | Tokens per minute per consumer | chat completions | Deterministic `usage` counts; the gateway's `429` |
| Prompt decoration / templates | System prompt or template plugins | model `rustybin-echo` | The reply is the decorated prompt |
| Prompt guard | Deny patterns, or an external guardrail service at `/guardrails/*` | any chat endpoint | The gateway blocks before Rustybin is called (inspector stays empty) |
| Response sanitising | PII or secret redaction on responses | model `rustybin-scripted`, prompt `customer record` or `secrets` | Redacted SSN, card, keys in the client response |
| Semantic cache | Embeddings model at `/ai/openai/v1/embeddings` | chat completions | Similar prompts served from cache |
| Fallback, retries | Fallback to a second provider or instance | `X-Rustybin-Fail: 429` or `?fail=overloaded` | The fallback target answers; `X-Rustybin-Instance` |
| Streaming | SSE pass-through, buffering off | `stream: true` with `X-Rustybin-Tokens-Per-Second` | Tokens arrive at the configured pace |
| Observability | Logging, metrics, tracing | any | `X-Rustybin-Request-Id` and `/ai/requests/{id}` |
| MCP gateway | Route on `Mcp-Method` / `Mcp-Name`, OAuth for `/mcp/protected`, tool filtering, server aggregation | `/mcp`, `/mcp/servers/*` | `inspect_request` shows injected identity headers |
| Agent (A2A) gateway | Route per agent, authenticate, rewrite Agent Card URLs | `/a2a/*`, `/.well-known/agent-card.json` | Card URLs pointing at the gateway (with `RUSTYBIN_TRUST_FORWARD`) |

## Vendor examples

The snippets below are **illustrative examples, not executed in CI**. They show
how the generic patterns map onto a few well-known gateways; check them against
your gateway version's documentation. Rustybin itself stays vendor neutral.

### Kong (declarative configuration)

Route `/demo` to Rustybin with key authentication and a rate limit, then look at
`/demo/anything` in the Live traffic view:

```yaml
_format_version: "3.0"
services:
  - name: rustybin
    url: http://rustybin:80
    routes:
      - name: rustybin-demo
        paths: ["/demo"]
        strip_path: true
    plugins:
      - name: key-auth
      - name: rate-limiting
        config:
          minute: 5
          policy: local
consumers:
  - username: alice
    keyauth_credentials:
      - key: alice-key
```

### Envoy (static configuration)

A listener on port 10000 forwarding to Rustybin with a 2 second timeout and
retries on 5xx; try it with `/flaky/recover/2` and `/delay/5s`:

```yaml
static_resources:
  listeners:
    - name: listener_0
      address:
        socket_address: { address: 0.0.0.0, port_value: 10000 }
      filter_chains:
        - filters:
            - name: envoy.filters.network.http_connection_manager
              typed_config:
                "@type": type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager
                stat_prefix: ingress_http
                route_config:
                  virtual_hosts:
                    - name: rustybin
                      domains: ["*"]
                      routes:
                        - match: { prefix: "/" }
                          route:
                            cluster: rustybin
                            timeout: 2s
                            retry_policy:
                              retry_on: 5xx
                              num_retries: 3
                http_filters:
                  - name: envoy.filters.http.router
                    typed_config:
                      "@type": type.googleapis.com/envoy.extensions.filters.http.router.v3.Router
  clusters:
    - name: rustybin
      type: STRICT_DNS
      load_assignment:
        cluster_name: rustybin
        endpoints:
          - lb_endpoints:
              - endpoint:
                  address:
                    socket_address: { address: rustybin, port_value: 80 }
```

### Apache APISIX (Admin API)

A route for `/echo` with round robin over two Rustybin instances (use `/identity`
to see the distribution):

```bash
curl http://127.0.0.1:9180/apisix/admin/routes/rustybin -X PUT \
  -H "X-API-KEY: $APISIX_ADMIN_KEY" \
  -d '{
    "uris": ["/echo", "/identity"],
    "upstream": {
      "type": "roundrobin",
      "nodes": { "rustybin-01:80": 1, "rustybin-02:80": 1 }
    }
  }'
```

Tyk, KrakenD and other gateways follow the same patterns; no snippets are given for
them here.
