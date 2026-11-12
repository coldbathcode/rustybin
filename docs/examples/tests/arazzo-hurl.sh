#!/usr/bin/env bash
# An Arazzo document imported, its test exported as Hurl and run with hurl
# (docs/src/saved-tests.md). Needs curl and hurl.
# Run through docs/examples/run.sh (exit code 77 = tool missing, skipped).
set -euo pipefail
command -v hurl >/dev/null || exit 77
BASE="${BASE:-http://localhost:8080}"
AUTH="Authorization: Bearer ${ADMIN_TOKEN:-}"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# ANCHOR: import
curl -sf -X POST -H "$AUTH" -H 'Content-Type: application/yaml' \
  --data-binary @"$here/pets.arazzo.yaml" "$BASE/_rustybin/tests/import" > /dev/null
# ANCHOR_END: import

# ANCHOR: export
curl -sf -H "$AUTH" "$BASE/_rustybin/tests/docs-create-and-read/export" > "$work/journey.hurl"
hurl --test --variable base_url="$BASE" --variable PETNAME=Rex "$work/journey.hurl"
# ANCHOR_END: export
grep -q 'create_id: jsonpath "$.id"' "$work/journey.hurl"
grep -q '/mock/petstore/v1/pets/{{create_id}}' "$work/journey.hurl"

curl -sf -X DELETE -H "$AUTH" "$BASE/_rustybin/tests/docs-create-and-read" > /dev/null
