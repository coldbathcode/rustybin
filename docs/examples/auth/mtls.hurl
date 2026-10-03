# mTLS, header mode (docs/src/reference/auth.md)
# run.sh starts the server with RUSTYBIN_MTLS_IN_HEADER=X-Client-Cert, the way a
# gateway that terminates mTLS forwards the client certificate.

# ANCHOR: get_client_cert
GET {{base_url}}/auth/mtls/get-client-cert
HTTP 200
[Captures]
client_cert: jsonpath "$.cert_pem" urlEncode
[Asserts]
jsonpath "$.cert_pem" startsWith "-----BEGIN CERTIFICATE-----"
jsonpath "$.key_pem" contains "PRIVATE KEY"
# ANCHOR_END: get_client_cert

# ANCHOR: get_ca
GET {{base_url}}/auth/mtls/get-ca-cert
HTTP 200
[Asserts]
jsonpath "$.ca_cert_pem" startsWith "-----BEGIN CERTIFICATE-----"
# ANCHOR_END: get_ca

# ANCHOR: header_mode
GET {{base_url}}/auth/mtls
X-Client-Cert: {{client_cert}}
HTTP 200
[Asserts]
jsonpath "$.auth_type" == "mtls"
jsonpath "$.header" == "X-Client-Cert"
jsonpath "$.client_dn" contains "CN="
jsonpath "$.claims.source" == "header"
jsonpath "$.claims.sha256_fingerprint" exists
# ANCHOR_END: header_mode

# ANCHOR: header_missing
GET {{base_url}}/auth/mtls
HTTP 401
[Asserts]
jsonpath "$.error" contains "X-Client-Cert"
# ANCHOR_END: header_missing
