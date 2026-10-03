# GraphQL

A GraphQL API with users, products, orders and reviews (the same fake customers
and orders the [MCP](../mcp/index.md) CRM tools use), served over the
GraphQL-over-HTTP conventions.

| Route | Purpose |
|---|---|
| `POST /graphql` | `application/json` (`query`, `variables`, `operationName`, `extensions`) or `application/graphql` (the query as the body) |
| `GET /graphql?query=...` | Queries only (mutations over GET are `405`); browsers asking for `text/html` without a query get the GraphiQL IDE |
| `GET /graphql/schema` | The schema in SDL |
| `GET /graphql/ws` | Subscriptions over WebSocket |

Schema summary (see `/graphql/schema` for the full SDL):

- Queries: `user(id)`, `users(limit, offset)`, `product(id)`, `products(category, limit)`,
  `order(id)`, `orders(userId, status, limit)`.
- Mutations: `createUser(input)`, `updateUser(id, input)`, `createOrder(input)` (nothing
  is stored; the result is computed).
- Subscriptions: `ticker(count, intervalMs)` (1 to 100 ticks, default 5, every 50 to
  10000 ms, default 1000) and `orderUpdates(orderId, intervalMs)` (PENDING to
  DELIVERED, default every 500 ms).

```hurl
{{#include ../../examples/protocols/graphql.hurl:query}}
```

```hurl
{{#include ../../examples/protocols/graphql.hurl:query_get}}
```

```hurl
{{#include ../../examples/protocols/graphql.hurl:graphql_body}}
```

```hurl
{{#include ../../examples/protocols/graphql.hurl:operation_name}}
```

```hurl
{{#include ../../examples/protocols/graphql.hurl:mutation}}
```

```hurl
{{#include ../../examples/protocols/graphql.hurl:mutation_get}}
```

## Status codes and media types

The response media type follows `Accept`:

- `application/json`, or no `Accept` header: the legacy behaviour, `200` for every
  well-formed request, errors in the `errors` array.
- `application/graphql-response+json`, and also `*/*` (what curl and Hurl send by
  default): the GraphQL-over-HTTP status codes, `400` when the document fails to
  parse or validate or exceeds a limit.

```hurl
{{#include ../../examples/protocols/graphql.hurl:status_codes}}
```

## Limits

Query depth 10, complexity 500 and 30 aliases (fragments expanded), so you can show
a gateway's own GraphQL protection in front of a server that also defends itself.

```hurl
{{#include ../../examples/protocols/graphql.hurl:depth_limit}}
```

## Automatic persisted queries

Send `extensions.persistedQuery` (`version: 1`, `sha256Hash` of the query text).
An unknown hash answers `PersistedQueryNotFound`; sending the query with its hash
registers it (the hash is checked); afterwards the hash alone is enough. The cache
is bounded. This is the flow APQ-aware gateways and CDNs rely on.

```hurl
{{#include ../../examples/protocols/graphql.hurl:apq}}
```

## Subscriptions

`/graphql/ws` speaks both WebSocket subprotocols: `graphql-transport-ws` (the
`graphql-ws` library) and the legacy `graphql-ws` (subscriptions-transport-ws). With
[websocat](https://github.com/vi/websocat) (`$WS` is the base URL with `ws://`, for
example `ws://localhost:8080`):

```bash
{{#include ../../examples/protocols/websocket.sh:graphql_subscription}}
```

```hurl
{{#include ../../examples/protocols/graphql.hurl:schema}}
```
