# Shared mode and API keys (docs/src/concepts/shared-mode.md)
#
# These run against the fourth instance of docs/examples/run.sh ({{shared_url}}):
# RUSTYBIN_SHARED_MODE=true with the registry written by _make-key-registry.sh
# (keys kdocsalpha "starter", kdocsbeta "free" with 3 requests a day, kdocsold
# revoked), RUSTYBIN_QUOTA_UPGRADE_URL=https://example.com/pricing and
# control-plane JWTs ({{key_inspector_token}} is bound to kdocsalpha).

# ANCHOR: ready
# Readiness waits for the first registry load; / never needs a key.
GET {{shared_url}}/_rustybin/ready
HTTP 200
[Asserts]
jsonpath "$.status" == "ready"
# ANCHOR_END: ready

# ANCHOR: key_required
GET {{shared_url}}/echo
HTTP 401
[Asserts]
jsonpath "$.error" == "rustybin_key_required"
# ANCHOR_END: key_required

# ANCHOR: key_header
GET {{shared_url}}/echo
X-Rustybin-Key: {{alpha_key}}
HTTP 200
[Asserts]
header "X-Rustybin-Plan" == "starter"
header "X-Rustybin-Quota-Remaining" exists
# ANCHOR_END: key_header

# ANCHOR: key_query
# When a client cannot set headers (browsers opening a WebSocket, webhooks):
GET {{shared_url}}/uuid?rb_key={{alpha_key}}
HTTP 200
[Asserts]
header "X-Rustybin-Plan" == "starter"
# ANCHOR_END: key_query

# ANCHOR: key_invalid
# Unknown, revoked and disabled keys are refused the same way.
GET {{shared_url}}/echo
X-Rustybin-Key: {{revoked_key}}
HTTP 401
[Asserts]
jsonpath "$.error" == "rustybin_key_invalid"
# ANCHOR_END: key_invalid

# ANCHOR: inspector
# The key itself reads its own captured requests; keys are redacted.
GET {{shared_url}}/_rustybin/requests?limit=2
X-Rustybin-Key: {{alpha_key}}
HTTP 200
[Asserts]
jsonpath "$.count" == 2
jsonpath "$.requests[0].key_id" == "kdocsalpha"
jsonpath "$.requests[1].key_id" == "kdocsalpha"
jsonpath "$.requests[0].path" == "/uuid"
jsonpath "$.requests[0].query" == "rb_key=rbk_...2b4c"
body not contains "{{alpha_key}}"
# ANCHOR_END: inspector

# ANCHOR: usage
GET {{shared_url}}/_rustybin/usage
X-Rustybin-Key: {{alpha_key}}
HTTP 200
[Asserts]
jsonpath "$.key_id" == "kdocsalpha"
jsonpath "$.plan" == "starter"
jsonpath "$.scope" == "key"
jsonpath "$.period.kind" == "day"
jsonpath "$.requests.limit" == 100000
jsonpath "$.requests.used" == 2
jsonpath "$.rate.rps" == 100
jsonpath "$.quota_store" == "local"
# ANCHOR_END: usage

# ANCHOR: jwt_bound
# A control-plane JWT with scope inspector and claim rb_key=kdocsalpha sees the same.
GET {{shared_url}}/_rustybin/requests
Authorization: Bearer {{key_inspector_token}}
HTTP 200
[Asserts]
jsonpath "$.count" == 2
jsonpath "$.requests[0].key_id" == "kdocsalpha"
jsonpath "$.requests[1].key_id" == "kdocsalpha"
# ANCHOR_END: jwt_bound

# ANCHOR: admin_only
# Everything else on the control plane needs the admin token.
GET {{shared_url}}/_rustybin/config
X-Rustybin-Key: {{alpha_key}}
HTTP 401
[Asserts]
jsonpath "$.error" == "rustybin_admin_required"

GET {{shared_url}}/_rustybin/config
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.shared_mode" == true
jsonpath "$.keys_source" == "file"
# ANCHOR_END: admin_only

# ANCHOR: quota
# kdocsbeta allows 3 requests a day: the 4th gets 429 until 00:00 UTC.
GET {{shared_url}}/uuid
X-Rustybin-Key: {{beta_key}}
[Options]
repeat: 3
HTTP 200

GET {{shared_url}}/uuid
X-Rustybin-Key: {{beta_key}}
HTTP 429
[Asserts]
header "Retry-After" toInt > 0
header "X-Rustybin-Limit" == "requests"
header "X-Rustybin-Plan" == "free"
jsonpath "$.error" == "rustybin_quota_exceeded"
jsonpath "$.quota" == "requests"
jsonpath "$.reset_at" matches /^\d{4}-\d{2}-\d{2}T00:00:00Z$/
jsonpath "$.upgrade_url" == "https://example.com/pricing"
# ANCHOR_END: quota

# ANCHOR: keys_usage
# Per-key totals of today for a platform collector (admin), paginated by key id.
GET {{shared_url}}/_rustybin/keys/usage?limit=1
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.day" matches /^\d{4}-\d{2}-\d{2}$/
jsonpath "$.keys[0].key_id" == "kdocsalpha"
jsonpath "$.keys[0].requests" == 2
jsonpath "$.keys[0].egress_bytes" > 0
jsonpath "$.next_after" == "kdocsalpha"

GET {{shared_url}}/_rustybin/keys/usage?limit=1&after=kdocsalpha
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.keys[0].key_id" == "kdocsbeta"
jsonpath "$.keys[0].requests" == 3
jsonpath "$.next_after" == null
# ANCHOR_END: keys_usage
