#!/usr/bin/env bash
# mTLS on Rustybin's own HTTPS listener (docs/src/reference/auth.md). Needs curl and jq.
# Run through docs/examples/run.sh (exit code 77 = tool missing, skipped).
set -euo pipefail
command -v jq >/dev/null || exit 77
HTTP="${BASE:-http://localhost:8080}"
HTTPS="${HTTPS_BASE:-https://localhost:8443}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }

# ANCHOR: download
curl -s "$HTTP/auth/mtls/get-client-cert" | jq -r .cert_pem > client.crt
curl -s "$HTTP/auth/mtls/get-client-cert" | jq -r .key_pem > client.key
curl -s "$HTTP/auth/mtls/get-ca-cert" | jq -r .ca_cert_pem > ca.crt
# ANCHOR_END: download

out=$(
# ANCHOR: tls_mode
# The server certificate is issued for localhost / 127.0.0.1 by the demo CA.
curl -s --cacert ca.crt --cert client.crt --key client.key "$HTTPS/auth/mtls"
# ANCHOR_END: tls_mode
)
expect "$out" '"auth_type":"mtls"'
expect "$out" '"source":"tls"'

out=$(
# ANCHOR: tls_no_cert
# Without a client certificate the TLS handshake still succeeds (client
# certificates are optional); the header fallback then reports what is missing.
curl -s --cacert ca.crt "$HTTPS/auth/mtls"
# ANCHOR_END: tls_no_cert
)
expect "$out" 'unauthorized'
