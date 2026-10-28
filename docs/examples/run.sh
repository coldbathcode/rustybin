#!/usr/bin/env bash
# Run every documentation example against a fresh Rustybin instance.
#
#   docs/examples/run.sh                 # build, start on 18800-18802, run all
#   docs/examples/run.sh ai/openai.hurl  # only some files (paths relative to docs/examples)
#
# Needs: hurl 8 (https://hurl.dev), curl. Optional: grpcurl and websocat for the
# gRPC and WebSocket shell examples (skipped with a notice when missing), jq
# for the TLS client certificate example.
#
# Environment:
#   RUSTYBIN_BIN        use this binary instead of `cargo build` + target/debug/rustybin
#   RUSTYBIN_DOCS_PORT  first of the ports (default 18800: HTTP, +1 HTTPS, +2 gRPC,
#                       +3 HTTP of a second instance with plan limits, +4 HTTP and
#                       +5 HTTPS of a third, secured instance: control-plane JWT
#                       auth, gRPC on the HTTP listeners, console title and link;
#                       +6 HTTP of a fourth instance in shared mode with API keys
#                       from a registry file in the temporary directory; +7 HTTP
#                       of a fifth, normal instance with RUSTYBIN_REQUIRE_KEY; +8 HTTP
#                       of a sixth, public instance with self-service mocks; +9 HTTP
#                       of a seventh instance in harness mode, loop-through)
#   REQUIRE_ALL_TOOLS=1 fail instead of skipping when grpcurl / websocat / jq are missing (CI)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PORT="${RUSTYBIN_DOCS_PORT:-18800}"
HTTPS_PORT=$((PORT + 1))
GRPC_PORT=$((PORT + 2))
PLAN_PORT=$((PORT + 3))
SECURED_PORT=$((PORT + 4))
SECURED_HTTPS_PORT=$((PORT + 5))
SHARED_PORT=$((PORT + 6))
KEYED_PORT=$((PORT + 7))
SS_PORT=$((PORT + 8))
HARNESS_PORT=$((PORT + 9))
ADMIN_TOKEN="docs-admin-token"
SECURED_AUDIENCE="docs-secured"
SHARED_AUDIENCE="docs-shared"
# API keys of the shared-mode examples (documentation values, not secrets).
ALPHA_KEY="rbk_kdocsalpha_7d1f0c9b2e4a6f8d0b1c3e5a7f9d2b4c"
BETA_KEY="rbk_kdocsbeta_0a2c4e6f8b1d3f5a7c9e1b3d5f7a9c2e"
REVOKED_KEY="rbk_kdocsold_5e7a9c1e3b5d7f9a2c4e6b8d0f1a3c5e"

if ! command -v hurl >/dev/null 2>&1; then
  echo "hurl is required (https://hurl.dev/docs/installation.html)" >&2
  exit 1
fi

if [[ -z "${RUSTYBIN_BIN:-}" ]]; then
  (cd "$ROOT" && cargo build --quiet)
  RUSTYBIN_BIN="$ROOT/target/debug/rustybin"
fi

TMP="$(mktemp -d)"
# Run a renamed copy: an unrelated `pkill rustybin` cannot stop it mid-run.
cp "$RUSTYBIN_BIN" "$TMP/rustybin-docs-examples"
# The settings below are the ones the documentation assumes (see
# docs/src/examples.md): an admin token, mTLS header mode, demo TLS files in a
# temporary directory, a fixed instance id, and OpenAPI mocks from
# docs/examples/mocks (a rustybin.yaml with an environment variable, and a
# directory of specs).
(cd "$TMP" && exec env \
  RUSTYBIN_HTTP_PORT="$PORT" \
  RUSTYBIN_HTTPS_PORT="$HTTPS_PORT" \
  RUSTYBIN_GRPC_PORT="$GRPC_PORT" \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID=docs-examples \
  RUSTYBIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  RUSTYBIN_MTLS_IN_HEADER=X-Client-Cert \
  RUSTYBIN_TLS_CERT="$TMP/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/server.key" \
  RUSTYBIN_CONFIG_FILE="$HERE/mocks/rustybin.yaml" \
  RUSTYBIN_MOCKS_DIR="$HERE/mocks/specs-dir" \
  INVENTORY_API_KEY=docs-inventory-key \
  "$TMP/rustybin-docs-examples") &
