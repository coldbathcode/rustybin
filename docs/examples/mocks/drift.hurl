# Contract drift (docs/src/mocks/drift.md): no check kept yet, a check without
# an upstream refused, the mock's drift.upstream saved to the draft, a check
# the outbound policy refuses (the docs instance allows no outbound host), and
# a variable that is not given refused before anything is sent.

# ANCHOR: runs
GET {{base_url}}/_rustybin/mocks/petstore/drift
HTTP 200
[Asserts]
jsonpath "$.mock" == "petstore"
jsonpath "$.runs" count == 0
jsonpath "$.max_runs" == 10
# ANCHOR_END: runs

# ANCHOR: no_upstream
POST {{base_url}}/_rustybin/mocks/petstore/drift
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{}
HTTP 400
[Asserts]
jsonpath "$.error" contains "drift.upstream"
# ANCHOR_END: no_upstream

# ANCHOR: variable
POST {{base_url}}/_rustybin/mocks/petstore/drift
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"upstream": "https://petstore.example.com/v1", "headers": {"Authorization": "Bearer \u007b\u007bTOKEN\u007d\u007d"}}
HTTP 400
[Asserts]
jsonpath "$.error" contains "variable TOKEN is not given"
# ANCHOR_END: variable

# ANCHOR: refused
POST {{base_url}}/_rustybin/mocks/petstore/drift
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"upstream": "https://petstore.example.com/v1"}
HTTP 403
[Asserts]
jsonpath "$.reason" == "egress_refused"
jsonpath "$.purpose" == "record"
# ANCHOR_END: refused
