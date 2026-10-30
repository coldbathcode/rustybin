# The whole instance as rustybin.yaml (docs/src/configuration-as-code.md), on
# the secured documentation instance: the schema, the export, a dry run, an
# apply, and the instance put back.

# ANCHOR: schema
GET {{secured_url}}/_rustybin/config/schema
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.properties.version.enum" includes 2
jsonpath "$.properties.secrets.description" contains "never values"
# ANCHOR_END: schema

# ANCHOR: export
GET {{secured_url}}/_rustybin/config/export
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
header "Content-Type" startsWith "application/yaml"
body contains "version: 2"
body contains "health:"
# ANCHOR_END: export

# ANCHOR: export_json
GET {{secured_url}}/_rustybin/config/export?format=json&secrets=omit
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.version" == 2
jsonpath "$.secrets" not exists
jsonpath "$.health.initial" == "healthy"
# ANCHOR_END: export_json

# ANCHOR: dry_run
POST {{secured_url}}/_rustybin/config/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "document": "version: 2\nhealth: {initial: unhealthy}\nsettings: {agents.default_ceiling: read-only}\n",
  "dry_run": true
}
HTTP 200
[Asserts]
jsonpath "$.dry_run" == true
jsonpath "$.summary" includes "health.initial: changed from \"healthy\" to \"unhealthy\""
jsonpath "$.summary" includes "settings.agents.default_ceiling: added (read-only)"
jsonpath "$.results.settings.set" includes "agents.default_ceiling"

GET {{secured_url}}/health
HTTP 200
# ANCHOR_END: dry_run

# ANCHOR: apply
POST {{secured_url}}/_rustybin/config/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "document": "version: 2\nhealth: {initial: unhealthy}\nsettings: {agents.default_ceiling: read-only}\n",
  "prune": false
}
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.results.health.initial" == "unhealthy"
jsonpath "$.results.settings.changed" includes "agents.default_ceiling"

GET {{secured_url}}/health
HTTP 503
# ANCHOR_END: apply

# Put the instance back: a document with prune clears the stored setting too.
POST {{secured_url}}/_rustybin/config/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "document": {"version": 2, "health": {"initial": "healthy"}, "settings": {}},
  "prune": true
}
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.results.settings.unset" includes "agents.default_ceiling"

GET {{secured_url}}/health
HTTP 200