SERVER=$!
# A second instance with the free plan and tiny overrides, so the plan limit
# examples (docs/src/concepts/plans-and-limits.md) reach a 429 in a few
# requests. HTTPS and gRPC use ephemeral ports.
mkdir -p "$TMP/plan"
(cd "$TMP/plan" && exec env \
  RUSTYBIN_HTTP_PORT="$PLAN_PORT" \
  RUSTYBIN_HTTPS_PORT=0 \
  RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID=docs-plan \
  RUSTYBIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  RUSTYBIN_TLS_CERT="$TMP/plan/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/plan/server.key" \
  RUSTYBIN_PLAN=free \
  RUSTYBIN_LIMIT_RPS=1 \
  RUSTYBIN_LIMIT_BURST=5 \
  RUSTYBIN_LIMIT_REQUESTS=7 \
  RUSTYBIN_LIMIT_EGRESS_MB=0.15 \
  RUSTYBIN_LIMIT_STREAMS=1 \
  RUSTYBIN_LIMIT_STREAM_SECS=2 \
  "$TMP/rustybin-docs-examples") &
PLAN_SERVER=$!
# A third instance as a hosted platform would run it (see
# docs/src/concepts/control-plane-security.md): control-plane JWTs signed by a
# key generated for this run, gRPC on the HTTP and HTTPS listeners, JSON logs,
# a console title and back link. Its gRPC port is ephemeral.
mkdir -p "$TMP/secured"
openssl genpkey -algorithm ed25519 -out "$TMP/secured/control.key" 2>/dev/null
openssl pkey -in "$TMP/secured/control.key" -pubout -out "$TMP/secured/control.pub"
(cd "$TMP/secured" && exec env \
  RUSTYBIN_HTTP_PORT="$SECURED_PORT" \
  RUSTYBIN_HTTPS_PORT="$SECURED_HTTPS_PORT" \
  RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID="$SECURED_AUDIENCE" \
  RUSTYBIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  RUSTYBIN_TLS_CERT="$TMP/secured/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/secured/server.key" \
  RUSTYBIN_HOSTED_MODE=true \
  RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="$(cat "$TMP/secured/control.pub")" \
  RUSTYBIN_GRPC_ON_HTTP=true \
  RUSTYBIN_CONSOLE_TITLE="Docs demo" \
  RUSTYBIN_CONSOLE_BACKLINK="https://portal.example.com/instances/docs-secured" \
  "$TMP/rustybin-docs-examples" >/dev/null) &
SECURED_SERVER=$!
# A fourth instance in shared mode (docs/src/concepts/shared-mode.md): API
# keys from a registry file, control-plane JWTs with the secured instance's
# key, a quota upgrade link. HTTPS and gRPC use ephemeral ports.
mkdir -p "$TMP/shared"
bash "$HERE/concepts/_make-key-registry.sh" "$TMP/shared/keys.json" "$ALPHA_KEY" "$BETA_KEY" "$REVOKED_KEY"
(cd "$TMP/shared" && exec env \
  RUSTYBIN_HTTP_PORT="$SHARED_PORT" \
  RUSTYBIN_HTTPS_PORT=0 \
  RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID="$SHARED_AUDIENCE" \
  RUSTYBIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  RUSTYBIN_TLS_CERT="$TMP/shared/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/shared/server.key" \
  RUSTYBIN_SHARED_MODE=true \
  RUSTYBIN_KEYS_FILE="$TMP/shared/keys.json" \
  RUSTYBIN_QUOTA_UPGRADE_URL="https://example.com/pricing" \
  RUSTYBIN_CONTROL_AUTH=jwt \
  RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="$(cat "$TMP/secured/control.pub")" \
  "$TMP/rustybin-docs-examples") &
