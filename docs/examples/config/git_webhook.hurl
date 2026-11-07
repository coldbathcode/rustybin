# The push webhook: off until git.webhook_secret is set.

# ANCHOR: git_webhook
POST {{base_url}}/_rustybin/git/webhook
{"ref": "refs/heads/main"}
HTTP 404
[Asserts]
jsonpath "$.error" contains "git.webhook_secret"
# ANCHOR_END: git_webhook
