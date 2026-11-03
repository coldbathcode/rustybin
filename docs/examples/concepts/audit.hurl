# The audit log (docs/src/audit-log.md). Runs against the password instance
# of docs/examples/run.sh (rustybin / rustybin) and leaves it as it found it
# (the log itself keeps what happened).

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
admin: jsonpath "$.token"

# ANCHOR: change
# A change: every mutating control route is an entry, with the names the
# path and the handler give (here the project).
PUT {{password_url}}/_rustybin/projects/docs-audit
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "description": "Audited" }
HTTP 201
# ANCHOR_END: change

# ANCHOR: list
GET {{password_url}}/_rustybin/audit?target=project:docs-audit&limit=5
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.entries[0].action" == "PUT /_rustybin/projects/{name}"
jsonpath "$.entries[0].event" == "create_project"
jsonpath "$.entries[0].via" == "api"
jsonpath "$.entries[0].actor.kind" == "user"
jsonpath "$.entries[0].actor.name" == "rustybin"
jsonpath "$.entries[0].targets[0].kind" == "project"
jsonpath "$.entries[0].targets[0].name" == "docs-audit"
jsonpath "$.entries[0].result.status" == 201
jsonpath "$.entries[0].result.outcome" == "ok"
jsonpath "$.log.capacity" == 10000
# ANCHOR_END: list

# ANCHOR: signin
# A failed sign-in names the user only when one has that name: this one
# does not, so the entry's actor is anonymous (and the name is not kept).
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-nobody", "password": "not-the-password" }
HTTP 401

GET {{password_url}}/_rustybin/audit?action=login&result=401&limit=1
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.entries[0].event" == "login"
jsonpath "$.entries[0].actor.kind" == "anonymous"
jsonpath "$.entries[0].actor.name" not exists
jsonpath "$.entries[0].result.reason" == "invalid_credentials"
# ANCHOR_END: signin

# A viewer, to be refused.
POST {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "name": "docs-vera", "roles": ["viewer"] }
HTTP 201
[Captures]
invitation: jsonpath "$.invitation.token"
vera_id: jsonpath "$.user.id"

POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "vera-secret-Lm4p" }
HTTP 200
[Captures]
vera: jsonpath "$.token"

# ANCHOR: refused
# Every refusal that names a permission is an entry, reads included.
PATCH {{password_url}}/_rustybin/settings
Authorization: Bearer {{vera}}
Content-Type: application/json
{ "values": { "console.title": "Vera's" } }
HTTP 403

GET {{password_url}}/_rustybin/audit?result=refused&actor=docs-vera&limit=1
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.entries[0].action" == "PATCH /_rustybin/settings"
jsonpath "$.entries[0].actor.name" == "docs-vera"
jsonpath "$.entries[0].result.status" == 403
jsonpath "$.entries[0].result.permission" == "settings.write"
# ANCHOR_END: refused

# ANCHOR: viewer
# Reading the log needs audit.read, which only the admin role grants.
GET {{password_url}}/_rustybin/audit
Authorization: Bearer {{vera}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "permission_required"
jsonpath "$.permission" == "audit.read"
# ANCHOR_END: viewer

# ANCHOR: settings
# How long the files are kept (settings.security): the key is in the entry,
# never the value.
PATCH {{password_url}}/_rustybin/settings
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "values": { "audit.retention_days": 90 } }
HTTP 200

GET {{password_url}}/_rustybin/audit?target=setting:audit.retention_days&limit=1
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.entries[0].action" == "PATCH /_rustybin/settings"
jsonpath "$.log.retention_days" == 90
# ANCHOR_END: settings

# Leave the instance as it was.
DELETE {{password_url}}/_rustybin/settings/audit.retention_days
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/users/{{vera_id}}
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/projects/docs-audit
Authorization: Bearer {{admin}}
HTTP 200

POST {{password_url}}/_rustybin/logout
Authorization: Bearer {{admin}}
HTTP 200
