# Open and closed APIs on the dev portal, with a portal login
# (docs/src/portal.md). Runs against the password instance of
# docs/examples/run.sh (rustybin / rustybin) and leaves it as it found it:
# an operator closes a mock, invites a tester as a portal reader, the
# tester joins, reads the closed API's credentials, signs out and in again.

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
admin: jsonpath "$.token"

POST {{password_url}}/_rustybin/mocks
Authorization: Bearer {{admin}}
Content-Type: application/json
{
  "name": "docs-closed",
  "deploy": true,
  "security": { "enabled": true, "mode": "api_key", "api_key": { "header": "x-api-key", "keys": ["docs-closed-key"] } },
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Payments", "version": "1.0"},
    "paths": {"/payments": {"get": {"operationId": "listPayments",
      "responses": {"200": {"description": "Payments", "content": {"application/json": {"example": [{"id": "pay_1"}]}}}}}}}
  }
}
HTTP 201

# ANCHOR: close
PUT {{password_url}}/_rustybin/portal/items/mock:docs-closed
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "title": "Payments", "access": "closed" }
HTTP 201
[Asserts]
jsonpath "$.published.access" == "closed"
# ANCHOR_END: close

# ANCHOR: signed_out
GET {{password_url}}/portal/api/session
HTTP 200
[Asserts]
jsonpath "$.signed_in" == false
jsonpath "$.signin" == true

GET {{password_url}}/portal/api/catalog
HTTP 200
[Asserts]
jsonpath "$.items[0].title" == "Payments"
jsonpath "$.items[0].locked" == true
jsonpath "$.items[0].detail.operations[0].curl" contains "<api-key>"
body not contains "docs-closed-key"
# ANCHOR_END: signed_out

# ANCHOR: invite_reader
POST {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "name": "docs-tess", "display_name": "Tess Tester", "roles": ["portal_reader"] }
HTTP 201
[Captures]
tess_id: jsonpath "$.user.id"
invitation: jsonpath "$.invitation.token"
[Asserts]
jsonpath "$.invitation.url" contains "/portal/#/join/rbi_"
# ANCHOR_END: invite_reader

# ANCHOR: join
# The tester opens the link and chooses a password: signed in to the portal.
POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "tess-secret-k3Qx", "portal": true }
HTTP 200
[Asserts]
jsonpath "$.joined" == true
jsonpath "$.portal_only" == true
jsonpath "$.token" not exists
header "Set-Cookie" contains "rustybin_portal="
header "Set-Cookie" contains "HttpOnly"
header "Set-Cookie" contains "SameSite=Strict"
header "Set-Cookie" contains "Path=/portal"
# ANCHOR_END: join

# ANCHOR: signed_in
GET {{password_url}}/portal/api/session
HTTP 200
[Asserts]
jsonpath "$.signed_in" == true
jsonpath "$.user" == "docs-tess"
jsonpath "$.display_name" == "Tess Tester"
jsonpath "$.reader" == true

GET {{password_url}}/portal/api/catalog
HTTP 200
[Asserts]
jsonpath "$.items[0].locked" == false
body contains "docs-closed-key"

GET {{password_url}}/portal/api/items/mock:docs-closed/collections/curl
HTTP 200
[Asserts]
body contains "docs-closed-key"

# The cookie opens nothing outside /portal.
GET {{password_url}}/_rustybin/config
HTTP 401
# ANCHOR_END: signed_in

# ANCHOR: logout
POST {{password_url}}/portal/api/logout
HTTP 200
[Asserts]
jsonpath "$.signed_out" == true
jsonpath "$.ended" == true
header "Set-Cookie" contains "Max-Age=0"

GET {{password_url}}/portal/api/items/mock:docs-closed/collections/curl
HTTP 401
# ANCHOR_END: logout

# ANCHOR: portal_login
POST {{password_url}}/portal/api/login
Content-Type: application/json
{ "username": "docs-tess", "password": "tess-secret-k3Qx" }
HTTP 200
[Asserts]
jsonpath "$.signed_in" == true
jsonpath "$.reader" == true
header "Set-Cookie" contains "rustybin_portal="

POST {{password_url}}/portal/api/login
Content-Type: application/json
{ "username": "docs-tess", "password": "not-the-password" }
HTTP 401
[Asserts]
jsonpath "$.reason" == "invalid_credentials"
# ANCHOR_END: portal_login

# ANCHOR: console_login
# The console sign-in of a portal reader says so (the console sends it to the portal).
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-tess", "password": "tess-secret-k3Qx" }
HTTP 200
[Asserts]
jsonpath "$.portal_only" == true
jsonpath "$.portal_url" == "/portal/"
# ANCHOR_END: console_login

POST {{password_url}}/portal/api/logout
HTTP 200

DELETE {{password_url}}/_rustybin/users/{{tess_id}}
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/portal/items/mock:docs-closed
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/mocks/docs-closed
Authorization: Bearer {{admin}}
HTTP 200

POST {{password_url}}/_rustybin/logout
Authorization: Bearer {{admin}}
HTTP 200
