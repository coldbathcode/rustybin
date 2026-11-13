#!/usr/bin/env bash
# Source control (docs/src/source-control.md) against a bare repository in a
# temporary directory: an instance with a state directory commits and pushes
# its configuration, a second instance starts from the repository
# (RUSTYBIN_CONFIG_GIT) and holds the same mock; then a diverged branch and its
# ways out (a side, a pull request, and the git host's API opening it: the
# host is a stand-in here, a Rustybin mock on loopback, never a real host),
# a branch per environment, signed commits and verified pulls. Run through
# docs/examples/run.sh, which exports RUSTYBIN_BIN and BASE.
set -euo pipefail
BIN="${RUSTYBIN_BIN:-$(command -v rustybin || true)}"
[[ -x "$BIN" ]] || { echo "set RUSTYBIN_BIN to a rustybin binary" >&2; exit 1; }
command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 77; }
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }
work="$(mktemp -d)"
# Two ports above the runner's, skipping any that something else holds.
busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
PORT=$(( ${BASE##*:} + 11 ))
while busy "$PORT"; do PORT=$(( PORT + 1 )); done
PORT2=$(( PORT + 1 ))
while busy "$PORT2"; do PORT2=$(( PORT2 + 1 )); done
ADMIN="docs-git-admin"
HOST_TOKEN="docs-git-host-token-123456"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; wait "${pids[@]}" 2>/dev/null || true; rm -rf "$work"' EXIT
cd "$work"

# ANCHOR: bare_repo
git init --quiet --bare -b main config.git
# ANCHOR_END: bare_repo

# ANCHOR: start_instance
# An instance with a state directory (the working copy lives in <state>/repo),
# the repository configured and, for a local repository, the loopback
# allowance of the outbound policy.
RUSTYBIN_HTTP_PORT=$PORT RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open RUSTYBIN_ADMIN_TOKEN=$ADMIN \
  RUSTYBIN_STATE_DIR="$work/state-a" RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true \
  RUSTYBIN_CONFIG_GIT="file://$work/config.git" \
  "$BIN" &
# ANCHOR_END: start_instance
pids+=($!)
A="http://127.0.0.1:$PORT"
for _ in $(seq 1 100); do curl -fs -o /dev/null "$A/" && break; sleep 0.1; done

# ANCHOR: status
curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git"
# ANCHOR_END: status
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git")
expect "$out" '"initialised":true'
expect "$out" '"kind":"local"'

# Something to commit: a mock created through the API.
curl -s -o /dev/null -X POST "$A/_rustybin/mocks" -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"name":"orders","spec":{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok"}}}}}}}'

# ANCHOR: commit_push
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"message": "Add the orders mock"}' "$A/_rustybin/git/commit"
curl -s -X POST -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/push"
# ANCHOR_END: commit_push
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/history")
expect "$out" 'Add the orders mock'
expect "$(git -C config.git ls-tree -r --name-only main)" 'rustybin/specs/orders.json'

# ANCHOR: second_instance
# A second instance starts from the repository: it clones and applies the
# document before serving, so it holds the orders mock too.
RUSTYBIN_HTTP_PORT=$PORT2 RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open RUSTYBIN_ADMIN_TOKEN=$ADMIN \
  RUSTYBIN_STATE_DIR="$work/state-b" RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true \
  RUSTYBIN_CONFIG_GIT="file://$work/config.git" RUSTYBIN_CONFIG_GIT_REQUIRED=true \
  "$BIN" &
# ANCHOR_END: second_instance
pids+=($!)
B="http://127.0.0.1:$PORT2"
for _ in $(seq 1 100); do curl -fs -o /dev/null "$B/" && break; sleep 0.1; done
out=$(curl -s "$B/_rustybin/mocks/orders")
expect "$out" '"name":"orders"'

# ANCHOR: pull_dry_run
# A pull with a dry run shows what the repository would change (nothing here).
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"apply": true, "dry_run": true, "prune": true}' "$B/_rustybin/git/pull"
# ANCHOR_END: pull_dry_run
out=$(curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"apply": true, "dry_run": true, "prune": true}' "$B/_rustybin/git/pull")
expect "$out" '"dry_run":true'
expect "$out" '"summary":\[\]'

# A checkpoint of the first instance, committed next to rustybin.yaml.
curl -s -o /dev/null -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"name": "demo-start"}' "$A/_rustybin/checkpoints"
out=$(
# ANCHOR: commit_checkpoint
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"message": "The demo-start checkpoint", "checkpoints": ["demo-start"]}' "$A/_rustybin/git/commit"
curl -s -X POST -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/push"
# ANCHOR_END: commit_checkpoint
)
echo "$out"
expect "$out" '"checkpoints":\["demo-start"\]'
expect "$(git -C config.git ls-tree -r --name-only main)" 'rustybin/checkpoints/demo-start.json'

