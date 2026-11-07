# Self-service next to the password sign-in: visitors need no account, and only
# the operator signs in.
# Run by docs/examples/run.sh against the instance with both.

# ANCHOR: ssi_visitor
# A caller without a credential creates a mock as a visitor...
POST {{signin_url}}/_rustybin/mocks
X-Rustybin-Session: docs-sign-in-visitor
{"spec": "{\"openapi\":\"3.0.3\",\"info\":{\"title\":\"Orders\",\"version\":\"1\"},\"paths\":{\"/orders\":{\"get\":{\"operationId\":\"list\",\"responses\":{\"200\":{\"description\":\"ok\"}}}}}}"}
HTTP 201
[Captures]
visitor_mock: jsonpath "$.name"
[Asserts]
jsonpath "$.manage_token" exists

# ...and reaches nothing that is the operator's.
GET {{signin_url}}/_rustybin/settings
HTTP 401
GET {{signin_url}}/_rustybin/users
HTTP 401
# ANCHOR_END: ssi_visitor

# A credential that fails is refused, never treated as a visitor.
GET {{signin_url}}/_rustybin/mocks
Authorization: Bearer not-a-token
HTTP 401

# ANCHOR: ssi_operator
# The operator signs in...
POST {{signin_url}}/_rustybin/login
{"username": "operator", "password": "docs-operator-password"}
HTTP 200
[Captures]
operator_token: jsonpath "$.token"

# ...and holds the instance, a visitor's mocks too.
GET {{signin_url}}/_rustybin/mocks?owner=all
Authorization: Bearer {{operator_token}}
HTTP 200
[Asserts]
jsonpath "$.mocks[0].name" == "{{visitor_mock}}"

GET {{signin_url}}/_rustybin/settings
Authorization: Bearer {{operator_token}}
HTTP 200
# ANCHOR_END: ssi_operator
