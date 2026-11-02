# Instance diagnostics (docs/src/troubleshooting.md), on the secured
# documentation instance: every check with its status, what was seen and the fix.

# ANCHOR: diagnostics
GET {{secured_url}}/_rustybin/diagnostics
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.status" matches "^(ok|warn|fail)$"
jsonpath "$.checks[*].id" contains "state_dir"
jsonpath "$.checks[*].id" contains "settings_file"
jsonpath "$.checks[*].id" contains "listeners"
jsonpath "$.checks[?(@.id == 'listeners')].status" contains "ok"
# ANCHOR_END: diagnostics

# ANCHOR: admin_only
GET {{secured_url}}/_rustybin/diagnostics
Authorization: Bearer {{console_token}}
HTTP 403
# ANCHOR_END: admin_only
