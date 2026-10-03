#!/usr/bin/env bash
# gRPC examples (docs/src/reference/grpc.md). Needs grpcurl.
# Run through docs/examples/run.sh (exit code 77 = tool missing, skipped).
set -euo pipefail
command -v grpcurl >/dev/null || exit 77
GRPC="${GRPC_ADDR:-127.0.0.1:50051}"   # host:port of the gRPC listener
cd "$(dirname "$0")/../../.."           # repository root (for proto/echo.proto)
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }

out=$(
# ANCHOR: list
grpcurl -plaintext "$GRPC" list
# ANCHOR_END: list
)
expect "$out" rustybin.echo.v1.EchoService
expect "$out" grpc.health.v1.Health

out=$(
# ANCHOR: unary
grpcurl -plaintext -H 'x-tenant: demo' -d '{"message": "hello"}' \
  "$GRPC" rustybin.echo.v1.EchoService/Echo
# ANCHOR_END: unary
)
expect "$out" '"message": "hello"'
expect "$out" '"x-tenant": "demo"'
expect "$out" '"instanceId": "docs-examples"'

out=$(
# ANCHOR: with_proto
grpcurl -plaintext -import-path proto -proto echo.proto \
  -d '{"message": "no reflection needed"}' "$GRPC" rustybin.echo.v1.EchoService/Echo
# ANCHOR_END: with_proto
)
expect "$out" 'no reflection needed'

out=$(
# ANCHOR: server_stream
grpcurl -plaintext -d '{"message": "tick", "count": 3}' \
  "$GRPC" rustybin.echo.v1.EchoService/ServerStream
# ANCHOR_END: server_stream
)
expect "$out" '"index": 2'

out=$(
# ANCHOR: client_stream
grpcurl -plaintext -d '{"message": "a"} {"message": "b"} {"message": "c"}' \
  "$GRPC" rustybin.echo.v1.EchoService/ClientStream
# ANCHOR_END: client_stream
)
expect "$out" '"message": "a b c"'
expect "$out" '"index": 3'

out=$(
# ANCHOR: fail
grpcurl -plaintext -d '{"code": 14, "message": "backend down", "reason": "DEMO_OUTAGE", "retry_delay_ms": 500}' \
  "$GRPC" rustybin.echo.v1.EchoService/Fail 2>&1 || true
# ANCHOR_END: fail
)
expect "$out" 'Code: Unavailable'
expect "$out" 'backend down'
expect "$out" 'google.rpc.RetryInfo'

out=$(
# ANCHOR: health
grpcurl -plaintext -d '{"service": "rustybin.echo.v1.EchoService"}' \
  "$GRPC" grpc.health.v1.Health/Check
# ANCHOR_END: health
)
expect "$out" SERVING
