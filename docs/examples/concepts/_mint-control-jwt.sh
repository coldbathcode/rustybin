#!/usr/bin/env bash
# Mint a control-plane JWT (EdDSA / Ed25519) with nothing but openssl (3.0+).
# docs/src/concepts/control-plane-security.md; used by docs/examples/run.sh.
#
#   _mint-control-jwt.sh <private-key.pem> <audience> [scope] [ttl-seconds] [rb_key]
#
# scope: console (default), inspector, admin, or several separated by spaces.
# A negative ttl gives an already expired token. rb_key: an API key id, for a
# token bound to one key in shared mode (docs/src/concepts/shared-mode.md).
set -euo pipefail
key="$1"
aud="$2"
scope="${3:-console}"
ttl="${4:-900}"
rb_key="${5:-}"

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

# ANCHOR: mint
# Optional claim binding the token to one API key (shared mode).
extra=""
if [[ -n "$rb_key" ]]; then
  extra=$(printf ',"rb_key":"%s"' "$rb_key")
fi
now=$(date +%s)
header=$(printf '{"alg":"EdDSA","typ":"JWT"}' | b64url)
payload=$(printf '{"aud":"%s","sub":"docs","scope":"%s","iat":%d,"exp":%d%s}' \
  "$aud" "$scope" "$now" $((now + ttl)) "$extra" | b64url)
input="$(mktemp)"
trap 'rm -f "$input"' EXIT
printf '%s.%s' "$header" "$payload" >"$input"
signature=$(openssl pkeyutl -sign -inkey "$key" -rawin -in "$input" | b64url)
echo "$header.$payload.$signature"
# ANCHOR_END: mint
