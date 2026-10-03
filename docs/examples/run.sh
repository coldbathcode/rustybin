#!/usr/bin/env bash
# Run every documentation example against a fresh Rustybin instance.
#
#   docs/examples/run.sh                 # build, start on 18800-18802, run all
#   docs/examples/run.sh ai/openai.hurl  # only some files (paths relative to docs/examples)
#
# Needs: hurl (https://hurl.dev), curl. Optional: grpcurl and websocat for the
# gRPC and WebSocket shell examples (skipped with a notice when missing), jq
# for the TLS client certificate example.
#
# Environment:
#   RUSTYBIN_BIN        use this binary instead of `cargo build` + target/debug/rustybin
#   RUSTYBIN_DOCS_PORT  first of the three ports (default 18800: HTTP, +1 HTTPS, +2 gRPC)
#   REQUIRE_ALL_TOOLS=1 fail instead of skipping when grpcurl / websocat / jq are missing (CI)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PORT="${RUSTYBIN_DOCS_PORT:-18800}"
HTTPS_PORT=$((PORT + 1))
GRPC_PORT=$((PORT + 2))
ADMIN_TOKEN="docs-admin-token"

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
# temporary directory and a fixed instance id.
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
  "$TMP/rustybin-docs-examples") &
SERVER=$!
trap 'kill "$SERVER" 2>/dev/null || true; wait "$SERVER" 2>/dev/null || true; rm -rf "$TMP"' EXIT

BASE="http://127.0.0.1:$PORT"
for _ in $(seq 1 100); do
  curl -fs -o /dev/null "$BASE/" && break
  sleep 0.1
done
curl -fs -o /dev/null "$BASE/" || { echo "server did not start" >&2; exit 1; }

export BASE
export HTTPS_BASE="https://127.0.0.1:$HTTPS_PORT"
export GRPC_ADDR="127.0.0.1:$GRPC_PORT"
export ADMIN_TOKEN

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
    --variable "https_url=$HTTPS_BASE" \
    --variable "grpc_url=http://$GRPC_ADDR" \
    --variable "admin_token=$ADMIN_TOKEN" \
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
