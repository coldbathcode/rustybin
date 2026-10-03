# Health toggle and instance identity (docs/src/reference/reliability.md)

# ANCHOR: health
GET {{base_url}}/health
HTTP 200
[Asserts]
jsonpath "$.status" == "healthy"
jsonpath "$.instance_id" == "docs-examples"
# ANCHOR_END: health

# ANCHOR: unhealthy
# The run.sh server has an admin token, so the toggles need it.
POST {{base_url}}/health/unhealthy
HTTP 401

POST {{base_url}}/health/unhealthy
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.healthy" == false

GET {{base_url}}/health
HTTP 503
[Asserts]
jsonpath "$.status" == "unhealthy"

# GET / stays 200: use it for platform liveness checks.
GET {{base_url}}/
HTTP 200
# ANCHOR_END: unhealthy

# ANCHOR: healthy
POST {{base_url}}/health/toggle
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.healthy" == true

POST {{base_url}}/health/healthy
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200

GET {{base_url}}/health
HTTP 200
# ANCHOR_END: healthy

# ANCHOR: identity
GET {{base_url}}/identity
HTTP 200
[Asserts]
jsonpath "$.instance_id" == "docs-examples"
jsonpath "$.hostname" exists
jsonpath "$.uptime_seconds" >= 0
jsonpath "$.request_count" >= 1
jsonpath "$.port.listener.scheme" == "http"
jsonpath "$.config.admin_token_configured" == true
jsonpath "$.request.remote_ip" == "127.0.0.1"
# ANCHOR_END: identity
