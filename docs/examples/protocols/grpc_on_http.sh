#!/usr/bin/env bash
# gRPC on the HTTP and HTTPS listeners (RUSTYBIN_GRPC_ON_HTTP=true), as a
# TLS-terminating proxy forwards it (docs/src/reference/grpc.md). Needs grpcurl.
# Runs against the secured instance of docs/examples/run.sh (exit code 77 =
# tool missing, skipped).
set -euo pipefail
command -v grpcurl >/dev/null || exit 77
HTTP_ADDR="${SECURED_ADDR:-127.0.0.1:80}"          # host:port of the HTTP listener
HTTPS_ADDR="${SECURED_HTTPS_ADDR:-127.0.0.1:443}"  # host:port of the HTTPS listener
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }

out=$(
# ANCHOR: h2c
# Cleartext HTTP/2 (h2c) on the HTTP port, what a proxy sends after TLS termination.
grpcurl -plaintext -d '{"message": "same port as the web"}' \
  "$HTTP_ADDR" rustybin.echo.v1.EchoService/Echo
# ANCHOR_END: h2c
)
expect "$out" '"message": "same port as the web"'
expect "$out" '"instanceId": "docs-secured"'

out=$(
# ANCHOR: tls
# HTTP/2 over TLS on the HTTPS port (ALPN h2; -insecure: demo certificate).
grpcurl -insecure "$HTTPS_ADDR" list
# ANCHOR_END: tls
)
expect "$out" rustybin.echo.v1.EchoService
expect "$out" grpc.reflection.v1.ServerReflection

out=$(
# ANCHOR: health
grpcurl -plaintext -d '{"service": "rustybin.echo.v1.EchoService"}' \
  "$HTTP_ADDR" grpc.health.v1.Health/Check
# ANCHOR_END: health
)
expect "$out" SERVING

# Plain HTTP on the same port is unaffected.
curl -fsS -o /dev/null "http://$HTTP_ADDR/echo"
