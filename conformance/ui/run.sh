#!/usr/bin/env bash
# Build Rustybin, start it on ports 18700-18702 (control plane open, the
# built-in mock AI provider), a
# second instance on 18703 with RUSTYBIN_CONTROL_AUTH=jwt, a third on 18704
# in public mode with self-service mocks, a fourth on 18705 in harness mode
# (loop-through) and a fifth on 18706 with the default control auth (the
# password sign-in), a sixth on 18707 with single sign-on through the
# first instance's demo identity provider and a seventh on 18708 with
# self-service mocks next to the operator's sign-in, run the browser
# end-to-end check of the web console (light and dark mode, the sign-in flows,
# visitors, the harness builder, single sign-on), then stop the servers.
#
# Usage: PYTHON=/path/to/venv/bin/python conformance/ui/run.sh
# (the venv needs `pip install playwright pyjwt cryptography`; Chromium comes
# from $PLAYWRIGHT_BROWSERS_PATH or `python -m playwright install chromium`).
#
# RUSTYBIN_UI_PORT moves the nine ports (default 18700).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PYTHON="${PYTHON:-python3}"
PORT="${RUSTYBIN_UI_PORT:-18700}"
JWT_PORT=$((PORT + 3))
SS_PORT=$((PORT + 4))
HARNESS_PORT=$((PORT + 5))
PASSWORD_PORT=$((PORT + 6))
SSO_PORT=$((PORT + 7))
SIGNIN_PORT=$((PORT + 8))
export RUSTYBIN_URL="http://127.0.0.1:$PORT"
export RUSTYBIN_JWT_URL="http://127.0.0.1:$JWT_PORT"
export RUSTYBIN_SS_URL="http://127.0.0.1:$SS_PORT"
export RUSTYBIN_HARNESS_URL="http://127.0.0.1:$HARNESS_PORT"
export RUSTYBIN_PASSWORD_URL="http://127.0.0.1:$PASSWORD_PORT"
export RUSTYBIN_SSO_URL="http://127.0.0.1:$SSO_PORT"
export RUSTYBIN_SSO_ADMIN_TOKEN="ui-sso-admin-token"
export RUSTYBIN_SIGNIN_URL="http://127.0.0.1:$SIGNIN_PORT"
export RUSTYBIN_SIGNIN_USER="operator"
export RUSTYBIN_SIGNIN_PASSWORD="ui-operator-password"
export SCREENSHOT_DIR="${SCREENSHOT_DIR:-$HERE/screenshots}"

(cd "$ROOT" && cargo build --quiet)
# Run a renamed copy from a temp dir: demo certificates are written to the
# working directory, and a unique name keeps unrelated `pkill rustybin` away.
TMP="$(mktemp -d)"
BIN="$TMP/ui-conformance-server"
cp "$ROOT/target/debug/rustybin" "$BIN"

# Signing key of the control-plane JWTs (fresh for every run).
openssl genpkey -algorithm ed25519 -out "$TMP/control.key" 2>/dev/null
openssl pkey -in "$TMP/control.key" -pubout -out "$TMP/control.pub"
export CONTROL_JWT_PRIVATE_KEY="$TMP/control.key"
export CONTROL_JWT_AUDIENCE="ui-conformance"

(cd "$TMP" && exec env RUSTYBIN_HTTP_PORT="$PORT" RUSTYBIN_HTTPS_PORT=$((PORT + 1)) RUSTYBIN_GRPC_PORT=$((PORT + 2)) \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open RUSTYBIN_ASSIST_PROVIDER=mock "$BIN") &
SERVER=$!
mkdir -p "$TMP/jwt"
(cd "$TMP/jwt" && exec env RUSTYBIN_HTTP_PORT="$JWT_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_CONTROL_AUTH=jwt \
  RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="$(cat "$TMP/control.pub")" \
  RUSTYBIN_CONTROL_JWT_AUDIENCE="$CONTROL_JWT_AUDIENCE" \
  RUSTYBIN_ADMIN_TOKEN=ui-admin-token \
  RUSTYBIN_CONSOLE_TITLE="Acme demo pod" \
  RUSTYBIN_CONSOLE_BACKLINK="https://portal.example.com/pods/acme" \
  "$BIN") &
