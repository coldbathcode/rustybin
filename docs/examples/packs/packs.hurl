# Demo packs (docs/src/demo-packs.md): the example pack previewed, applied,
# used, listed, exported, refused a second time, torn down.

# ANCHOR: preview
POST {{base_url}}/_rustybin/packs/apply?dry_run=true
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,acme-pack.yaml;
HTTP 200
[Asserts]
jsonpath "$.dry_run" == true
jsonpath "$.project" == "acme"
jsonpath "$.would_create.mocks[0]" == "acme-orders"
jsonpath "$.would_create.storyboards[0]" == "acme-demo"
jsonpath "$.console_title" == "Acme demo"
# ANCHOR_END: preview

# Nothing is applied by a preview.
GET {{base_url}}/_rustybin/packs
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
body not contains "Acme Corp"
jsonpath "$.max_packs" == 20

# ANCHOR: apply
POST {{base_url}}/_rustybin/packs/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,acme-pack.yaml;
HTTP 201
[Asserts]
jsonpath "$.dry_run" == false
jsonpath "$.created.mocks[0]" == "acme-orders"
jsonpath "$.created.portal_items[0]" == "mock:acme-orders"
jsonpath "$.created.tests[0]" == "acme-order-1"
jsonpath "$.created.test_schedules[0]" == "acme-hourly"
# ANCHOR_END: apply

# ANCHOR: use
GET {{base_url}}/mock/acme-orders/orders/1
HTTP 200
[Asserts]
jsonpath "$.status" == "paid"

GET {{base_url}}/_rustybin/projects/acme
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.resources.mocks[0]" == "acme-orders"
jsonpath "$.resources.storyboards[0]" == "acme-demo"

POST {{base_url}}/_rustybin/storyboards/acme-demo/steps/2/run
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.ok" == true
# ANCHOR_END: use

# ANCHOR: list
GET {{base_url}}/_rustybin/packs
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.packs[0].name" == "acme"
jsonpath "$.packs[0].customer" == "Acme Corp"
jsonpath "$.packs[0].holds.mocks[0]" == "acme-orders"
# ANCHOR_END: list

# ANCHOR: taken
POST {{base_url}}/_rustybin/packs/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,acme-pack.yaml;
HTTP 409
[Asserts]
jsonpath "$.reason" == "name_taken"
jsonpath "$.collisions[0].kind" == "project"
jsonpath "$.collisions[1].name" == "acme-orders"
# ANCHOR_END: taken

# ANCHOR: refused
POST {{base_url}}/_rustybin/packs/apply
Authorization: Bearer {{admin_token}}
{
  "document": "pack: {name: sneaky}\nusers:\n  - {name: mallory, roles: [admin]}\n",
  "dry_run": true
}
HTTP 400
[Asserts]
jsonpath "$.error" contains "not users"
# ANCHOR_END: refused

# ANCHOR: export
GET {{base_url}}/_rustybin/packs/acme/export
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
header "Content-Type" contains "yaml"
body contains "customer: Acme Corp"
body contains "name: acme-orders"
body not contains "project:"
# ANCHOR_END: export

# ANCHOR: teardown_preview
DELETE {{base_url}}/_rustybin/packs/acme?dry_run=true
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.would_remove.mocks[0]" == "acme-orders"
jsonpath "$.would_remove.test_schedules[0]" == "acme-hourly"
jsonpath "$.console_title" == "would be restored"
# ANCHOR_END: teardown_preview

# ANCHOR: teardown
DELETE {{base_url}}/_rustybin/packs/acme
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.removed.storyboards[0]" == "acme-demo"
jsonpath "$.console_title" == "restored"

GET {{base_url}}/mock/acme-orders/orders/1
HTTP 404

GET {{base_url}}/_rustybin/projects/acme
Authorization: Bearer {{admin_token}}
HTTP 404
# ANCHOR_END: teardown