out=$(
# ANCHOR: pull_checkpoints
# The second instance pulls: the answer lists the repository's checkpoints
# (a pull never restores one), and ?from=git shows what each holds.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"apply": true}' "$B/_rustybin/git/pull"
curl -s -H "Authorization: Bearer $ADMIN" "$B/_rustybin/checkpoints?from=git"
# ANCHOR_END: pull_checkpoints
)
echo "$out"
expect "$out" '"checkpoints":\["demo-start"\]'
expect "$out" '"from":"git"'
expect "$out" '"ok":true'

out=$(
# ANCHOR: restore_from_git
# What a restore would change, then the restore.
curl -s -X POST -H "Authorization: Bearer $ADMIN" \
  "$B/_rustybin/checkpoints/demo-start/restore?from=git&dry_run=true"
curl -s -X POST -H "Authorization: Bearer $ADMIN" \
  "$B/_rustybin/checkpoints/demo-start/restore?from=git"
# ANCHOR_END: restore_from_git
)
echo "$out"
expect "$out" '"dry_run":true'
expect "$out" '"dry_run":false'
expect "$out" '"from":"git"'
expect "$out" '"problems":\[\]'

# ANCHOR: deploy_key
# The deploy key for an ssh remote: none yet, then generated (the public half
# is what the repository gets; the private half stays in <state>/repo.key).
curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/key"
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{}' "$A/_rustybin/git/key"
# ANCHOR_END: deploy_key
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/key")
expect "$out" '"public":"ssh-ed25519 '
out=$(curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" -d '{}' "$A/_rustybin/git/key")
expect "$out" 'a deploy key exists'

# ANCHOR: connect
# A third instance with nothing but a state directory: the repository is
# configured through the settings, then connected (cloned) on request.
PORT3=$(( PORT2 + 1 ))
while busy "$PORT3"; do PORT3=$(( PORT3 + 1 )); done
RUSTYBIN_HTTP_PORT=$PORT3 RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open RUSTYBIN_ADMIN_TOKEN=$ADMIN \
  RUSTYBIN_STATE_DIR="$work/state-c" RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true \
  "$BIN" &
pids+=($!)
C="http://127.0.0.1:$PORT3"
for _ in $(seq 1 100); do curl -fs -o /dev/null "$C/" && break; sleep 0.1; done
curl -s -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d "{\"values\": {\"git.remote\": \"file://$work/config.git\"}}" "$C/_rustybin/settings" >/dev/null
curl -s -X POST -H "Authorization: Bearer $ADMIN" "$C/_rustybin/git/init"
# ANCHOR_END: connect
out=$(curl -s -X POST -H "Authorization: Bearer $ADMIN" "$C/_rustybin/git/init")
expect "$out" '"initialised":true'
expect "$out" '"remote_branch":true'
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$C/_rustybin/git/history")
expect "$out" 'Add the orders mock'

# A diverged branch: the first instance commits a second mock without pushing
# while someone else pushes to the repository directly.
curl -s -o /dev/null -X POST "$A/_rustybin/mocks" -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"name":"invoices","spec":{"openapi":"3.0.3","info":{"title":"Invoices","version":"1"},"paths":{"/invoices":{"get":{"operationId":"listInvoices","responses":{"200":{"description":"ok"}}}}}}}'
curl -s -o /dev/null -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"message": "Add the invoices mock"}' "$A/_rustybin/git/commit"
git clone --quiet "file://$work/config.git" elsewhere
echo "# notes" > elsewhere/README.md
git -C elsewhere add README.md
git -C elsewhere -c user.name=someone -c user.email=someone@example.invalid commit --quiet -m "Add a README"
git -C elsewhere push --quiet origin main

