# Control-plane security (docs/src/concepts/control-plane-security.md).
# Runs against the secured instance of docs/examples/run.sh:
# RUSTYBIN_HOSTED_MODE=true (control auth jwt), audience docs-secured.

# ANCHOR: no_token
GET {{secured_url}}/_rustybin/status
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Bearer realm=\"rustybin-control\""
jsonpath "$.reason" == "missing_token"
jsonpath "$.control_auth" == "jwt"
# ANCHOR_END: no_token

# ANCHOR: open_routes
GET {{secured_url}}/_rustybin/ready
HTTP 200
[Asserts]
jsonpath "$.status" == "ready"

GET {{secured_url}}/echo
HTTP 200

GET {{secured_url}}/ui/
HTTP 200
[Asserts]
body contains "<meta name=\"rustybin-control-auth\" content=\"jwt\">"
body contains "<meta name=\"rustybin-console-title\" content=\"Docs demo\">"
# ANCHOR_END: open_routes

# ANCHOR: console_token
GET {{secured_url}}/_rustybin/status
Authorization: Bearer {{console_token}}
HTTP 200
[Asserts]
jsonpath "$.control_auth" == "jwt"
jsonpath "$.console.title" == "Docs demo"
jsonpath "$.console.backlink" == "https://portal.example.com/instances/docs-secured"
# ANCHOR_END: console_token

# ANCHOR: console_cannot_mutate
DELETE {{secured_url}}/_rustybin/requests
Authorization: Bearer {{console_token}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "insufficient_scope"
jsonpath "$.scopes" contains "console"
# ANCHOR_END: console_cannot_mutate

# ANCHOR: inspector_scope
GET {{secured_url}}/_rustybin/requests?limit=5
Authorization: Bearer {{inspector_token}}
HTTP 200
[Asserts]
jsonpath "$.requests" isCollection

GET {{secured_url}}/_rustybin/config
Authorization: Bearer {{inspector_token}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "insufficient_scope"
# ANCHOR_END: inspector_scope

# ANCHOR: refused_tokens
GET {{secured_url}}/_rustybin/status
Authorization: Bearer {{expired_token}}
HTTP 401
[Asserts]
jsonpath "$.reason" == "expired"

GET {{secured_url}}/_rustybin/status
Authorization: Bearer {{other_audience_token}}
HTTP 401
[Asserts]
jsonpath "$.reason" == "invalid_audience"
# ANCHOR_END: refused_tokens

# ANCHOR: admin_token
DELETE {{secured_url}}/_rustybin/requests
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: admin_token

# ANCHOR: metrics
GET {{secured_url}}/_rustybin/metrics
Authorization: Bearer {{console_token}}
HTTP 200
[Asserts]
header "Content-Type" startsWith "text/plain; version=0.0.4"
body contains "rustybin_requests_total{route=\"/echo\",method=\"GET\",status_class=\"2xx\"}"
body contains "rustybin_build_info{version="
# ANCHOR_END: metrics

# ANCHOR: usage_build
GET {{secured_url}}/_rustybin/usage
Authorization: Bearer {{console_token}}
HTTP 200
[Asserts]
jsonpath "$.instance_id" == "docs-secured"
jsonpath "$.build.version" exists
jsonpath "$.control_auth" == "jwt"
# ANCHOR_END: usage_build
