# Users and roles, with invitations (docs/src/users.md). Runs against the
# password instance of docs/examples/run.sh (rustybin / rustybin, after
# login.hurl put the default back) and leaves it as it found it.

POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "rustybin", "password": "rustybin" }
HTTP 200
[Captures]
admin: jsonpath "$.token"

# ANCHOR: invite
POST {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "name": "docs-alice", "display_name": "Alice Doe", "email": "alice@example.com", "roles": ["editor"] }
HTTP 201
[Captures]
invitation: jsonpath "$.invitation.token"
[Asserts]
jsonpath "$.user.name" == "docs-alice"
jsonpath "$.user.status" == "invited"
jsonpath "$.invitation.url" contains "/ui/#/join/rbi_"
jsonpath "$.invitation.token" startsWith "rbi_"
jsonpath "$.invitation.expires_at" exists
jsonpath "$.invitation.reset" == false
# ANCHOR_END: invite

# ANCHOR: list
GET {{password_url}}/_rustybin/users
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.users[?(@.name == 'rustybin')].first_admin" == true
jsonpath "$.users[?(@.name == 'docs-alice')].status" == "invited"
jsonpath "$.users[?(@.name == 'docs-alice')].invitation.expires_at" exists
jsonpath "$.open_invitations" == 1
body not contains "{{invitation}}"
# ANCHOR_END: list

# ANCHOR: join
# The person opens the link and chooses a password; the answer signs them in.
POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "alice-secret-k3Qx" }
HTTP 200
[Captures]
alice: jsonpath "$.token"
[Asserts]
jsonpath "$.user" == "docs-alice"
jsonpath "$.joined" == true
jsonpath "$.token_type" == "Bearer"

# The link works once.
POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{invitation}}", "password": "another-secret-1" }
HTTP 401
[Asserts]
jsonpath "$.reason" == "invalid_invitation"
# ANCHOR_END: join

# ANCHOR: me
GET {{password_url}}/_rustybin/me
Authorization: Bearer {{alice}}
HTTP 200
[Asserts]
jsonpath "$.name" == "docs-alice"
jsonpath "$.display_name" == "Alice Doe"
jsonpath "$.roles[0]" == "editor"
jsonpath "$.permissions" contains "mocks.write"
jsonpath "$.permissions" not contains "users.read"
jsonpath "$.sessions" == 1

PATCH {{password_url}}/_rustybin/me
Authorization: Bearer {{alice}}
Content-Type: application/json
{ "display_name": "Alice D.", "email": null }
HTTP 200
[Asserts]
jsonpath "$.display_name" == "Alice D."
jsonpath "$.email" == null

GET {{password_url}}/_rustybin/users
Authorization: Bearer {{alice}}
HTTP 403
[Asserts]
jsonpath "$.reason" == "permission_required"
jsonpath "$.permission" == "users.read"
# ANCHOR_END: me

# ANCHOR: my_password
PUT {{password_url}}/_rustybin/me/password
Authorization: Bearer {{alice}}
Content-Type: application/json
{ "current_password": "alice-secret-k3Qx", "new_password": "alice-secret-2-Pz" }
HTTP 200
[Captures]
alice: jsonpath "$.token"
[Asserts]
jsonpath "$.changed" == true
jsonpath "$.user" == "docs-alice"

GET {{password_url}}/_rustybin/me/sessions
Authorization: Bearer {{alice}}
HTTP 200
[Asserts]
jsonpath "$.count" == 1
jsonpath "$.current" exists
# ANCHOR_END: my_password

# ANCHOR: roles
GET {{password_url}}/_rustybin/roles
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.roles[*].name" contains "viewer"
jsonpath "$.roles[?(@.name == 'admin')].privileged" == true
jsonpath "$.roles[?(@.name == 'editor')].users" == 1
jsonpath "$.permissions[0].name" == "instance.read"

PUT {{password_url}}/_rustybin/roles/catalog-reader
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "description": "Reads mocks and traffic", "permissions": ["mocks.read", "traffic.read"] }
HTTP 200
[Asserts]
jsonpath "$.created" == true
jsonpath "$.privileged" == false
jsonpath "$.built_in" == false
# ANCHOR_END: roles

# ANCHOR: change_roles
PATCH {{password_url}}/_rustybin/users/docs-alice
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "roles": ["catalog-reader"] }
HTTP 200
[Asserts]
jsonpath "$.roles[0]" == "catalog-reader"

