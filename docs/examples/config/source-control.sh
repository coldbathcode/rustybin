#!/usr/bin/env bash
# Source control (docs/src/source-control.md) against a bare repository in a
# temporary directory: an instance with a state directory commits and pushes
# its configuration, a second instance starts from the repository
# (RUSTYBIN_CONFIG_GIT) and holds the same mock. Run through
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
echo "ok"
