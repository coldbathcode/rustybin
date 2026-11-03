# Gateway scenarios (docs/src/gateway-scenarios.md): the catalogue, a caching
# run prepared, read, a check the egress policy refuses (the docs instance
# allows no gateway host), and the run ended; then a request size limit run
# whose check is refused for a test parameter out of its bounds.

# ANCHOR: list
GET {{base_url}}/_rustybin/scenarios
HTTP 200
[Asserts]
jsonpath "$.scenarios" count == 7
jsonpath "$.scenarios[*].id" contains "caching"
# ANCHOR_END: list

# ANCHOR: start
POST {{base_url}}/_rustybin/scenarios/caching/runs
Authorization: Bearer {{admin_token}}
HTTP 201
[Captures]
run: jsonpath "$.id"
[Asserts]
jsonpath "$.upstream" contains "/mock/scn-"
jsonpath "$.setup" count == 3
jsonpath "$.resources[0].kind" == "mock"
# ANCHOR_END: start

# ANCHOR: show
GET {{base_url}}/_rustybin/scenarios/caching/runs/{{run}}
HTTP 200
[Asserts]
jsonpath "$.checks" == 0
jsonpath "$.last_check" == null
# ANCHOR_END: show

# ANCHOR: check
POST {{base_url}}/_rustybin/scenarios/caching/runs/{{run}}/check
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"gateway_url": "http://127.0.0.1:9/catalog-api"}
HTTP 403
[Asserts]
jsonpath "$.error" contains "purpose scenario"
# ANCHOR_END: check

# ANCHOR: end
DELETE {{base_url}}/_rustybin/scenarios/caching/runs/{{run}}
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.removed[0].kind" == "mock"
# ANCHOR_END: end

# ANCHOR: parameters
POST {{base_url}}/_rustybin/scenarios/request_size_limit/runs
Authorization: Bearer {{admin_token}}
HTTP 201
[Captures]
size_run: jsonpath "$.id"
[Asserts]
jsonpath "$.request_path" == "/upload"
jsonpath "$.parameters[0].name" == "size_limit"
jsonpath "$.parameters[0].maximum" == 1048576

POST {{base_url}}/_rustybin/scenarios/request_size_limit/runs/{{size_run}}/check
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"gateway_url": "http://127.0.0.1:9/upload-api", "size_limit": 4194304}
HTTP 400
[Asserts]
jsonpath "$.error" contains "size_limit: 16 to 1048576"
# ANCHOR_END: parameters

DELETE {{base_url}}/_rustybin/scenarios/request_size_limit/runs/{{size_run}}
Authorization: Bearer {{admin_token}}
HTTP 200