SHARED_SERVER=$!
# A fifth, normal instance that only requires a key on the data plane.
mkdir -p "$TMP/keyed"
(cd "$TMP/keyed" && exec env \
  RUSTYBIN_HTTP_PORT="$KEYED_PORT" \
  RUSTYBIN_HTTPS_PORT=0 \
  RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID=docs-keyed \
  RUSTYBIN_TLS_CERT="$TMP/keyed/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/keyed/server.key" \
  RUSTYBIN_REQUIRE_KEY=true \
  RUSTYBIN_KEYS_FILE="$TMP/shared/keys.json" \
  "$TMP/rustybin-docs-examples") &
KEYED_SERVER=$!
# A sixth, public instance with self-service mocks (docs/src/mocks/self-service.md):
# the mocks domain mocks.example, 10 minute periods (Extend is open at once),
# one anonymous visit per session and day, leads written to a file.
mkdir -p "$TMP/ss"
(cd "$TMP/ss" && exec env \
  RUSTYBIN_HTTP_PORT="$SS_PORT" \
  RUSTYBIN_HTTPS_PORT=0 \
  RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID=docs-self-service \
  RUSTYBIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  RUSTYBIN_TLS_CERT="$TMP/ss/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/ss/server.key" \
  RUSTYBIN_PUBLIC_MODE=true \
  RUSTYBIN_MOCKS_SELF_SERVICE=true \
  RUSTYBIN_MOCKS_SELF_SERVICE_HOST=mocks.example \
  RUSTYBIN_MOCKS_VISIT_MINUTES=10 \
  RUSTYBIN_MOCKS_ANON_HOURS=1 \
  RUSTYBIN_MOCKS_ANON_VISITS_PER_DAY=1 \
  RUSTYBIN_LEADS_FILE="$TMP/ss/leads.jsonl" \
  RUSTYBIN_PRIVACY_URL=https://example.com/privacy \
  "$TMP/rustybin-docs-examples") &
SS_SERVER=$!
# A seventh instance in harness mode (docs/src/harness.md): roles both and
# coordinator (loop-through: it sends load to itself through its own
# listener and is a cluster of one), 127.0.0.1 allowed, one engine thread.
mkdir -p "$TMP/harness"
(cd "$TMP/harness" && exec env \
  RUSTYBIN_HTTP_PORT="$HARNESS_PORT" \
  RUSTYBIN_HTTPS_PORT=0 \
  RUSTYBIN_GRPC_PORT=0 \
  RUSTYBIN_HOST=127.0.0.1 \
  RUSTYBIN_LOG_LEVEL=warn \
  RUSTYBIN_INSTANCE_ID=docs-harness \
  RUSTYBIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  RUSTYBIN_TLS_CERT="$TMP/harness/server.crt" \
  RUSTYBIN_TLS_KEY="$TMP/harness/server.key" \
  RUSTYBIN_HARNESS=on \
  RUSTYBIN_HARNESS_ROLES=both,coordinator \
  RUSTYBIN_HARNESS_TARGETS=127.0.0.1,localhost \
  RUSTYBIN_HARNESS_CONTROL_URL=auto \
  RUSTYBIN_HARNESS_THREADS=1 \
  "$TMP/rustybin-docs-examples") &
HARNESS_SERVER=$!
ALL_SERVERS=("$SERVER" "$PLAN_SERVER" "$SECURED_SERVER" "$SHARED_SERVER" "$KEYED_SERVER" "$SS_SERVER" "$HARNESS_SERVER")
trap 'kill "${ALL_SERVERS[@]}" 2>/dev/null || true; wait "${ALL_SERVERS[@]}" 2>/dev/null || true; rm -rf "$TMP"' EXIT

BASE="http://127.0.0.1:$PORT"
PLAN_BASE="http://127.0.0.1:$PLAN_PORT"
SECURED_BASE="http://127.0.0.1:$SECURED_PORT"
SHARED_BASE="http://127.0.0.1:$SHARED_PORT"
KEYED_BASE="http://127.0.0.1:$KEYED_PORT"
SS_BASE="http://127.0.0.1:$SS_PORT"
HARNESS_BASE="http://127.0.0.1:$HARNESS_PORT"
for url in "$BASE" "$PLAN_BASE" "$SECURED_BASE" "$SHARED_BASE" "$KEYED_BASE" "$SS_BASE" "$HARNESS_BASE"; do
  for _ in $(seq 1 100); do
    curl -fs -o /dev/null "$url/" && break
    sleep 0.1
  done
  curl -fs -o /dev/null "$url/" || { echo "server $url did not start" >&2; exit 1; }
