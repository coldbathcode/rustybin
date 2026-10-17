# RUSTYBIN_REQUIRE_KEY on a normal instance (docs/src/concepts/shared-mode.md)
#
# These run against the fifth instance of docs/examples/run.sh ({{keyed_url}}):
# RUSTYBIN_REQUIRE_KEY=true with the same registry as the shared instance.

# ANCHOR: require_key
GET {{keyed_url}}/echo
HTTP 401
[Asserts]
jsonpath "$.error" == "rustybin_key_required"

GET {{keyed_url}}/echo
X-Rustybin-Key: {{alpha_key}}
HTTP 200

# The control plane keeps its own authentication (open on this instance).
GET {{keyed_url}}/_rustybin/requests
HTTP 200
[Asserts]
jsonpath "$.requests[0].key_id" == "kdocsalpha"
# ANCHOR_END: require_key
