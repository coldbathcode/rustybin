# Source control: a diverged branch's ways out, branches per environment,
# signed commits and verified pulls (docs/src/source-control.md), on the first documentation
# instance. It has no state directory, so the calls that need a working copy
# answer 409 and say so; docs/examples/config/source-control.sh runs the same
# calls against a bare repository.

# ANCHOR: status_signing
# The status says whether commits are signed and, on a diverged branch, what
# differs (null here: nothing has diverged).
GET {{base_url}}/_rustybin/git
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.signing.enabled" == false
jsonpath "$.signing.key_set" == false
jsonpath "$.divergence" == null
# ANCHOR_END: status_signing

# ANCHOR: resolve_preview
# What a way out would change, before choosing it.
POST {{base_url}}/_rustybin/git/resolve
Authorization: Bearer {{admin_token}}
{"with": "repository", "dry_run": true}
HTTP 409
[Asserts]
jsonpath "$.error" contains "RUSTYBIN_STATE_DIR"
# ANCHOR_END: resolve_preview

# ANCHOR: resolve_pull_request
# The instance's side to a new branch, for a pull request at the git host.
POST {{base_url}}/_rustybin/git/resolve
Authorization: Bearer {{admin_token}}
{"with": "pull_request", "branch": "review/orders"}
HTTP 409
[Asserts]
jsonpath "$.error" contains "RUSTYBIN_STATE_DIR"
# ANCHOR_END: resolve_pull_request

# Anything else is not a way out.
POST {{base_url}}/_rustybin/git/resolve
Authorization: Bearer {{admin_token}}
{"with": "merge"}
HTTP 400
[Asserts]
jsonpath "$.error" contains "pull_request"

# ANCHOR: branches
GET {{base_url}}/_rustybin/git/branches
Authorization: Bearer {{admin_token}}
HTTP 409
[Asserts]
jsonpath "$.error" contains "RUSTYBIN_STATE_DIR"
# ANCHOR_END: branches

# ANCHOR: switch_preview
# What switching to a branch would change, validated like the apply.
POST {{base_url}}/_rustybin/git/branch
Authorization: Bearer {{admin_token}}
{"branch": "staging", "dry_run": true, "prune": true}
HTTP 409
[Asserts]
jsonpath "$.error" contains "RUSTYBIN_STATE_DIR"
# ANCHOR_END: switch_preview

# ANCHOR: branch_start
# A new branch from this instance's configuration.
POST {{base_url}}/_rustybin/git/branch
Authorization: Bearer {{admin_token}}
{"branch": "staging", "create": true, "message": "Start staging"}
HTTP 409
[Asserts]
jsonpath "$.error" contains "RUSTYBIN_STATE_DIR"
# ANCHOR_END: branch_start

# A name git would read as an option is refused before anything runs.
POST {{base_url}}/_rustybin/git/branch
Authorization: Bearer {{admin_token}}
{"branch": "--upload-pack=x"}
HTTP 400
[Asserts]
jsonpath "$.error" contains "branch name"

# The branch the instance is on is not a switch.
POST {{base_url}}/_rustybin/git/branch
Authorization: Bearer {{admin_token}}
{"branch": "main"}
HTTP 409
[Asserts]
jsonpath "$.error" contains "already on main"

# ANCHOR: signing_key
# The signing key's public half and fingerprint (none yet), never the key.
GET {{base_url}}/_rustybin/git/signing-key
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.signing.enabled" == false
jsonpath "$.signing.public" == null
jsonpath "$.signing.fingerprint" == null
# ANCHOR_END: signing_key

# ANCHOR: signing_key_generate
POST {{base_url}}/_rustybin/git/signing-key
Authorization: Bearer {{admin_token}}
{"replace": false}
HTTP 409
[Asserts]
jsonpath "$.error" contains "RUSTYBIN_STATE_DIR"
# ANCHOR_END: signing_key_generate

# ANCHOR: status_verification
# The status says whether pulled commits are verified (off by default), who
# the allowed signers are and the last verified head.
GET {{base_url}}/_rustybin/git
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.verification.enabled" == false
jsonpath "$.verification.signers" count == 0
jsonpath "$.verification.verified_head" == null
jsonpath "$.verification.last_refusal" == null
# ANCHOR_END: status_verification

# ANCHOR: signers_refused
# An allowed signer is a name and a public key, nothing more: a line with an
# option (a certificate authority here) is refused, and nothing is stored.
PATCH {{base_url}}/_rustybin/settings
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "values": { "git.allowed_signers": ["ann cert-authority ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPcTdrsIh7YkANjaKyUYQYzJTCVVKgE9HtCjRkTmaiXX"] } }
HTTP 400
[Asserts]
jsonpath "$.error" == "invalid settings"
jsonpath "$.problems[0]" contains "not an allowed signer"
# ANCHOR_END: signers_refused

# Reading needs git.read, changing needs git.write: nobody without a
# credential on an instance with an admin token.
POST {{base_url}}/_rustybin/git/branch
{"branch": "staging"}
HTTP 401
