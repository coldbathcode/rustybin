# Multi-step orchestration pipeline (docs/src/reference/orchestration.md)

# ANCHOR: pipeline
# Step 1: authenticate (any non-empty X-Api-Key) and get a correlation id.
POST {{base_url}}/orchestration/step/1
X-Api-Key: demo-key
Content-Type: application/json
{"merchant_id": "merchant_123"}
HTTP 200
[Captures]
correlation_id: jsonpath "$.correlation_id"
[Asserts]
jsonpath "$.status" == "success"
jsonpath "$.permissions" contains "card_payment"

# Step 2: enrich. The risk score is a stable hash of the inputs.
POST {{base_url}}/orchestration/step/2
X-Correlation-Id: {{correlation_id}}
Content-Type: application/json
{"merchant_id": "merchant_123", "card_number": "4111111111111111", "amount": 99.99}
HTTP 200
[Captures]
risk_score: jsonpath "$.enrichment.risk_score"
[Asserts]
jsonpath "$.correlation_id" == "{{correlation_id}}"
jsonpath "$.enrichment.risk_threshold" == 70

# Step 3: validate, passing the enrichment result along.
POST {{base_url}}/orchestration/step/3
X-Correlation-Id: {{correlation_id}}
Content-Type: application/json
{"merchant_id": "merchant_123", "amount": 99.99, "currency": "USD", "risk_score": {{risk_score}}}
HTTP 200
[Asserts]
jsonpath "$.validation.risk_score" == {{risk_score}}
jsonpath "$.validation.risk_score_source" exists
jsonpath "$.validation.result" matches /^(approved|declined)$/
# ANCHOR_END: pipeline

# ANCHOR: approved
# A low risk score is approved; step 4 then processes the payment.
POST {{base_url}}/orchestration/step/3
X-Correlation-Id: demo-corr-1
Content-Type: application/json
{"merchant_id": "merchant_123", "amount": 99.99, "currency": "USD", "risk_score": 15}
HTTP 200
[Asserts]
jsonpath "$.validation.result" == "approved"

POST {{base_url}}/orchestration/step/4
X-Correlation-Id: demo-corr-1
X-Validation-Result: approved
Content-Type: application/json
{"merchant_id": "merchant_123", "amount": 99.99, "currency": "USD"}
HTTP 200
[Asserts]
jsonpath "$.transaction.status" == "completed"
jsonpath "$.transaction.processor_response.code" == "00"
# ANCHOR_END: approved

# ANCHOR: declined
POST {{base_url}}/orchestration/step/3
X-Correlation-Id: demo-corr-2
Content-Type: application/json
{"merchant_id": "merchant_123", "amount": 99.99, "risk_score": 85}
HTTP 200
[Asserts]
jsonpath "$.validation.approved" == false
jsonpath "$.validation.result" == "declined"

POST {{base_url}}/orchestration/step/4
X-Correlation-Id: demo-corr-2
X-Validation-Result: declined
Content-Type: application/json
{"amount": 99.99, "currency": "USD"}
HTTP 403
# ANCHOR_END: declined

# ANCHOR: missing_header
POST {{base_url}}/orchestration/step/1
Content-Type: application/json
{"merchant_id": "merchant_123"}
HTTP 401
# ANCHOR_END: missing_header
