# Projects: roles on part of an instance (docs/src/projects.md). Runs
# against the password instance of docs/examples/run.sh (rustybin /
# rustybin) and leaves it as it found it.

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
admin: jsonpath "$.token"

# ANCHOR: create
PUT {{password_url}}/_rustybin/projects/docs-payments
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "description": "The payments team" }
HTTP 201
[Asserts]
jsonpath "$.name" == "docs-payments"
jsonpath "$.created" == true
jsonpath "$.members" count == 0
# ANCHOR_END: create

# ANCHOR: invite
# A user with roles in the project only (no instance role): they see it
# and nothing else.
POST {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "name": "docs-pia", "roles": [], "projects": [{ "project": "docs-payments", "roles": ["editor"] }] }
HTTP 201
[Captures]
invitation: jsonpath "$.invitation.token"
[Asserts]
jsonpath "$.user.projects[0].project" == "docs-payments"
jsonpath "$.user.projects[0].roles[0]" == "editor"
# ANCHOR_END: invite

POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "pia-secret-k3Qx" }
HTTP 200
[Captures]
pia: jsonpath "$.token"

# ANCHOR: session
GET {{password_url}}/_rustybin/session
Authorization: Bearer {{pia}}
HTTP 200
[Asserts]
jsonpath "$.roles" count == 0
jsonpath "$.permissions" includes "instance.read"
jsonpath "$.permissions" not includes "mocks.write"
jsonpath "$.projects[0].project" == "docs-payments"
jsonpath "$.projects[0].permissions" includes "mocks.write"
# ANCHOR_END: session

# ANCHOR: mock
# Her mock goes to her one project (or name it: "project": "docs-payments").
POST {{password_url}}/_rustybin/mocks
Authorization: Bearer {{pia}}
Content-Type: application/json
{ "name": "docs-pay-api", "spec": { "openapi": "3.0.3", "info": { "title": "Payments", "version": "1" }, "paths": { "/payments": { "get": { "operationId": "listPayments", "responses": { "200": { "description": "ok" } } } } } } }
HTTP 201
[Asserts]
jsonpath "$.project" == "docs-payments"
# ANCHOR_END: mock

# ANCHOR: list
# Her list holds her project's mocks only; the instance's sample is not hers.
GET {{password_url}}/_rustybin/mocks
Authorization: Bearer {{pia}}
HTTP 200
[Asserts]
jsonpath "$.mocks" count == 1
jsonpath "$.mocks[0].name" == "docs-pay-api"
jsonpath "$.mocks[0].can_manage" == true

GET {{password_url}}/_rustybin/mocks/petstore
Authorization: Bearer {{pia}}
HTTP 404

# An admin filters by project.
GET {{password_url}}/_rustybin/mocks?project=docs-payments
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.mocks" count == 1
jsonpath "$.mocks[0].name" == "docs-pay-api"
# ANCHOR_END: list

# ANCHOR: instance
# Instance-wide things need instance roles.
GET {{password_url}}/_rustybin/settings
Authorization: Bearer {{pia}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "permission_required"
jsonpath "$.permission" == "settings.read"
# ANCHOR_END: instance

# ANCHOR: delete
# A project that holds resources is deleted only with move_to.
DELETE {{password_url}}/_rustybin/projects/docs-payments
Authorization: Bearer {{admin}}
HTTP 409
[Asserts]
jsonpath "$.reason" == "not_empty"
jsonpath "$.resources.mocks[0]" == "docs-pay-api"

DELETE {{password_url}}/_rustybin/projects/docs-payments?move_to=instance
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.deleted" == "docs-payments"
jsonpath "$.moved.mocks[0]" == "docs-pay-api"
jsonpath "$.moved.to" == "instance"
# ANCHOR_END: delete

# Clean up.
DELETE {{password_url}}/_rustybin/mocks/docs-pay-api
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/users/docs-pia
Authorization: Bearer {{admin}}
HTTP 200