# The next request already has the new permissions.
GET {{password_url}}/_rustybin/me
Authorization: Bearer {{alice}}
HTTP 200
[Asserts]
jsonpath "$.permissions" count == 2

POST {{password_url}}/_rustybin/mocks
Authorization: Bearer {{alice}}
Content-Type: application/json
{ "name": "nope", "spec": "openapi: 3.0.3\ninfo: {title: x, version: '1'}\npaths: {}\n" }
HTTP 403
[Asserts]
jsonpath "$.permission" == "mocks.write"
# ANCHOR_END: change_roles

# ANCHOR: guard_rails
PATCH {{password_url}}/_rustybin/users/rustybin
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "roles": ["viewer"] }
HTTP 403
[Asserts]
jsonpath "$.reason" == "own_roles"

DELETE {{password_url}}/_rustybin/users/rustybin
Authorization: Bearer {{admin}}
HTTP 409
[Asserts]
jsonpath "$.reason" == "first_admin"

DELETE {{password_url}}/_rustybin/roles/catalog-reader
Authorization: Bearer {{admin}}
HTTP 409
[Asserts]
jsonpath "$.reason" == "role_in_use"
# ANCHOR_END: guard_rails

# ANCHOR: reset
POST {{password_url}}/_rustybin/users/docs-alice/invite
Authorization: Bearer {{admin}}
HTTP 201
[Captures]
reset: jsonpath "$.invitation.token"
[Asserts]
jsonpath "$.invitation.reset" == true

# The old password works until the link is accepted.
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-alice", "password": "alice-secret-2-Pz" }
HTTP 200

POST {{password_url}}/_rustybin/join
Content-Type: application/json
{ "token": "{{reset}}", "password": "alice-secret-3-Qw" }
HTTP 200
[Asserts]
jsonpath "$.reset" == true

# Accepted: the old password and the old sessions are gone.
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-alice", "password": "alice-secret-2-Pz" }
HTTP 401

GET {{password_url}}/_rustybin/me
Authorization: Bearer {{alice}}
HTTP 401
# ANCHOR_END: reset

# ANCHOR: sessions
GET {{password_url}}/_rustybin/users/docs-alice/sessions
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.count" == 1

DELETE {{password_url}}/_rustybin/users/docs-alice/sessions
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.ended" == 1
# ANCHOR_END: sessions

# ANCHOR: disable
PATCH {{password_url}}/_rustybin/users/docs-alice
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "disabled": true }
HTTP 200
[Asserts]
jsonpath "$.status" == "disabled"

# A disabled user gets the answer of a wrong password.
POST {{password_url}}/_rustybin/login
Content-Type: application/json
{ "username": "docs-alice", "password": "alice-secret-3-Qw" }
HTTP 401
[Asserts]
jsonpath "$.reason" == "invalid_credentials"
# ANCHOR_END: disable

# ANCHOR: document
# rustybin.yaml carries users: and roles: (never a password): a user the
# document creates is invited, and the answer has the link.
POST {{password_url}}/_rustybin/config/apply
Authorization: Bearer {{admin}}
Content-Type: application/json
{ "document": "version: 2\nroles:\n- name: catalog-reader\n  description: Reads mocks and traffic\n  permissions: [mocks.read, traffic.read]\nusers:\n- name: docs-bob\n  display_name: Bob\n  roles: [catalog-reader]\n" }
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.changes[*].key" contains "docs-bob"
jsonpath "$.results.users.created[0]" == "docs-bob"
jsonpath "$.results.users.invitations[0].url" contains "/ui/#/join/rbi_"

GET {{password_url}}/_rustybin/config/export?format=json
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.users[?(@.name == 'docs-bob')].roles[0]" == "catalog-reader"
jsonpath "$.roles[0].name" == "catalog-reader"
body not contains "argon2"
# ANCHOR_END: document

# Clean up: the users, then the role nobody holds any more.
DELETE {{password_url}}/_rustybin/users/docs-alice
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/users/docs-bob
Authorization: Bearer {{admin}}
HTTP 200

DELETE {{password_url}}/_rustybin/roles/catalog-reader
Authorization: Bearer {{admin}}
HTTP 200
[Asserts]
jsonpath "$.deleted" == "catalog-reader"
