# JWT and HMAC signatures (docs/src/reference/auth.md)

# ANCHOR: jwt_hs256
# The demo HS256 token (secret rustybin-demo-secret-do-not-use-in-production, exp in 2100).
GET {{base_url}}/auth/jwt?iss=rustybin&aud=rustybin
Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaXNzIjoicnVzdHliaW4iLCJhdWQiOiJydXN0eWJpbiIsInNjb3BlIjoib3BlbmlkIHByb2ZpbGUiLCJpYXQiOjE1MTYyMzkwMjIsImV4cCI6NDEwMjQ0NDgwMH0.o8obKOeREkQXJtz3ZyqxTs5bvJBn9lxiYnL2NC9Npbw
HTTP 200
[Asserts]
jsonpath "$.auth_type" == "jwt"
jsonpath "$.username" == "1234567890"
jsonpath "$.jwt_header.alg" == "HS256"
jsonpath "$.claims.name" == "John Doe"
# ANCHOR_END: jwt_hs256

# ANCHOR: jwt_wrong_audience
GET {{base_url}}/auth/jwt?aud=payments-api
Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaXNzIjoicnVzdHliaW4iLCJhdWQiOiJydXN0eWJpbiIsInNjb3BlIjoib3BlbmlkIHByb2ZpbGUiLCJpYXQiOjE1MTYyMzkwMjIsImV4cCI6NDEwMjQ0NDgwMH0.o8obKOeREkQXJtz3ZyqxTs5bvJBn9lxiYnL2NC9Npbw
HTTP 401
[Asserts]
header "WWW-Authenticate" contains "error=\"invalid_token\""
# ANCHOR_END: jwt_wrong_audience

# ANCHOR: jwt_rs256
# RS256 tokens from the built-in identity provider validate too.
POST {{base_url}}/oauth/token
[Form]
grant_type: client_credentials
client_id: rustybin
client_secret: secret
HTTP 200
[Captures]
rs_token: jsonpath "$.access_token"

GET {{base_url}}/auth/jwt
Authorization: Bearer {{rs_token}}
HTTP 200
[Asserts]
jsonpath "$.jwt_header.alg" == "RS256"
jsonpath "$.jwt_header.kid" == "rustybin-rs256-key"
jsonpath "$.claims.client_id" == "rustybin"
# ANCHOR_END: jwt_rs256

# ANCHOR: jwt_decode
# Decode WITHOUT validation: what did the gateway forward?
GET {{base_url}}/auth/jwt/decode
Authorization: Bearer eyJhbGciOiJub25lIn0.eyJzdWIiOiJnYXRld2F5LXVzZXIiLCJyb2xlIjoiYWRtaW4ifQ.
HTTP 200
[Asserts]
jsonpath "$.verified" == false
jsonpath "$.jwt_header.alg" == "none"
jsonpath "$.claims.sub" == "gateway-user"
# ANCHOR_END: jwt_decode

# ANCHOR: jwt_none_rejected
GET {{base_url}}/auth/jwt
Authorization: Bearer eyJhbGciOiJub25lIn0.eyJzdWIiOiJnYXRld2F5LXVzZXIiLCJyb2xlIjoiYWRtaW4ifQ.
HTTP 401
# ANCHOR_END: jwt_none_rejected

# ANCHOR: jwt_exchange
POST {{base_url}}/auth/jwt/exchange
Authorization: Bearer {{rs_token}}
HTTP 200
[Captures]
exchanged: jsonpath "$.exchanged_token"
[Asserts]
jsonpath "$.claims.iss" == "rustybin"
jsonpath "$.claims.sub" == "rustybin"

GET {{base_url}}/auth/jwt/decode
Authorization: Bearer {{exchanged}}
HTTP 200
[Asserts]
jsonpath "$.jwt_header.alg" == "HS256"
# ANCHOR_END: jwt_exchange

# ANCHOR: hmac
# Signing string: "date: <Date>\nGET /auth/hmac?clock_skew=0 HTTP/1.1",
# HMAC-SHA256 with the default secret `secret`, base64 encoded.
# clock_skew=0 disables the Date freshness check so the example stays valid.
GET {{base_url}}/auth/hmac?clock_skew=0
Date: Mon, 02 Jan 2006 15:04:05 GMT
Authorization: hmac username="alice", algorithm="hmac-sha256", headers="date request-line", signature="hQpFaCiOTydp6g+6wF5Un7Z+m/CWhnU2x7pOC9ofD+Y="
HTTP 200
[Asserts]
jsonpath "$.auth_type" == "hmac"
jsonpath "$.username" == "alice"
jsonpath "$.claims.algorithm" == "hmac-sha256"
jsonpath "$.claims.signed_headers" == "date request-line"
# ANCHOR_END: hmac

# ANCHOR: hmac_digest
# draft-cavage style, HMAC-SHA512, with a body digest and path credentials (bob / s3cr3t).
POST {{base_url}}/auth/hmac/bob/s3cr3t?clock_skew=0
Date: Mon, 02 Jan 2006 15:04:05 GMT
Digest: SHA-256=8m4mfuAzMf9c4QtoehuhqbSQEv+ydpTJIuF0EbS4bmw=
Authorization: Signature keyId="bob", algorithm="hmac-sha512", headers="date (request-target) digest", signature="E1ntlIp2Br+QVxBf758Fn/lvg1Z+ko8cRo6HKRnBIj9rn0w3Z8StPrcZ440Kip3QMq44GYmMvclWhILfpiC2bw=="
Content-Type: application/json
`{"amount":42}`
HTTP 200
[Asserts]
jsonpath "$.claims.digest_verified" == true
# ANCHOR_END: hmac_digest

# ANCHOR: hmac_fail
GET {{base_url}}/auth/hmac
HTTP 401
[Asserts]
header "WWW-Authenticate" contains "hmac realm=\"rustybin\""
# ANCHOR_END: hmac_fail
