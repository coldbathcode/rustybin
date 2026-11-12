# Event mocks from AsyncAPI (docs/src/mocks/asyncapi.md): an AsyncAPI
# document applied as a mock, its channels, a message published by Rustybin,
# messages published by a client (valid, invalid, answered with a reply), the
# channel's log, and a webhook subscriber the outbound policy refuses (the
# docs instance allows no outbound host).

DELETE {{base_url}}/_rustybin/mocks/order-events
Authorization: Bearer {{admin_token}}
HTTP *

# ANCHOR: apply
POST {{base_url}}/_rustybin/mocks/apply
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,order-events.yaml;
HTTP 200
[Asserts]
jsonpath "$.created" contains "order-events"
# ANCHOR_END: apply

# ANCHOR: channels
GET {{base_url}}/_rustybin/mocks/order-events/channels
HTTP 200
[Asserts]
jsonpath "$.asyncapi" == "3.0.0"
jsonpath "$.count" == 3
jsonpath "$.channels[0].id" == "orders-created"
jsonpath "$.channels[0].address" == "orders/created"
jsonpath "$.channels[0].pattern" == "every"
jsonpath "$.channels[0].settings.every_ms" == 500
jsonpath "$.channels[0].send[0].name" == "orderCreated"
jsonpath "$.channels[0].path" == "/events/order-events/orders-created"
jsonpath "$.channels[2].pattern" == "on_receive"
jsonpath "$.channels[2].sample.message" == "quoteRequest"
# ANCHOR_END: channels

# ANCHOR: describe
GET {{base_url}}/events/order-events/orders-created
HTTP 200
[Asserts]
jsonpath "$.address" == "orders/created"
jsonpath "$.pattern" == "every"
jsonpath "$.send[0].name" == "orderCreated"
# ANCHOR_END: describe

# The same channel below the mock's own root.
GET {{base_url}}/mock/order-events/_rustybin/events/orders-created
HTTP 200

# ANCHOR: publish
POST {{base_url}}/_rustybin/mocks/order-events/channels/orders-created/publish
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{}
HTTP 200
[Asserts]
jsonpath "$.published" == 1
jsonpath "$.messages[0].direction" == "sent"
jsonpath "$.messages[0].message" == "orderCreated"
jsonpath "$.messages[0].source" == "example"
jsonpath "$.messages[0].payload.id" == 1001
jsonpath "$.messages[0].delivered.ws" == 0
# ANCHOR_END: publish

# ANCHOR: publish_given
POST {{base_url}}/_rustybin/mocks/order-events/channels/orders-created/publish
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"payload": {"id": 4242, "total": 0.5, "status": "shipped"}}
HTTP 200
[Asserts]
jsonpath "$.messages[0].source" == "given"
jsonpath "$.messages[0].payload.id" == 4242
# ANCHOR_END: publish_given

# ANCHOR: client_valid
POST {{base_url}}/events/order-events/orders-cancel
Content-Type: application/json
{"id": 1001, "reason": "ordered twice"}
HTTP 202
[Asserts]
jsonpath "$.accepted" == true
jsonpath "$.message" == "orderCancel"
# ANCHOR_END: client_valid

# ANCHOR: client_invalid
POST {{base_url}}/events/order-events/orders-cancel
Content-Type: application/json
{"id": "1001", "reason": "no"}
HTTP 422
[Asserts]
jsonpath "$.error.type" == "invalid_message"
jsonpath "$.error.issues" count == 2
jsonpath "$.error.issues[0].pointer" == "/id"
# ANCHOR_END: client_invalid

# ANCHOR: reply
POST {{base_url}}/events/order-events/quotes
Content-Type: application/json
{"sku": "SKU-7", "quantity": 2}
HTTP 202
[Asserts]
jsonpath "$.message" == "quoteRequest"
jsonpath "$.reply.message" == "quote"
jsonpath "$.reply.payload.price" isNumber
# ANCHOR_END: reply

# ANCHOR: messages
GET {{base_url}}/_rustybin/mocks/order-events/channels/orders-cancel/messages?limit=10
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.count" == 2
jsonpath "$.messages[0].direction" == "received"
jsonpath "$.messages[0].transport" == "http"
jsonpath "$.messages[0].valid" == false
jsonpath "$.messages[0].issues[0].pointer" == "/id"
jsonpath "$.messages[1].valid" == true
jsonpath "$.messages[1].message" == "orderCancel"
# ANCHOR_END: messages

# ANCHOR: after
# Only what came after a sequence number: how a feed asks again.
GET {{base_url}}/_rustybin/mocks/order-events/channels/quotes/messages?after=4
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.messages[0].direction" == "sent"
jsonpath "$.messages[0].message" == "quote"
jsonpath "$.messages[1].direction" == "received"
# ANCHOR_END: after

# ANCHOR: subscribe_refused
POST {{base_url}}/_rustybin/mocks/order-events/subscribers
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{"channel": "orders-created", "url": "https://hooks.example.com/orders"}
HTTP 403
[Asserts]
jsonpath "$.reason" == "egress_refused"
jsonpath "$.purpose" == "events"
# ANCHOR_END: subscribe_refused

# ANCHOR: subscribers
GET {{base_url}}/_rustybin/mocks/order-events/subscribers
HTTP 200
[Asserts]
jsonpath "$.count" == 0
# ANCHOR_END: subscribers

# ANCHOR: unsubscribe
DELETE {{base_url}}/_rustybin/mocks/order-events/subscribers/0a1b2c3d4e5f6a7b
Authorization: Bearer {{admin_token}}
HTTP 404
# ANCHOR_END: unsubscribe

# ANCHOR: report
GET {{base_url}}/_rustybin/mocks/order-events/report
HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.channels" count == 3
jsonpath "$.channels[0].send[0].source" == "example"
jsonpath "$.channels[1].receive[0].message" == "orderCancel"
# ANCHOR_END: report

# ANCHOR: not_asyncapi
# An OpenAPI mock has operations, not channels.
GET {{base_url}}/_rustybin/mocks/petstore/channels
HTTP 409
[Asserts]
jsonpath "$.reason" == "not_asyncapi"
# ANCHOR_END: not_asyncapi

DELETE {{base_url}}/_rustybin/mocks/order-events
Authorization: Bearer {{admin_token}}
HTTP 200
