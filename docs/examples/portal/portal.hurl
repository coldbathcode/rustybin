# The dev portal (docs/src/portal.md): off until something is published,
# publish items, the catalogue testers read, a download, the counters, the
# portal behind a credential, unpublish. Runs against the open instance of
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
body contains "api-reference"
body contains "mock/inventory/_rustybin/openapi.json"
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