done

export BASE
export PLAN_BASE
export SECURED_BASE
export SHARED_BASE
export KEYED_BASE
export HTTPS_BASE="https://127.0.0.1:$HTTPS_PORT"
export GRPC_ADDR="127.0.0.1:$GRPC_PORT"
export SECURED_ADDR="127.0.0.1:$SECURED_PORT"
export SECURED_HTTPS_ADDR="127.0.0.1:$SECURED_HTTPS_PORT"
export ADMIN_TOKEN
export RUSTYBIN_BIN

# Control-plane tokens for the secured instance.
MINT="$HERE/concepts/_mint-control-jwt.sh"
CONSOLE_TOKEN="$(bash "$MINT" "$TMP/secured/control.key" "$SECURED_AUDIENCE" console 900)"
INSPECTOR_TOKEN="$(bash "$MINT" "$TMP/secured/control.key" "$SECURED_AUDIENCE" inspector 900)"
EXPIRED_TOKEN="$(bash "$MINT" "$TMP/secured/control.key" "$SECURED_AUDIENCE" console -600)"
OTHER_AUDIENCE_TOKEN="$(bash "$MINT" "$TMP/secured/control.key" another-instance console 900)"
# A control-plane token for the shared instance, bound to the alpha key.
KEY_INSPECTOR_TOKEN="$(bash "$MINT" "$TMP/secured/control.key" "$SHARED_AUDIENCE" inspector 900 kdocsalpha)"
export CONSOLE_TOKEN

cd "$HERE"
if [[ $# -gt 0 ]]; then
  files=("$@")
else
  mapfile -t files < <(find . -name '*.hurl' -o -name '*.sh' ! -name run.sh ! -name '_*' | sed 's#^\./##' | sort)
fi

hurl_files=()
sh_files=()
for f in "${files[@]}"; do
  case "$f" in
    *.hurl) hurl_files+=("$f") ;;
    *.sh) sh_files+=("$f") ;;
  esac
done

status=0
if [[ ${#hurl_files[@]} -gt 0 ]]; then
  # One file at a time: a few examples flip instance-global state (/health).
  hurl --test --jobs 1 --color \
    --variable "base_url=$BASE" \
    --variable "plan_url=$PLAN_BASE" \
    --variable "https_url=$HTTPS_BASE" \
    --variable "grpc_url=http://$GRPC_ADDR" \
    --variable "admin_token=$ADMIN_TOKEN" \
    --variable "secured_url=$SECURED_BASE" \
    --variable "console_token=$CONSOLE_TOKEN" \
    --variable "inspector_token=$INSPECTOR_TOKEN" \
    --variable "expired_token=$EXPIRED_TOKEN" \
    --variable "other_audience_token=$OTHER_AUDIENCE_TOKEN" \
    --variable "shared_url=$SHARED_BASE" \
    --variable "keyed_url=$KEYED_BASE" \
    --variable "ss_url=$SS_BASE" \
    --variable "alpha_key=$ALPHA_KEY" \
    --variable "beta_key=$BETA_KEY" \
    --variable "revoked_key=$REVOKED_KEY" \
    --variable "key_inspector_token=$KEY_INSPECTOR_TOKEN" \
    --variable "harness_url=$HARNESS_BASE" \
    "${hurl_files[@]}" || status=1
fi

for f in "${sh_files[@]}"; do
  echo "### $f"
  if timeout 120 bash "$f"; then
    echo "ok   $f"
  else
    rc=$?
    if [[ $rc -eq 77 && "${REQUIRE_ALL_TOOLS:-0}" != "1" ]]; then
      echo "skip $f (a required tool is not installed)"
    else
      echo "FAIL $f (exit $rc)"
      status=1
    fi
  fi
done

exit "$status"
