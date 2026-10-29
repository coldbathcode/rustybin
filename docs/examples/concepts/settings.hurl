# Instance settings and the egress policy (docs/src/configuration.md,
# docs/src/console.md, docs/src/concepts/control-plane-security.md).
# The instance has an admin token and no RUSTYBIN_STATE_DIR: changes last
# until it restarts (persisted is false).

# ANCHOR: get
GET {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.persisted" == false
jsonpath "$.locked" == false
jsonpath "$.secrets_key" == "process"
jsonpath "$.settings[?(@.key == 'console.title')].source" == "default"
jsonpath "$.settings[?(@.key == 'console.title')].env" == "RUSTYBIN_CONSOLE_TITLE"
jsonpath "$.settings[?(@.key == 'leads.webhook_token')].secret" == true
jsonpath "$.settings[?(@.key == 'leads.webhook_token')].value.set" == false
# ANCHOR_END: get

# ANCHOR: admin
GET {{base_url}}/_rustybin/settings
HTTP 401
# ANCHOR_END: admin

# ANCHOR: patch
PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{
  "values": { "console.title": "Payments team", "egress.allow": ["*.example.com", "10.0.0.0/8"] },
  "secrets": { "leads.webhook_token": "hook-secret-k3Qx" }
}
HTTP 200
[Asserts]
jsonpath "$.changed" count == 3
jsonpath "$.settings[?(@.key == 'console.title')].value" == "Payments team"
jsonpath "$.settings[?(@.key == 'console.title')].source" == "stored"
jsonpath "$.settings[2].key" == "egress.allow"
jsonpath "$.settings[2].value[0]" == "*.example.com"
# A secret comes back as set plus a hint, never as its value.
jsonpath "$.settings[?(@.key == 'leads.webhook_token')].value.set" == true
jsonpath "$.settings[?(@.key == 'leads.webhook_token')].value.hint" == "k3Qx"
body not contains "hook-secret"
# ANCHOR_END: patch

# ANCHOR: title
GET {{base_url}}/_rustybin/status
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.console.title" == "Payments team"
jsonpath "$.settings.persisted" == false
# ANCHOR_END: title

# ANCHOR: invalid
PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{ "values": { "egress.allow": ["not a host"], "console.nope": 1 } }
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid settings"
jsonpath "$.problems" count == 2
# ANCHOR_END: invalid

# ANCHOR: egress
GET {{base_url}}/_rustybin/egress
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.allow" count == 2
jsonpath "$.allow_source" == "stored"
jsonpath "$.allow_loopback" == false
# Purposes in a fixed order: assist, mcp_relay, registry, git, a2a_push, leads, key_registry.
jsonpath "$.purposes[2].purpose" == "registry"
jsonpath "$.purposes[2].allow[0]" == "registry.modelcontextprotocol.io"
jsonpath "$.purposes[1].allow" count == 0
jsonpath "$.always_refused" includes "link-local addresses"
# ANCHOR_END: egress

# ANCHOR: delete
DELETE {{base_url}}/_rustybin/settings/console.title
X-Rustybin-Admin-Token: {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.changed" includes "console.title"
jsonpath "$.settings[?(@.key == 'console.title')].source" == "default"
# ANCHOR_END: delete

# Leave the instance as it was for the other examples.
PATCH {{base_url}}/_rustybin/settings
X-Rustybin-Admin-Token: {{admin_token}}
Content-Type: application/json
{ "values": { "egress.allow": null }, "secrets": { "leads.webhook_token": null } }
HTTP 200
