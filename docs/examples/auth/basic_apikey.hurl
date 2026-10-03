# Basic auth and API keys (docs/src/reference/auth.md)

# ANCHOR: basic
GET {{base_url}}/auth/basic-auth
[BasicAuth]
basic: password
HTTP 200
[Asserts]
jsonpath "$.authenticated" == true
jsonpath "$.auth_type" == "basic-auth"
jsonpath "$.username" == "basic"
# ANCHOR_END: basic

# ANCHOR: basic_custom
POST {{base_url}}/auth/basic-auth/alice/s3cret
Authorization: Basic YWxpY2U6czNjcmV0
HTTP 200
[Asserts]
jsonpath "$.username" == "alice"
# ANCHOR_END: basic_custom

# ANCHOR: basic_fail
GET {{base_url}}/auth/basic-auth
HTTP 401
[Asserts]
header "WWW-Authenticate" == "Basic realm=\"rustybin\", charset=\"UTF-8\""
jsonpath "$.authenticated" == false
# ANCHOR_END: basic_fail

# ANCHOR: apikey
GET {{base_url}}/auth/api-key
apikey: my-key
HTTP 200
[Asserts]
jsonpath "$.auth_type" == "api-key"
jsonpath "$.header" == "apikey"
# ANCHOR_END: apikey

# ANCHOR: apikey_custom
GET {{base_url}}/auth/api-key/X-Token/s3cret
x-token: s3cret
HTTP 200

GET {{base_url}}/auth/api-key/X-Token/s3cret
X-Token: wrong
HTTP 401
# ANCHOR_END: apikey_custom
