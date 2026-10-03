# Identity and load balancing

`/identity` (any method) tells you which instance answered and what it saw of the
request, the basic building block of load balancing, sticky session and failover
demos.

| Field | Meaning |
|---|---|
| `instance_id` | `RUSTYBIN_INSTANCE_ID` (a random UUID when unset) |
| `hostname`, `version`, `uptime_seconds` | The process |
| `request_count` | How many `/identity` requests this instance has answered (useful to show how a balancer spreads load) |
| `port` | The HTTP, HTTPS and gRPC ports, and the `listener` (`scheme`, `port`) this request came in on |
| `config` | Non-secret settings: host, public mode, trust forward, body limit, request timeout, max delay, inspector capacity, CORS origins, `admin_token_configured` |
| `environment` | Rust compiler, MSRV, build profile, OS, architecture |
| `request` | Method, TCP peer IP, resolved client IP, `Host`, scheme and base URL |
| `timestamp` | Now |

```hurl
{{#include ../../examples/reliability/health.hurl:identity}}
```

## Running several instances

Give each replica its own `RUSTYBIN_INSTANCE_ID`:

```bash
docker run -d -p 8001:80 -e RUSTYBIN_INSTANCE_ID=instance-01 --name rb1 rustybin
docker run -d -p 8002:80 -e RUSTYBIN_INSTANCE_ID=instance-02 --name rb2 rustybin
docker run -d -p 8003:80 -e RUSTYBIN_INSTANCE_ID=instance-03 --name rb3 rustybin
```

(`docker-compose.yml` has a commented-out block doing the same.) The instance id
also appears in `/health`, in the mock LLM's `X-Rustybin-Instance` header and in the
gRPC `EchoService` responses, so every protocol shows which upstream was picked.

Demo ideas:

- **Round robin / weighted**: call `/identity` repeatedly through the gateway and
  count `instance_id` values.
- **Failover**: `POST /health/unhealthy` on one instance (see
  [Reliability](reliability.md#health-toggle)) and watch the gateway's active health
  check take it out of rotation.
- **Sticky sessions / consistent hashing**: hash on `X-Rustybin-Session` or a cookie
  (`/cookies/set?...`) and show that the same client keeps hitting the same
  `instance_id`.
- **Client IP preservation**: compare `request.peer_ip` (the gateway) with
  `request.remote_ip` (the client, when `RUSTYBIN_TRUST_FORWARD=true` and the
  gateway sends `X-Forwarded-For` or `Forwarded`).
