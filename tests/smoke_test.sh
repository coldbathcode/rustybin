#!/bin/bash
set -e

BASE="${RUSTYBIN_URL:-http://localhost:80}"
PASS=0
FAIL=0

check() {
    local name="$1"
    local expected_status="$2"
    shift 2
    local actual_status
    actual_status=$(curl -s -o /dev/null -w "%{http_code}" "$@")
    if [ "$actual_status" = "$expected_status" ]; then
        echo "  ✓ $name ($actual_status)"
        PASS=$((PASS + 1))
    else
        echo "  ✗ $name (expected $expected_status, got $actual_status)"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== Rustybin Smoke Test ==="
echo "Target: $BASE"
echo ""

echo "Health & Identity:"
check "GET /health" "200" "$BASE/health"
check "GET /identity" "200" "$BASE/identity"

echo ""
echo "Echo:"
check "GET /echo" "200" "$BASE/echo"
check "POST /echo" "200" -X POST "$BASE/echo" -d '{"test":true}' -H 'Content-Type: application/json'
check "GET /anything/foo/bar" "200" "$BASE/anything/foo/bar"
check "GET /echo (XML)" "200" -H 'Accept: application/xml' "$BASE/echo"

echo ""
echo "Status:"
check "GET /status/200" "200" "$BASE/status/200"
check "GET /status/418" "418" "$BASE/status/418"
check "GET /status/204" "204" "$BASE/status/204"
check "GET /status/302" "302" "$BASE/status/302"

echo ""
echo "Response Shaping:"
check "GET /delay/100" "200" "$BASE/delay/100"
check "GET /bytes/512" "200" "$BASE/bytes/512"
check "GET /stream/3" "200" "$BASE/stream/3"
check "GET /drip" "200" "$BASE/drip?bytes=100&delay=10&chunk_size=10"
check "GET /response-headers" "200" "$BASE/response-headers?X-Test=hello"
check "GET /cache/60" "200" "$BASE/cache/60"

echo ""
echo "Redirects & Cookies:"
check "GET /redirect/1 (no follow)" "302" "$BASE/redirect/1"
check "GET /redirect-to" "302" "$BASE/redirect-to?url=http://example.com"
check "GET /cookies" "200" "$BASE/cookies"
check "GET /cookies/set" "302" "$BASE/cookies/set?test=value"

echo ""
echo "Info:"
check "GET /ip" "200" "$BASE/ip"
check "GET /date" "200" "$BASE/date"
check "GET /date/America/New_York" "200" "$BASE/date/America/New_York"
check "GET /time" "200" "$BASE/time"

echo ""
echo "Random:"
check "GET /uuid" "200" "$BASE/uuid"
check "GET /guuid" "200" "$BASE/guuid"
check "GET /random" "200" "$BASE/random"
check "GET /random/int" "200" "$BASE/random/int"
check "GET /random/lorem-ipsum/2" "200" "$BASE/random/lorem-ipsum/2"

echo ""
echo "Images:"
check "GET /image/png" "200" "$BASE/image/png"
check "GET /image/jpeg" "200" "$BASE/image/jpeg"
check "GET /image/gif" "200" "$BASE/image/gif"

echo ""
echo "Auth - Basic:"
check "Basic Auth (valid)" "200" -u basic:password "$BASE/auth/basic-auth"
check "Basic Auth (invalid)" "401" -u wrong:creds "$BASE/auth/basic-auth"
check "Basic Auth (custom)" "200" -u alice:secret "$BASE/auth/basic-auth/alice/secret"

echo ""
echo "Auth - API Key:"
check "API Key (valid)" "200" -H 'apikey: my-key' "$BASE/auth/api-key"
check "API Key (missing)" "401" "$BASE/auth/api-key"

echo ""
echo "Auth - HMAC:"
HMAC_DATE="Mon, 02 Jan 2006 15:04:05 GMT"
HMAC_SIG=$(printf 'date: %s' "$HMAC_DATE" | openssl dgst -sha256 -hmac "secret" -binary | base64)
check "HMAC (valid)" "200" -H "date: $HMAC_DATE" \
    -H "authorization: hmac username=\"alice\", algorithm=\"hmac-sha256\", headers=\"date\", signature=\"$HMAC_SIG\"" \
    "$BASE/auth/hmac"
check "HMAC (missing)" "401" "$BASE/auth/hmac"

echo ""
echo "Auth - JWT:"
JWT_HEADER=$(printf '{"alg":"HS256","typ":"JWT"}' | base64 | tr '/+' '_-' | tr -d '=')
JWT_PAYLOAD=$(printf '{"sub":"1234","name":"Test"}' | base64 | tr '/+' '_-' | tr -d '=')
TEST_JWT="${JWT_HEADER}.${JWT_PAYLOAD}.fakesig"
check "JWT (valid structure)" "200" -H "Authorization: Bearer $TEST_JWT" "$BASE/auth/jwt"
check "JWT (missing)" "401" "$BASE/auth/jwt"
check "JWT exchange" "200" -H "Authorization: Bearer $TEST_JWT" "$BASE/auth/jwt/exchange"

echo ""
echo "OIDC:"
check "OIDC Discovery" "200" "$BASE/.well-known/openid-configuration"
check "JWKS" "200" "$BASE/oauth/jwks"
check "Token (client_credentials)" "200" -X POST "$BASE/oauth/token" -d 'grant_type=client_credentials&client_id=rustybin&client_secret=secret'
check "Authorize page" "200" "$BASE/oauth/authorize?client_id=rustybin&redirect_uri=http://localhost/echo&response_type=code&scope=openid"

echo ""
echo "mTLS:"
check "Get client cert" "200" "$BASE/auth/mtls/get-client-cert"
check "Get CA cert" "200" "$BASE/auth/mtls/get-ca-cert"

echo ""
echo "AI Gateway:"
check "Chat completions" "200" -X POST "$BASE/ai/v1/chat/completions" -H 'Content-Type: application/json' -d '{"model":"rustybin-gpt","messages":[{"role":"user","content":"hello"}]}'
check "Completions" "200" -X POST "$BASE/ai/v1/completions" -H 'Content-Type: application/json' -d '{"model":"rustybin-gpt","prompt":"test"}'
check "Embeddings" "200" -X POST "$BASE/ai/v1/embeddings" -H 'Content-Type: application/json' -d '{"model":"rustybin-embed","input":"test"}'
check "Models" "200" "$BASE/ai/v1/models"
check "Anthropic messages" "200" -X POST "$BASE/ai/anthropic/v1/messages" -H 'Content-Type: application/json' -d '{"model":"rustybin-claude","max_tokens":128,"messages":[{"role":"user","content":"hello"}]}'

echo ""
echo "GraphQL:"
check "GraphQL query" "200" -X POST "$BASE/graphql" -H 'Content-Type: application/json' -d '{"query":"{ users { id name } }"}'
check "GraphQL schema" "200" "$BASE/graphql/schema"

echo ""
echo "Orchestration:"
check "Step 1" "200" -X POST -H 'X-Api-Key: test' -H 'Content-Type: application/json' -d '{"merchant_id":"M001","request_type":"payment"}' "$BASE/orchestration/step/1"
check "Orchestration status" "200" "$BASE/orchestration/status"

echo ""
echo "SOAP:"
check "SOAP GetUser" "200" -X POST -H 'Content-Type: text/xml' -d '<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><GetUser xmlns="http://rustybin.local/users"><userId>1</userId></GetUser></soap:Body></soap:Envelope>' "$BASE/soap"
check "WSDL" "200" "$BASE/soap/wsdl"

echo ""
echo "Flaky:"
check "Flaky 0%" "200" "$BASE/flaky/0"
check "Flaky 100%" "503" "$BASE/flaky/100"
check "Flaky pattern SSF" "200" "$BASE/flaky/pattern/SSF"
check "Flaky status" "200" "$BASE/flaky/status"
check "Flaky reset" "200" -X POST "$BASE/flaky/reset"

echo ""
echo "Health toggle:"
check "Set unhealthy" "503" -X POST "$BASE/health/unhealthy"
check "Health reflects unhealthy" "503" "$BASE/health"
check "Set healthy" "200" -X POST "$BASE/health/healthy"
check "Health reflects healthy" "200" "$BASE/health"

echo ""
echo "gRPC (optional, needs grpcurl):"
GRPC_HOST="${RUSTYBIN_GRPC_ADDR:-localhost:50051}"
if command -v grpcurl >/dev/null 2>&1; then
    if grpcurl -plaintext -d '{"message":"ping"}' "$GRPC_HOST" rustybin.echo.v1.EchoService/Echo >/dev/null 2>&1; then
        echo "  ✓ gRPC EchoService/Echo"
        PASS=$((PASS + 1))
    else
        echo "  ✗ gRPC EchoService/Echo (call failed)"
        FAIL=$((FAIL + 1))
    fi
else
    echo "  - skipped (grpcurl not installed)"
fi

echo ""
echo "Docs:"
check "OpenAPI JSON" "200" "$BASE/openapi.json"
check "OpenAPI YAML" "200" "$BASE/openapi.yaml"
check "Docs UI" "200" "$BASE/docs"

echo ""
echo "==============================="
echo "Results: $PASS passed, $FAIL failed"
echo "==============================="

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
