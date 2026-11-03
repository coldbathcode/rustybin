# The dev portal (docs/src/portal.md): off until something is published,
# publish items, the catalogue testers read, a download, the counters, a
# closed item, the portal behind a credential, unpublish. Runs against the open instance of
# docs/examples/run.sh, whose admin token guards changes; the plan instance
# has nothing published.

# ANCHOR: off
GET {{plan_url}}/portal/api/catalog
HTTP 404
[Asserts]
jsonpath "$.error" == "the portal is off"
# ANCHOR_END: off

# ANCHOR: items
GET {{base_url}}/_rustybin/portal
HTTP 200
[Asserts]
jsonpath "$.mode" == "auto"
jsonpath "$.items[?(@.id == 'mock:inventory')].kind" contains "api"
jsonpath "$.items[?(@.id == 'mcp:main')].state" contains "serving"
jsonpath "$.items[?(@.id == 'a2a:weather')].title" contains "Weather Agent"
# ANCHOR_END: items

# ANCHOR: publish_mock
PUT {{base_url}}/_rustybin/portal/items/mock:inventory
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "title": "Inventory",
  "summary": "Stock levels by SKU. **SKU-0** is always sold out.",
  "tags": ["retail", "demo"],
  "contact": "platform-team@example.com"
}
HTTP 201
[Asserts]
jsonpath "$.published.title" == "Inventory"
jsonpath "$.published.credentials" == true
jsonpath "$.hidden" == null
# ANCHOR_END: publish_mock

# ANCHOR: publish_more
PUT {{base_url}}/_rustybin/portal/items/mcp:main
Authorization: Bearer {{admin_token}}
HTTP 201

PUT {{base_url}}/_rustybin/portal/items/llm
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "summary": "Point your SDK at these base URLs.", "order": 1 }
HTTP 201
# ANCHOR_END: publish_more

# ANCHOR: catalog
GET {{base_url}}/portal/api/catalog
HTTP 200
[Asserts]
header "ETag" exists
jsonpath "$.portal.title" == "API portal"
jsonpath "$.kinds[*].id" contains "api"
jsonpath "$.items" count == 3
jsonpath "$.items[0].id" == "mock:inventory"
jsonpath "$.items[0].detail.base_url" endsWith "/mock/inventory"
jsonpath "$.items[0].detail.security.api_key.header" == "x-api-key"
jsonpath "$.items[0].detail.operations[?(@.id == 'getItem')].curl" contains "x-api-key: docs-inventory-key"
jsonpath "$.items[?(@.id == 'mcp:main')].detail.claude" contains "claude mcp add --transport http rustybin-main"
jsonpath "$.items[?(@.id == 'llm')].detail.providers[0].base_url" endsWith "/ai/openai/v1"
[Captures]
etag: header "ETag"
# ANCHOR_END: catalog

# ANCHOR: not_modified
GET {{base_url}}/portal/api/catalog
If-None-Match: {{etag}}
HTTP 304
# ANCHOR_END: not_modified

# ANCHOR: page
GET {{base_url}}/portal/
HTTP 200
[Asserts]
header "Content-Type" startsWith "text/html"
body contains "<meta name=\"rustybin-portal-auth\" content=\"open\">"
# ANCHOR_END: page

# ANCHOR: download
GET {{base_url}}/portal/api/items/mock:inventory/collections/hurl
HTTP 200
[Asserts]
header "Content-Disposition" contains "inventory.hurl"
body contains "/mock/inventory"
# ANCHOR_END: download

# ANCHOR: reference
GET {{base_url}}/portal/reference/mock:inventory
HTTP 200
[Asserts]
header "Content-Type" startsWith "text/html"
header "Content-Security-Policy" == "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox"
body contains "api-reference"
body contains "integrity=\"sha384-"
body contains "/mock/inventory"
# ANCHOR_END: reference

# ANCHOR: event
POST {{base_url}}/portal/api/events
Content-Type: application/json
{ "item": "mock:inventory", "event": "view" }
HTTP 204
# ANCHOR_END: event