JWT_SERVER=$!
mkdir -p "$TMP/ss"
export LEADS_FILE="$TMP/ss/leads.jsonl"
(cd "$TMP/ss" && exec env RUSTYBIN_HTTP_PORT="$SS_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open \
  RUSTYBIN_PUBLIC_MODE=true RUSTYBIN_MOCKS_SELF_SERVICE=true \
  RUSTYBIN_MOCKS_VISIT_MINUTES=5 RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY=0 \
  RUSTYBIN_LEADS_FILE="$LEADS_FILE" RUSTYBIN_PRIVACY_URL=https://example.com/privacy \
  "$BIN") &
SS_SERVER=$!
mkdir -p "$TMP/harness"
(cd "$TMP/harness" && exec env RUSTYBIN_HTTP_PORT="$HARNESS_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn RUSTYBIN_CONTROL_AUTH=open \
  RUSTYBIN_HARNESS=on RUSTYBIN_HARNESS_ROLES=both RUSTYBIN_HARNESS_TARGETS=127.0.0.1,localhost \
  RUSTYBIN_HARNESS_THREADS=1 \
  "$BIN") &
HARNESS_SERVER=$!
# The default control auth: nothing set but the ports, so the console asks
# for rustybin / rustybin.
mkdir -p "$TMP/password"
(cd "$TMP/password" && exec env RUSTYBIN_HTTP_PORT="$PASSWORD_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn \
  "$BIN") &
PASSWORD_SERVER=$!
# Single sign-on: the identity provider is the first instance's demo one
# (alice is in admins, bob is not); only single sign-on and the admin token
# sign in.
mkdir -p "$TMP/sso"
(cd "$TMP/sso" && exec env RUSTYBIN_HTTP_PORT="$SSO_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_CONSOLE_PASSWORD=ui-sso-password \
  RUSTYBIN_ADMIN_TOKEN="$RUSTYBIN_SSO_ADMIN_TOKEN" \
  RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true \
  RUSTYBIN_SSO_ENABLED=true RUSTYBIN_SSO_NAME="Demo IdP" \
  RUSTYBIN_SSO_ISSUER="$RUSTYBIN_URL" RUSTYBIN_SSO_CLIENT_ID=rustybin-public \
  RUSTYBIN_SSO_SCOPES="openid profile email groups" RUSTYBIN_SSO_ROLE_MAP="admins=admin" \
  RUSTYBIN_SSO_PASSWORD_SIGN_IN=false \
  "$BIN") &
SSO_SERVER=$!
# Self-service next to the sign-in: visitors need none, the operator signs in.
mkdir -p "$TMP/signin"
(cd "$TMP/signin" && exec env RUSTYBIN_HTTP_PORT="$SIGNIN_PORT" RUSTYBIN_HTTPS_PORT=0 RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_PUBLIC_MODE=true RUSTYBIN_MOCKS_SELF_SERVICE=true \
  RUSTYBIN_CONSOLE_USER="$RUSTYBIN_SIGNIN_USER" RUSTYBIN_CONSOLE_PASSWORD="$RUSTYBIN_SIGNIN_PASSWORD" \
  RUSTYBIN_PRIVACY_URL=https://example.com/privacy \
  "$BIN") &
SIGNIN_SERVER=$!
trap 'kill $SERVER $JWT_SERVER $SS_SERVER $HARNESS_SERVER $PASSWORD_SERVER $SSO_SERVER $SIGNIN_SERVER 2>/dev/null || true; rm -rf "$TMP"' EXIT

for url in "$RUSTYBIN_URL" "$RUSTYBIN_JWT_URL" "$RUSTYBIN_SS_URL" "$RUSTYBIN_HARNESS_URL" "$RUSTYBIN_PASSWORD_URL" "$RUSTYBIN_SSO_URL" "$RUSTYBIN_SIGNIN_URL"; do
  for _ in $(seq 1 50); do
    curl -sf -o /dev/null "$url/" && break
    sleep 0.2
  done
done

# The source control check starts an instance of its own (a state directory
# and a bare repository in a temporary directory) and stops it again.
export RUSTYBIN_UI_BIN="$BIN"
export RUSTYBIN_UI_GIT_PORT=$((PORT + 9))

"$PYTHON" "$HERE/console_e2e.py"
