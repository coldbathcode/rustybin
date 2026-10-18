# Self-service mocks (docs/src/mocks/self-service.md), against the docs
# instance in public mode with RUSTYBIN_MOCKS_SELF_SERVICE=true, the mocks
# domain mocks.example, 10 minute periods and one anonymous visit per session.

# ANCHOR: ss_create
POST {{ss_url}}/_rustybin/mocks
X-Rustybin-Session: docs-visitor-a
{
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Orders", "version": "1"},
    "paths": {
      "/orders/{id}": {"get": {"operationId": "getOrder",
        "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}],
        "responses": {"200": {"description": "ok", "content": {"application/json": {
          "schema": {"type": "object", "properties": {"id": {"type": "integer"}}}}}}}}},
      "/receipt": {"get": {"operationId": "getReceipt",
        "responses": {"200": {"description": "ok", "content": {"text/html": {"example": "<h1>Thanks</h1>"}}}}}}
    }
  }
}
HTTP 201
[Captures]
mock_token: jsonpath "$.manage_token"
mock_name: jsonpath "$.name"
[Asserts]
jsonpath "$.name" matches /^orders-[a-z2-7]{6}$/
jsonpath "$.owner" == "visit"
jsonpath "$.manage_token" startsWith "rbm_"
jsonpath "$.url" contains ".mocks.example"
jsonpath "$.visit.expires_in_secs" >= 595
jsonpath "$.visit.next_extension" == "free"
# ANCHOR_END: ss_create

# ANCHOR: ss_call
GET {{ss_url}}/orders/7
Host: {{mock_name}}.mocks.example
HTTP 200
[Asserts]
jsonpath "$.id" == 7
header "X-Content-Type-Options" == "nosniff"
header "Content-Security-Policy" contains "sandbox"
# ANCHOR_END: ss_call

# ANCHOR: ss_html_as_text
GET {{ss_url}}/receipt
Host: {{mock_name}}.mocks.example
HTTP 200
[Asserts]
header "Content-Type" == "text/plain; charset=utf-8"
body == "<h1>Thanks</h1>"
# ANCHOR_END: ss_html_as_text

# ANCHOR: ss_main_host
GET {{ss_url}}/mock/{{mock_name}}/orders/7
HTTP 404
[Asserts]
jsonpath "$.url" == "https://{{mock_name}}.mocks.example"
# ANCHOR_END: ss_main_host

# ANCHOR: ss_only_mocks
GET {{ss_url}}/_rustybin/mocks
Host: {{mock_name}}.mocks.example
HTTP 404
# ANCHOR_END: ss_only_mocks

# ANCHOR: ss_stranger
GET {{ss_url}}/_rustybin/mocks/{{mock_name}}
HTTP 404
# ANCHOR_END: ss_stranger

# ANCHOR: ss_owner
GET {{ss_url}}/_rustybin/mocks/{{mock_name}}
X-Rustybin-Mock-Token: {{mock_token}}
HTTP 200
[Asserts]
jsonpath "$.can_manage" == true
jsonpath "$.yours" == true
# ANCHOR_END: ss_owner

# ANCHOR: ss_visit
GET {{ss_url}}/_rustybin/mocks/visit
X-Rustybin-Mock-Token: {{mock_token}}
HTTP 200
[Asserts]
jsonpath "$.minutes_used" == 10
jsonpath "$.minutes_allowed" == 60
jsonpath "$.extendable" == true
jsonpath "$.mocks" count == 1
jsonpath "$.mock_cap" == 3
# ANCHOR_END: ss_visit

# ANCHOR: ss_extend
POST {{ss_url}}/_rustybin/mocks/visit/extend
X-Rustybin-Mock-Token: {{mock_token}}
HTTP 200
[Asserts]
jsonpath "$.minutes_used" == 20
jsonpath "$.expires_in_secs" >= 1190
# ANCHOR_END: ss_extend

# ANCHOR: ss_email_required
POST {{ss_url}}/_rustybin/mocks
X-Rustybin-Session: docs-visitor-a
{"spec": {"openapi": "3.0.3", "info": {"title": "Second", "version": "1"}, "paths": {}}}
HTTP 403
[Asserts]
jsonpath "$.error" == "rustybin_email_required"
jsonpath "$.privacy_url" == "https://example.com/privacy"
jsonpath "$.consent" == "checkbox"
# ANCHOR_END: ss_email_required

# ANCHOR: ss_email_visit
POST {{ss_url}}/_rustybin/mocks/visit
X-Rustybin-Session: docs-visitor-a
{"email": "ada@example.com", "marketing_consent": true}
HTTP 201
[Captures]
second_token: jsonpath "$.manage_token"
[Asserts]
jsonpath "$.visit.email_given" == true
jsonpath "$.visit.minutes_allowed" == 360
# ANCHOR_END: ss_email_visit

# ANCHOR: ss_operator
GET {{ss_url}}/_rustybin/mocks/visits
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.count" == 2
jsonpath "$.visitor_mocks" == 1
jsonpath "$.visits[0].owner" startsWith "v:"
# ANCHOR_END: ss_operator

# ANCHOR: ss_pause
POST {{ss_url}}/_rustybin/mocks/visits/pause
Authorization: Bearer {{admin_token}}
HTTP 200

POST {{ss_url}}/_rustybin/mocks
X-Rustybin-Mock-Token: {{second_token}}
{"spec": {"openapi": "3.0.3", "info": {"title": "Third", "version": "1"}, "paths": {}}}
HTTP 503
[Asserts]
header "Retry-After" exists
jsonpath "$.error" == "rustybin_self_service_paused"

POST {{ss_url}}/_rustybin/mocks/visits/resume
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: ss_pause

# ANCHOR: ss_end
DELETE {{ss_url}}/_rustybin/mocks/visit
X-Rustybin-Mock-Token: {{mock_token}}
HTTP 200
[Asserts]
jsonpath "$.ended" == true
jsonpath "$.deleted" count == 1
# ANCHOR_END: ss_end

GET {{ss_url}}/orders/7
Host: {{mock_name}}.mocks.example
HTTP 404
