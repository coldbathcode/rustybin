#!/usr/bin/env bash
# `rustybin mock ...` (docs/src/mocks/cli.md). Run through docs/examples/run.sh,
# which exports RUSTYBIN_BIN, BASE and ADMIN_TOKEN.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${RUSTYBIN_BIN:-$(command -v rustybin || true)}"
[[ -x "$BIN" ]] || { echo "set RUSTYBIN_BIN to a rustybin binary" >&2; exit 1; }
rustybin() { "$BIN" "$@"; }
BASE="${BASE:-http://localhost:8080}"
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp "$HERE/specs/inventory.yaml" "$HERE/rustybin.yaml" "$work/"
mkdir -p "$work/specs" && cp "$HERE/specs/inventory.yaml" "$work/specs/"
cd "$work"

out=$(
# ANCHOR: validate
rustybin mock validate inventory.yaml
# ANCHOR_END: validate
)
expect "$out" 'Inventory 2.0.0 (OpenAPI 3.1.0)'
# The spec has an example for one operation of three: the coverage warning.
expect "$out" 'examples: 1 of 3 operation(s)'
expect "$out" '0 error(s), 1 warning(s)'

out=$(
# ANCHOR: init
rustybin mock init inventory.yaml --name docs-cli --generated
# ANCHOR_END: init
)
expect "$out" '- name: docs-cli'
expect "$out" 'mode: generated'

out=$(
# ANCHOR: sample
rustybin mock sample inventory.yaml getItem --status 404
rustybin mock sample inventory.yaml getItem --request
# ANCHOR_END: sample
)
expect "$out" '"title": "unknown SKU"'
expect "$out" '"method": "GET"'

out=$(
# ANCHOR: check_config
INVENTORY_API_KEY=k1 rustybin mock check-config rustybin.yaml
# ANCHOR_END: check_config
)
expect "$out" 'inventory: Inventory (3 operations): 0 error(s), 1 warning(s)'

rustybin mock init inventory.yaml --name docs-cli --generated > rustybin.yaml
# Start clean when an earlier run stopped half way.
rustybin mock delete docs-cli --server "$BASE" --token "$ADMIN_TOKEN" >/dev/null 2>&1 || true

out=$(
# ANCHOR: push
export RUSTYBIN_SERVER="$BASE" RUSTYBIN_TOKEN="$ADMIN_TOKEN"
rustybin mock push rustybin.yaml
rustybin mock push rustybin.yaml
# ANCHOR_END: push
)
expect "$out" 'created: docs-cli'
expect "$out" 'unchanged: docs-cli'
export RUSTYBIN_SERVER="$BASE" RUSTYBIN_TOKEN="$ADMIN_TOKEN"

out=$(
# ANCHOR: list
rustybin mock list
# ANCHOR_END: list
)
expect "$out" 'docs-cli .* live'
expect "$out" "$BASE/mock/docs-cli/v2"
expect "$out" 'inventory .* secured'

out=$(
# ANCHOR: switch
rustybin mock disable docs-cli
curl -s -o /dev/null -w '%{http_code}\n' "$BASE/mock/docs-cli/v2/items"
rustybin mock enable docs-cli
curl -s -o /dev/null -w '%{http_code}\n' "$BASE/mock/docs-cli/v2/items"
# ANCHOR_END: switch
)
expect "$out" 'docs-cli: disabled'
expect "$out" '503'
expect "$out" 'docs-cli: enabled'
expect "$out" '200'

out=$(
# ANCHOR: deploy_export
rustybin mock deploy docs-cli
rustybin mock export docs-cli > docs-cli.rustybin.yaml
# ANCHOR_END: deploy_export
)
expect "$out" 'docs-cli: deploy'
expect "$(cat docs-cli.rustybin.yaml)" 'spec_inline:'

out=$(
# ANCHOR: json
rustybin mock list --json
# ANCHOR_END: json
)
expect "$out" '"name":"docs-cli"'

out=$(
# ANCHOR: delete
rustybin mock delete docs-cli
# ANCHOR_END: delete
)
expect "$out" 'docs-cli: deleted'

# A wrong token: exit code 2 and the server's message.
if out=$(rustybin mock disable inventory --token wrong 2>&1); then exit 1; fi
expect "$out" 'HTTP 401'