# ANCHOR: insights
GET {{base_url}}/_rustybin/portal/insights
HTTP 200
[Asserts]
jsonpath "$.items[?(@.id == 'mock:inventory')].views_7d" == 1
jsonpath "$.items[?(@.id == 'mock:inventory')].downloads.hurl" == 1
# ANCHOR_END: insights

# ANCHOR: close
# Closed: everyone reads it, only portal readers use it.
PUT {{base_url}}/_rustybin/portal/items/mock:inventory
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "access": "closed" }
HTTP 200
[Asserts]
jsonpath "$.published.access" == "closed"
jsonpath "$.secured" == true
# ANCHOR_END: close

# ANCHOR: locked
GET {{base_url}}/portal/api/catalog
HTTP 200
[Asserts]
jsonpath "$.items[0].id" == "mock:inventory"
jsonpath "$.items[0].access" == "closed"
jsonpath "$.items[0].locked" == true
jsonpath "$.items[0].detail.operations[?(@.id == 'getItem')].curl" contains "x-api-key: <api-key>"
jsonpath "$.items[0].detail.downloads" count == 0
body not contains "docs-inventory-key"

GET {{base_url}}/portal/api/items/mock:inventory/collections/hurl
HTTP 401
[Asserts]
jsonpath "$.reason" == "signin_required"

# The calls need the credentials too.
GET {{base_url}}/mock/inventory/items/SKU-1
HTTP 401
# ANCHOR_END: locked

# ANCHOR: reader
# A script keeps a header: a bearer control credential with portal.read.
GET {{base_url}}/portal/api/catalog
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.portal.reader" == true
jsonpath "$.items[0].locked" == false
body contains "docs-inventory-key"

GET {{base_url}}/portal/api/items/mock:inventory/collections/hurl
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: reader

# ANCHOR: security_off
POST {{base_url}}/_rustybin/mocks
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "name": "docs-open-api",
  "deploy": true,
  "spec": {
    "openapi": "3.0.3",
    "info": {"title": "Quotes", "version": "1.0"},
    "paths": {"/quotes": {"get": {"operationId": "listQuotes",
      "responses": {"200": {"description": "Quotes", "content": {"application/json": {"example": [{"id": 1}]}}}}}}}
  }
}
HTTP 201

# No mocked security: closing it is refused, naming the fix.
PUT {{base_url}}/_rustybin/portal/items/mock:docs-open-api
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "access": "closed" }
HTTP 409
[Asserts]
jsonpath "$.reason" == "security_off"

# secure: true turns it on (api_key: the spec declares none), deploys it and closes it.
PUT {{base_url}}/_rustybin/portal/items/mock:docs-open-api
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "access": "closed", "secure": true }
HTTP 201
[Asserts]
jsonpath "$.secured_now" == true
jsonpath "$.published.access" == "closed"

GET {{base_url}}/mock/docs-open-api/quotes
HTTP 401

GET {{base_url}}/mock/docs-open-api/quotes
x-api-key: demo-key
HTTP 200
# ANCHOR_END: security_off

DELETE {{base_url}}/_rustybin/portal/items/mock:docs-open-api
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/mocks/docs-open-api
Authorization: Bearer {{admin_token}}
HTTP 200

PUT {{base_url}}/_rustybin/portal/items/mock:inventory
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "access": "open" }
HTTP 200

# ANCHOR: password
PATCH {{base_url}}/_rustybin/settings
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "values": { "portal.auth": "password" } }
HTTP 200

GET {{base_url}}/portal/api/catalog
HTTP 401
[Asserts]
jsonpath "$.auth" == "password"

GET {{base_url}}/portal/api/catalog
Authorization: Bearer {{admin_token}}
HTTP 200

GET {{base_url}}/portal/
HTTP 200
# ANCHOR_END: password

PATCH {{base_url}}/_rustybin/settings
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "values": { "portal.auth": null } }
HTTP 200

# ANCHOR: unpublish
DELETE {{base_url}}/_rustybin/portal/items/llm
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
jsonpath "$.published" == false
# ANCHOR_END: unpublish

DELETE {{base_url}}/_rustybin/portal/items/mcp:main
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/portal/items/mock:inventory
Authorization: Bearer {{admin_token}}
HTTP 200

GET {{base_url}}/portal/api/catalog
HTTP 404
