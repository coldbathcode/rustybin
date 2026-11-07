# A spec fetched on the server: the outbound policy decides, and the purpose
# has no default.

# ANCHOR: spec_fetch
# The host is not in RUSTYBIN_EGRESS_ALLOW, so nothing is fetched.
POST {{base_url}}/_rustybin/mocks/import/fetch
X-Rustybin-Admin-Token: {{admin_token}}
```json
{"url": "https://specs.example.com/orders.yaml"}
```
HTTP 403
[Asserts]
jsonpath "$.reason" == "egress_refused"
jsonpath "$.purpose" == "spec_fetch"
# ANCHOR_END: spec_fetch