# The push answers 409 with the two ways out; this instance's copy wins.
out=$(
# ANCHOR: diverged
curl -s -w '%{http_code}' -X POST -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/push"
# ANCHOR_END: diverged
)
echo "$out"
expect "$out" '"conflict":true'
expect "$out" '409$'
out=$(
# ANCHOR: resolve
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"with": "instance", "message": "Keep the instance configuration"}' "$A/_rustybin/git/resolve"
# ANCHOR_END: resolve
)
echo "$out"
expect "$out" '"resolved":"instance"'
expect "$(git -C config.git ls-tree -r --name-only main)" 'rustybin/specs/invoices.json'
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git")
expect "$out" '"behind":0'

# Diverged again, and this time nobody's copy should simply win: the first
# instance commits a third mock while someone pushes to the repository.
curl -s -o /dev/null -X POST "$A/_rustybin/mocks" -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"name":"payments","spec":{"openapi":"3.0.3","info":{"title":"Payments","version":"1"},"paths":{"/payments":{"get":{"operationId":"listPayments","responses":{"200":{"description":"ok"}}}}}}}'
curl -s -o /dev/null -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"message": "Add the payments mock"}' "$A/_rustybin/git/commit"
git -C elsewhere pull --quiet origin main
echo "more notes" >> elsewhere/README.md
git -C elsewhere -c user.name=someone -c user.email=someone@example.invalid commit --quiet -am "More notes"
git -C elsewhere push --quiet origin main

out=$(
# ANCHOR: what_differs
# The status of a diverged branch says what differs: the commits of each
# side and what each way out would change, section by section.
curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git?fetch=true"
# ANCHOR_END: what_differs
)
expect "$out" '"divergence":{"ahead":1,"behind":1'
expect "$out" '"options":\["instance","repository","pull_request"\]'
expect "$out" 'Add the payments mock'
expect "$out" 'More notes'

out=$(
# ANCHOR: resolve_preview
# A preview of a way out changes nothing: here, what taking the repository's
# copy would do to this instance (the payments mock would go).
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"with": "repository", "dry_run": true}' "$A/_rustybin/git/resolve"
# ANCHOR_END: resolve_preview
)
echo "$out"
expect "$out" '"would_resolve":"repository"'
expect "$out" '"would_delete":\["payments"\]'
expect "$(curl -s "$A/_rustybin/mocks/payments")" '"name":"payments"'

out=$(
# ANCHOR: pull_request
# Neither side wins here: this instance's side goes to a new branch, and a
# person opens a pull request from it at the git host. Nothing is applied.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"with": "pull_request", "branch": "review/payments"}' "$A/_rustybin/git/resolve"
# ANCHOR_END: pull_request
)
echo "$out"
expect "$out" '"resolved":"pull_request"'
expect "$out" '"branch":"review/payments"'
expect "$out" '"base":"main"'
expect "$(git -C config.git log -1 --format=%s review/payments)" 'Add the payments mock'
expect "$(git -C config.git log -1 --format=%s main)" 'More notes'

# The git host's API can open the pull request. No real host is called here:
# a second Rustybin plays one, with a mock of the one call the instance makes.
PORT4=$(( PORT3 + 1 ))
while busy "$PORT4"; do PORT4=$(( PORT4 + 1 )); done
RUSTYBIN_HTTP_PORT=$PORT4 RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open RUSTYBIN_ADMIN_TOKEN=$ADMIN \
  "$BIN" &
