#!/usr/bin/env bash
# Mint a control-plane JWT (EdDSA / Ed25519) with nothing but openssl (3.0+).
# docs/src/concepts/control-plane-security.md; used by docs/examples/run.sh.
#
#   _mint-control-jwt.sh <private-key.pem> <audience> [scope] [ttl-seconds]
#
# scope: console (default), inspector, admin, or several separated by spaces.
# A negative ttl gives an already expired token.
set -euo pipefail
key="$1"
aud="$2"
scope="${3:-console}"
ttl="${4:-900}"

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

# ANCHOR: mint
now=$(date +%s)
header=$(printf '{"alg":"EdDSA","typ":"JWT"}' | b64url)
payload=$(printf '{"aud":"%s","sub":"docs","scope":"%s","iat":%d,"exp":%d}' \
  "$aud" "$scope" "$now" $((now + ttl)) | b64url)
input="$(mktemp)"
trap 'rm -f "$input"' EXIT
printf '%s.%s' "$header" "$payload" >"$input"
signature=$(openssl pkeyutl -sign -inkey "$key" -rawin -in "$input" | b64url)
echo "$header.$payload.$signature"
# ANCHOR_END: mint
