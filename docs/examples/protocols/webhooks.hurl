# Webhook signatures (docs/src/reference/webhooks.md)

# ANCHOR: schemes
GET {{base_url}}/webhooks
HTTP 200
[Asserts]
jsonpath "$.outbound_delivery" == false
jsonpath "$.demo_secrets[0].id" == "demo"
jsonpath "$.demo_secrets[0].secret" == "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"
# ANCHOR_END: schemes

# ANCHOR: verify_standard
# The Standard Webhooks test vector (its timestamp is old, so skip the replay check).
POST {{base_url}}/webhooks/verify?ignore_timestamp=true
X-Webhook-Secret: whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw
webhook-id: msg_p5jXN8AQM9LWM0D4loKWxJek
webhook-timestamp: 1614265330
webhook-signature: v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=
Content-Type: application/json
`{"test": 2432232314}`
HTTP 200
[Asserts]
jsonpath "$.valid" == true
jsonpath "$.signature_valid" == true
# ANCHOR_END: verify_standard

# ANCHOR: verify_stale
POST {{base_url}}/webhooks/verify
X-Webhook-Secret: whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw
webhook-id: msg_p5jXN8AQM9LWM0D4loKWxJek
webhook-timestamp: 1614265330
webhook-signature: v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=
Content-Type: application/json
`{"test": 2432232314}`
HTTP 401
[Asserts]
jsonpath "$.error" == "timestamp_too_old"
jsonpath "$.signature_valid" == true
jsonpath "$.expected_signature" == "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
# ANCHOR_END: verify_stale

# ANCHOR: verify_github
POST {{base_url}}/webhooks/verify/github
X-Webhook-Secret: demo-secret
X-Hub-Signature-256: sha256=a30169f10c965c2302e48cf57ce4d4b80cc383086b1f62f3ebeb54d21bab149a
Content-Type: application/json
`{"zen":"Keep it logically awesome."}`
HTTP 200
[Asserts]
jsonpath "$.valid" == true
# ANCHOR_END: verify_github

# ANCHOR: verify_stripe
POST {{base_url}}/webhooks/verify/stripe?ignore_timestamp=true
X-Webhook-Secret: whsec_test_secret
Stripe-Signature: t=1700000000,v1=13941114bb88ac44a76abcfddea5b92aa6182a4b63d8be3aae908a616083bd7e
Content-Type: application/json
`{"id":"evt_test"}`
HTTP 200
[Asserts]
jsonpath "$.valid" == true
# ANCHOR_END: verify_stripe

# ANCHOR: sign_and_receive
# Ask Rustybin to sign a payload with the `demo` secret...
POST {{base_url}}/webhooks/sign
Content-Type: application/json
`{"type":"order.created","id":"ord_42"}`
HTTP 200
[Captures]
wh_id: jsonpath "$.headers['webhook-id']"
wh_ts: jsonpath "$.headers['webhook-timestamp']"
wh_sig: jsonpath "$.headers['webhook-signature']"
[Asserts]
jsonpath "$.scheme" == "standard-webhooks"
jsonpath "$.receive_path" == "/webhooks/receive/demo"
jsonpath "$.curl" contains "{{base_url}}/webhooks/verify"

# ...then deliver it (for example through a gateway) to the demo receiver.
POST {{base_url}}/webhooks/receive/demo
webhook-id: {{wh_id}}
webhook-timestamp: {{wh_ts}}
webhook-signature: {{wh_sig}}
Content-Type: application/json
`{"type":"order.created","id":"ord_42"}`
HTTP 204
# ANCHOR_END: sign_and_receive

# ANCHOR: receive_tampered
POST {{base_url}}/webhooks/receive/demo
X-Hub-Signature-256: sha256=0000000000000000000000000000000000000000000000000000000000000000
Content-Type: application/json
`{"type":"order.created"}`
HTTP 401
[Asserts]
jsonpath "$.error" == "signature_mismatch"
# ANCHOR_END: receive_tampered