pids+=($!)
H="http://127.0.0.1:$PORT4"
for _ in $(seq 1 100); do curl -fs -o /dev/null "$H/" && break; sleep 0.1; done
curl -s -o /dev/null -X POST "$H/_rustybin/mocks" -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"name":"githost","spec":{"openapi":"3.0.3","info":{"title":"A git host, for the example","version":"1"},"paths":{"/api/v3/repos/{owner}/{repo}/pulls":{"post":{"operationId":"openPullRequest","parameters":[{"name":"owner","in":"path","required":true,"schema":{"type":"string"}},{"name":"repo","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"201":{"description":"opened","content":{"application/json":{"example":{"number":7,"html_url":"https://git.example.com/acme/config/pull/7"}}}}}}}}}}'
HOST_API="$H/mock/githost/api/v3"
# ANCHOR: host_settings
# Which host, where its API is (only for a self-hosted one: github.com and
# gitlab.com are recognised from the repository's address), and a token
# that may open pull requests. The token is a secret: stored sealed, never
# shown again.
curl -s -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d "{\"values\": {\"git.host\": \"github\", \"git.host_api\": \"$HOST_API\"}, \"secrets\": {\"git.host_token\": \"$HOST_TOKEN\"}}" \
  "$A/_rustybin/settings"
# ANCHOR_END: host_settings
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git")
expect "$out" '"host":{"setting":"github","kind":"github"'
expect "$out" '"token_from":"git.host_token","ready":true'
out=$(
# ANCHOR: pull_request_open_preview
# What would be opened and where. Nothing is pushed and the host is not called.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"with": "pull_request", "branch": "review/payments-2", "open": true, "dry_run": true}' "$A/_rustybin/git/resolve"
# ANCHOR_END: pull_request_open_preview
)
echo "$out"
expect "$out" '"would_open":{"host":{"kind":"github"'
expect "$out" '"head":"review/payments-2"'
out=$(
# ANCHOR: pull_request_open
# Pushed, and the host is asked to open the pull request: one call.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"with": "pull_request", "branch": "review/payments-2", "open": true, "message": "Payments, for review"}' "$A/_rustybin/git/resolve"
# ANCHOR_END: pull_request_open
)
echo "$out"
expect "$out" '"opened":true'
expect "$out" '"untrusted_pull_request":{"number":7,"url":"https://git.example.com/acme/config/pull/7"}'
expect "$(git -C config.git log -1 --format=%s review/payments-2)" 'Add the payments mock'
case "$out" in *"$HOST_TOKEN"*) echo "the token is in the answer" >&2; exit 1 ;; esac
# The stand-in saw one call, with the two branches and the token as a bearer.
seen=$(curl -s -H "Authorization: Bearer $ADMIN" "$H/_rustybin/mocks/githost/requests")
expect "$seen" 'review/payments-2'
# A host that cannot be asked (none) is said before anything is pushed.
curl -s -o /dev/null -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"values": {"git.host": "none"}}' "$A/_rustybin/settings"
out=$(curl -s -w '%{http_code}' -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"with": "pull_request", "branch": "review/payments-3", "open": true}' "$A/_rustybin/git/resolve")
expect "$out" '"reason":"host_not_set"'
expect "$out" '409$'
if git -C config.git rev-parse --verify --quiet refs/heads/review/payments-3 >/dev/null; then
  echo "a refused call pushed a branch" >&2; exit 1
fi

# The person merges the pull request at the host (a merge commit).
git -C elsewhere fetch --quiet origin
git -C elsewhere -c user.name=someone -c user.email=someone@example.invalid merge --quiet --no-ff \
  -m "Merge review/payments" origin/review/payments
git -C elsewhere push --quiet origin main
out=$(
# ANCHOR: after_merge
# The merge holds this instance's commits, so an ordinary pull fast-forwards.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"apply": true, "prune": true}' "$A/_rustybin/git/pull"
# ANCHOR_END: after_merge
)
echo "$out"
expect "$out" '"fast_forwarded":true'
expect "$out" '"ahead":0'
expect "$(curl -s "$A/_rustybin/mocks/payments")" '"name":"payments"'

# A branch per environment. The first instance becomes staging: a new branch
# from what it holds.
curl -s -o /dev/null -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"values": {"console.title": "Staging"}}' "$A/_rustybin/settings"
out=$(
# ANCHOR: branch_start
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"branch": "staging", "create": true, "message": "Start staging"}' "$A/_rustybin/git/branch"
# ANCHOR_END: branch_start
)
echo "$out"
expect "$out" '"created":true'
expect "$out" '"branch":"staging"'
expect "$(git -C config.git log -1 --format=%s staging)" 'Start staging'
out=$(
# ANCHOR: branches
curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/branches"
# ANCHOR_END: branches
)
echo "$out"
expect "$out" '"branch":"staging"'
expect "$out" '"name":"main"'
expect "$out" '"name":"review/payments"'

out=$(
# ANCHOR: switch_preview
# The second instance is on main. What would moving it to staging change?
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"branch": "staging", "dry_run": true, "prune": true}' "$B/_rustybin/git/branch"
# ANCHOR_END: switch_preview
)
echo "$out"
expect "$out" '"dry_run":true'
expect "$out" 'instance.title: set to'
expect "$(curl -s -H "Authorization: Bearer $ADMIN" "$B/_rustybin/git")" '"branch":"main"'
out=$(
# ANCHOR: switch
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"branch": "staging", "prune": true}' "$B/_rustybin/git/branch"
# ANCHOR_END: switch
)
echo "$out"
expect "$out" '"switched":true'
expect "$out" '"from":"main"'
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$B/_rustybin/git")
expect "$out" '"branch":"staging"'
expect "$(curl -s "$B/_rustybin/mocks/payments")" '"name":"payments"'

