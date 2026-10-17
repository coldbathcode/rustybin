#!/usr/bin/env bash
# Write the key registry used by the shared-mode examples
# (docs/src/concepts/shared-mode.md); used by docs/examples/run.sh.
#
#   _make-key-registry.sh <out.json> <alpha key> <beta key> <revoked key>
#
# Needs openssl (any version) for SHA-256.
set -euo pipefail
out="$1"
alpha="$2"
beta="$3"
revoked="$4"

# ANCHOR: hash
# The registry stores the SHA-256 (lowercase hex) of the whole key, never the key.
key_hash() { printf '%s' "$1" | openssl dgst -sha256 -r | cut -d' ' -f1; }
# ANCHOR_END: hash

# ANCHOR: registry
cat >"$out" <<JSON
{
  "version": "docs-1",
  "keys": [
    { "id": "kdocsalpha", "hash": "$(key_hash "$alpha")", "label": "starter", "status": "active",
      "limits": { "rps": 100, "burst": 200, "requests": 100000, "egress_bytes": 1000000000,
                  "inspector_entries": 1000, "inspector_ttl_secs": 86400 } },
    { "id": "kdocsbeta", "hash": "$(key_hash "$beta")", "label": "free", "status": "active",
      "limits": { "rps": 5, "burst": 10, "requests": 3 } },
    { "id": "kdocsold", "hash": "$(key_hash "$revoked")", "label": "free", "status": "revoked" }
  ]
}
JSON
# ANCHOR_END: registry
