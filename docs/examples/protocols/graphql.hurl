# GraphQL over HTTP (docs/src/reference/graphql.md)

# ANCHOR: query
POST {{base_url}}/graphql
Content-Type: application/json
{"query": "{ users(limit: 2) { id name email } }"}
HTTP 200
[Asserts]
jsonpath "$.data.users" count == 2
jsonpath "$.data.users[0].id" == "u1"
jsonpath "$.data.users[0].name" == "Alice Chen"
# ANCHOR_END: query

# ANCHOR: query_get
GET {{base_url}}/graphql
[Query]
query: { products(limit: 2) { id name price } }
HTTP 200
[Asserts]
jsonpath "$.data.products" count == 2
# ANCHOR_END: query_get

# ANCHOR: graphql_body
POST {{base_url}}/graphql
Content-Type: application/graphql
```
{ product(id: "p1") { name price inStock } }
```
HTTP 200
[Asserts]
jsonpath "$.data.product.name" == "Mechanical Keyboard"
# ANCHOR_END: graphql_body

# ANCHOR: operation_name
POST {{base_url}}/graphql
Content-Type: application/json
{
  "query": "query A { users(limit: 1) { name } } query B { products(limit: 1) { name } }",
  "operationName": "B",
  "variables": {}
}
HTTP 200
[Asserts]
jsonpath "$.data.products" count == 1
jsonpath "$.data.users" not exists
# ANCHOR_END: operation_name

# ANCHOR: mutation
POST {{base_url}}/graphql
Content-Type: application/json
{"query": "mutation($in: CreateUserInput!) { createUser(input: $in) { id name role } }", "variables": {"in": {"name": "Ada", "email": "ada@example.com"}}}
HTTP 200
[Asserts]
jsonpath "$.data.createUser.name" == "Ada"
jsonpath "$.data.createUser.role" == "USER"
# ANCHOR_END: mutation

# ANCHOR: mutation_get
GET {{base_url}}/graphql
[Query]
query: mutation { createUser(input: {name: "x", email: "y"}) { id } }
HTTP 405
# ANCHOR_END: mutation_get

# ANCHOR: status_codes
# Legacy media type: 200 even for a validation error.
POST {{base_url}}/graphql
Accept: application/json
Content-Type: application/json
{"query": "{ nope }"}
HTTP 200
[Asserts]
header "Content-Type" == "application/json"
jsonpath "$.errors" count >= 1

# GraphQL-over-HTTP media type (also chosen for Accept: */*): 400.
POST {{base_url}}/graphql
Accept: application/graphql-response+json
Content-Type: application/json
{"query": "{ nope }"}
HTTP 400
[Asserts]
header "Content-Type" contains "application/graphql-response+json"
# ANCHOR_END: status_codes

# ANCHOR: apq
# Automatic persisted queries: the hash alone is unknown at first...
POST {{base_url}}/graphql
Content-Type: application/json
{"extensions": {"persistedQuery": {"version": 1, "sha256Hash": "b55b2debe0cc080e753c0833207d47312584ae9adc4b1a4f0b9f138af321e371"}}}
HTTP 200
[Asserts]
jsonpath "$.errors[0].message" == "PersistedQueryNotFound"
jsonpath "$.errors[0].extensions.code" == "PERSISTED_QUERY_NOT_FOUND"

# ...register it by sending the query with its SHA-256 hash...
POST {{base_url}}/graphql
Content-Type: application/json
{"query": "query DocsApq { users(limit: 1) { name } }", "extensions": {"persistedQuery": {"version": 1, "sha256Hash": "b55b2debe0cc080e753c0833207d47312584ae9adc4b1a4f0b9f138af321e371"}}}
HTTP 200

# ...then the hash is enough.
POST {{base_url}}/graphql
Content-Type: application/json
{"extensions": {"persistedQuery": {"version": 1, "sha256Hash": "b55b2debe0cc080e753c0833207d47312584ae9adc4b1a4f0b9f138af321e371"}}}
HTTP 200
[Asserts]
jsonpath "$.data.users[0].name" == "Alice Chen"
# ANCHOR_END: apq

# ANCHOR: depth_limit
POST {{base_url}}/graphql
Accept: application/graphql-response+json
Content-Type: application/json
{"query": "{ users { orders { user { orders { user { orders { user { orders { user { orders { user { name } } } } } } } } } } } }"}
HTTP 400
[Asserts]
jsonpath "$.errors[0].message" contains "nested too deep"
# ANCHOR_END: depth_limit

# ANCHOR: schema
GET {{base_url}}/graphql/schema
HTTP 200
[Asserts]
body contains "type QueryRoot"
body contains "type SubscriptionRoot"
# ANCHOR_END: schema