# Signed commits: a signing key of its own, then git.sign.
out=$(
# ANCHOR: signing_key
# Generate the signing key: the answer is its public half and fingerprint,
# to register at the git host as a signing key. Then turn signing on.
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{}' "$A/_rustybin/git/signing-key"
curl -s -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"values": {"git.sign": true}}' "$A/_rustybin/settings" >/dev/null
# ANCHOR_END: signing_key
)
echo "$out"
expect "$out" '"public":"ssh-ed25519 '
expect "$out" '"fingerprint":"SHA256:'
curl -s -o /dev/null -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"values": {"console.title": "Staging, signed"}}' "$A/_rustybin/settings"
out=$(
# ANCHOR: signed_commit
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"message": "A signed commit"}' "$A/_rustybin/git/commit"
# ANCHOR_END: signed_commit
)
echo "$out"
expect "$out" '"signed":true'
expect "$(git -C state-a/repo cat-file commit HEAD)" 'BEGIN SSH SIGNATURE'
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git")
expect "$out" '"signing":{"enabled":true,"key_set":true'

# Verified pulls: the second instance takes only what an allowed signer
# signed. Its allowed signer is the first instance's signing key (a public
# key, not a secret). git 2.38 or later verifies a merge commit.
if [[ "$(printf '%s\n' 2.38 "$(git --version | awk '{print $3}')" | sort -V | head -1)" != "2.38" ]]; then
  echo "ok (git is older than 2.38: verified pulls not run)"
  exit 0
fi
KEY=$(curl -s -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/signing-key" | sed -E 's/.*"public":"(ssh-ed25519 [A-Za-z0-9+\/=]+).*/\1/')
out=$(
# ANCHOR: verify_on
# Who may sign what this instance pulls (a name and a public key each), then
# verification on: the head of this moment is trusted as it is.
curl -s -X PATCH -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d "{\"values\": {\"git.allowed_signers\": [\"instance-a $KEY\"], \"git.verify\": true}}" \
  "$B/_rustybin/settings" >/dev/null
curl -s -H "Authorization: Bearer $ADMIN" "$B/_rustybin/git"
# ANCHOR_END: verify_on
)
expect "$out" '"verification":{"enabled":true,"signers":\[{"name":"instance-a","key_type":"ssh-ed25519","fingerprint":"SHA256:'
expect "$out" '"trusted":{"sha":"'
# The first instance pushes its signed commit: the second pulls and applies it.
curl -s -o /dev/null -X POST -H "Authorization: Bearer $ADMIN" "$A/_rustybin/git/push"
out=$(curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"apply": true, "prune": true}' "$B/_rustybin/git/pull")
echo "$out"
expect "$out" '"fast_forwarded":true'
# Someone pushes an unsigned commit to the branch: refused, nothing applied.
git -C elsewhere fetch --quiet origin
git -C elsewhere checkout --quiet -B staging origin/staging
echo "unsigned" > elsewhere/UNSIGNED.md
git -C elsewhere add UNSIGNED.md
git -C elsewhere -c user.name=someone -c user.email=someone@example.invalid -c commit.gpgsign=false \
  commit --quiet -m "An unsigned commit"
git -C elsewhere push --quiet origin staging
before=$(git -C state-b/repo rev-parse HEAD)
out=$(
# ANCHOR: verify_refused
curl -s -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
  -d '{"apply": true, "prune": true}' "$B/_rustybin/git/pull"
# ANCHOR_END: verify_refused
)
echo "$out"
expect "$out" '"reason":"unverified"'
expect "$out" '"why":"has no SSH signature"'
expect "$out" "\"commit\":\"$(git -C elsewhere rev-parse HEAD)\""
[[ "$(git -C state-b/repo rev-parse HEAD)" == "$before" ]] || { echo "the working copy moved" >&2; exit 1; }
out=$(curl -s -H "Authorization: Bearer $ADMIN" "$B/_rustybin/git")
expect "$out" '"last_refusal":{"at":"'
echo "ok"
