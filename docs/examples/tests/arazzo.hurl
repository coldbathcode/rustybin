# Multi-step tests and Arazzo (docs/src/saved-tests.md): a multi-step test
# written out, an Arazzo document previewed, imported, run and exported.

# ANCHOR: steps
PUT {{base_url}}/_rustybin/tests/docs-two-steps
Authorization: Bearer {{admin_token}}
Content-Type: application/json
file,two-steps.json;
HTTP 200
[Asserts]
jsonpath "$.steps" count == 2
jsonpath "$.variables[0]" == "PET_NAME"
# ANCHOR_END: steps

# ANCHOR: run_steps
POST {{base_url}}/_rustybin/tests/run
Authorization: Bearer {{admin_token}}
{"names": ["docs-two-steps"], "variables": {"PET_NAME": "Rex"}}
HTTP 200
[Asserts]
jsonpath "$.passed" == true
jsonpath "$.results[0].steps[0].id" == "create"
jsonpath "$.results[0].steps[0].outputs.id" == "4"
jsonpath "$.results[0].steps[1].status" == 200
# ANCHOR_END: run_steps

# ANCHOR: preview
POST {{base_url}}/_rustybin/tests/import?dry_run=true
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,pets.arazzo.yaml;
HTTP 200
[Asserts]
jsonpath "$.dry_run" == true
jsonpath "$.sources.petstore" == "petstore"
jsonpath "$.tests[0].name" == "docs-create-and-read"
jsonpath "$.tests[0].steps[0].request.path" == "/mock/petstore/v1/pets"
jsonpath "$.tests[0].steps[1].request.path" endsWith "steps.create.outputs.id}}"
jsonpath "$.left_out[0]" startsWith "workflows.docs-create-and-read.steps.read.onFailure[0] (retry)"
# ANCHOR_END: preview

# ANCHOR: import
POST {{base_url}}/_rustybin/tests/import
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,pets.arazzo.yaml;
HTTP 201
[Asserts]
jsonpath "$.tests[0].name" == "docs-create-and-read"
jsonpath "$.tests[0].variables[0]" == "PETNAME"
# ANCHOR_END: import

# ANCHOR: run
POST {{base_url}}/_rustybin/tests/run
Authorization: Bearer {{admin_token}}
{"names": ["docs-create-and-read"], "variables": {"PETNAME": "Rex"}}
HTTP 200
[Asserts]
jsonpath "$.passed" == true
jsonpath "$.results[0].steps" count == 2
# ANCHOR_END: run

# ANCHOR: no_mock
POST {{base_url}}/_rustybin/tests/import
Authorization: Bearer {{admin_token}}
{
  "document": {
    "arazzo": "1.0.1",
    "info": {"title": "Billing", "version": "1"},
    "sourceDescriptions": [{"name": "billing", "url": "https://billing.example.com/openapi.yaml", "type": "openapi"}],
    "workflows": [{"workflowId": "pay", "steps": [{"stepId": "pay", "operationId": "createInvoice"}]}]
  }
}
HTTP 400
[Asserts]
jsonpath "$.error" startsWith "sourceDescriptions.billing: no mock on this instance is named billing"
# ANCHOR_END: no_mock

# ANCHOR: export_arazzo
GET {{base_url}}/_rustybin/tests/docs-create-and-read/export?format=arazzo
Authorization: Bearer {{admin_token}}
HTTP 200
[Asserts]
body contains "arazzo: 1.0.1"
body contains "operationId: createPet"
body contains "value: $steps.create.outputs.id"
# ANCHOR_END: export_arazzo

# ANCHOR: harness
POST {{base_url}}/_rustybin/harness/import
Authorization: Bearer {{admin_token}}
Content-Type: application/yaml
file,pets.arazzo.yaml;
HTTP 400
[Asserts]
jsonpath "$.error" contains "every request was left out"
jsonpath "$.error" contains "PETNAME"
# ANCHOR_END: harness

# ANCHOR: cleanup
DELETE {{base_url}}/_rustybin/tests/docs-create-and-read
Authorization: Bearer {{admin_token}}
HTTP 200

DELETE {{base_url}}/_rustybin/tests/docs-two-steps
Authorization: Bearer {{admin_token}}
HTTP 200
# ANCHOR_END: cleanup
