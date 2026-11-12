//! Multi-step tests and Arazzo (demo next step 07), against the built-in
//! petstore mock.

use super::*;

const ARAZZO: &str = r#"
arazzo: 1.0.1
info: { title: Pet journeys, version: 1.0.0 }
sourceDescriptions:
  - { name: petstore, url: ./petstore.yaml, type: openapi }
workflows:
  - workflowId: create-and-read
    summary: Create a pet, read it back by the id the create answered
    inputs:
      type: object
      properties: { petName: { type: string } }
    steps:
      - stepId: create
        operationId: createPet
        requestBody:
          contentType: application/json
          payload: { name: $inputs.petName, tag: dog }
        successCriteria:
          - condition: $statusCode == 201
          - context: $response.body
            condition: $.status == 'available'
            type: jsonpath
        outputs:
          id: $response.body#/id
          where: $response.header.Location
      - stepId: read
        operationId: getPetById
        parameters:
          - { name: petId, in: path, value: $steps.create.outputs.id }
          - { name: x-trace, in: header, value: "pet-{$steps.create.outputs.id}" }
        successCriteria:
          - condition: $statusCode == 200
          - condition: $response.body#/name == 'never matches'
            type: regex
        onFailure:
          - { name: again, type: goto, stepId: create }
          - { name: wait, type: retry, retryAfter: 1, retryLimit: 3 }
  - workflowId: nested
    steps:
      - stepId: inner
        workflowId: create-and-read
  - workflowId: by-path
    steps:
      - stepId: list
        operationPath: "{$sourceDescriptions.petstore.url}#/paths/~1pets/get"
        parameters:
          - { name: limit, in: query, value: 2 }
        successCriteria:
          - condition: $statusCode == 200
        onSuccess:
          - { name: done, type: end }
"#;

fn instance() -> (AppState, Router) {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    (state, app)
}

async fn import(app: &Router, body: Value) -> (StatusCode, Value) {
    call(app, "POST", IMPORT_PATH, "ops", Some(body)).await
}

#[tokio::test]
async fn a_two_step_test_passes_values_on_and_names_the_step_that_fails() {
    let (_state, app) = instance();
    let test = json!({
        "steps": [
            { "id": "create",
              "request": { "method": "POST", "path": "/mock/petstore/v1/pets", "headers": { "content-type": "application/json" }, "body": "{\"name\": \"{{PET}}\", \"tag\": \"dog\"}" },
              "assert": [{ "status": 201 }],
              "outputs": { "id": "$response.body#/id", "code": "$statusCode", "where": "$response.header.location", "api_token": "$response.body#/name" } },
            { "id": "read",
              "request": { "path": "/mock/petstore/v1/pets/{{steps.create.outputs.id}}", "headers": { "x-seen": "{{steps.create.outputs.code}}" } },
              "assert": [{ "status": 200 }] },
            { "id": "list", "request": { "path": "/mock/petstore/v1/pets" }, "assert": [{ "status": 200 }] }
        ]
    });
    let (s, v) = call(
        &app,
        "PUT",
        "/_rustybin/tests/pets",
        "ops",
        Some(test.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["variables"], json!(["PET"]), "{v}");
    let run = json!({ "names": ["pets"], "variables": { "PET": "Rex-secret-name" } });
    let (s, v) = call(&app, "POST", RUN_PATH, "ops", Some(run.clone())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["passed"], json!(true), "{v}");
    let steps = v["results"][0]["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["outputs"]["id"], json!("4"), "{v}");
    assert_eq!(steps[0]["outputs"]["code"], json!("201"));
    // An output named like a credential is never shown.
    assert_eq!(steps[0]["outputs"]["api_token"], json!("(hidden)"));
    assert_eq!(steps[1]["status"], json!(200));
    // A variable's value is in no result.
    assert!(!v.to_string().contains("Rex-secret-name"), "{v}");

    // The second step's path is wrong: the test fails, naming the step,
    // and the third step is not run.
    let mut wrong = test;
    wrong["steps"][1]["request"]["path"] = json!("/mock/petstore/v1/pets/0");
    let (s, v) = call(&app, "PUT", "/_rustybin/tests/pets", "ops", Some(wrong)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = call(&app, "POST", RUN_PATH, "ops", Some(run)).await;
    assert_eq!(v["passed"], json!(false), "{v}");
    let r = &v["results"][0];
    assert_eq!(
        r["failures"][0]["assertion"],
        json!("step read: status 200"),
        "{v}"
    );
    assert_eq!(r["failures"][0]["seen"], json!("404"));
    assert_eq!(r["steps"][0]["passed"], json!(true));
    assert_eq!(r["steps"][1]["passed"], json!(false));
    assert_eq!(r["steps"][2]["skipped"], json!(true));
    assert!(r["steps"][2]["status"].is_null());
    // JUnit names it too.
    let req = Request::builder()
        .method("POST")
        .uri(format!("{RUN_PATH}?format=junit"))
        .header("x-rustybin-session", "ops")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "variables": { "PET": "Rex" } }).to_string(),
        ))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    let xml = crate::test_support::body_string(resp).await;
    assert!(xml.contains("step read: status 200"), "{xml}");
}

#[test]
fn the_rules_of_a_multi_step_test() {
    let parse = |v: Value| -> Result<SavedTest, String> {
        test_from_value("t", v).and_then(|t| t.validate().map(|_| t))
    };
    let step = |id: &str, path: &str| json!({ "id": id, "request": { "path": path } });
    let ok = parse(json!({ "steps": [
        { "id": "sign-in", "request": { "method": "POST", "path": "/post" }, "outputs": { "token": "$response.body#/json/token" } },
        { "id": "use", "request": { "path": "/get", "headers": { "authorization": "Bearer {{steps.sign-in.outputs.token}}" } } }
    ] }))
    .expect("an earlier step's output is a credential's source");
    // What a document holds has no request, and back.
    let text = serde_json::to_string(&ok).expect("json");
    assert!(
        !text.contains("\"request\":{\"method\":\"GET\",\"path\":\"\""),
        "{text}"
    );
    assert_eq!(serde_json::from_str::<SavedTest>(&text).expect("back"), ok);
    assert!(ok.changes_state());
    for (says, bad) in [
        ("a request, or steps", json!({})),
        (
            "not both",
            json!({ "request": { "path": "/get" }, "steps": [step("a", "/get")] }),
        ),
        (
            "each step has its own assert",
            json!({ "assert": [{ "status": 200 }], "steps": [step("a", "/get")] }),
        ),
        (
            "steps[2].id",
            json!({ "steps": [step("a", "/get"), step("a", "/get")] }),
        ),
        ("steps[1].id", json!({ "steps": [step("a b", "/get")] })),
        (
            "steps[1].request.path",
            json!({ "steps": [step("a", "/_rustybin/settings")] }),
        ),
        (
            "is not an output of an earlier step",
            json!({ "steps": [step("a", "/get/{{steps.b.outputs.id}}"), { "id": "b", "request": { "path": "/get" }, "outputs": { "id": "$statusCode" } }] }),
        ),
        (
            "is not an output of an earlier step",
            json!({ "steps": [step("a", "/get"), step("b", "/get/{{steps.a.outputs.id}}")] }),
        ),
        (
            "steps[1].outputs.x",
            json!({ "steps": [{ "id": "a", "request": { "path": "/get" }, "outputs": { "x": "$url" } }] }),
        ),
        (
            "a credential is never saved",
            json!({ "steps": [{ "id": "a", "request": { "path": "/get", "headers": { "authorization": "Bearer abc" } } }] }),
        ),
    ] {
        let e = parse(bad).expect_err(says);
        assert!(e.contains(says), "{says}: {e}");
    }
    let many: Vec<Value> = (0..=MAX_STEPS)
        .map(|i| step(&format!("s{i}"), "/get"))
        .collect();
    assert!(parse(json!({ "steps": many }))
        .expect_err("many")
        .contains("at most 20"));
    // The name of the import route is not a test's.
    assert!(!valid_name("import"));
}

#[tokio::test]
async fn an_arazzo_document_becomes_tests_and_names_what_it_left_out() {
    let (state, app) = instance();
    let (s, v) = import(&app, json!({ "document": ARAZZO, "dry_run": true })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["workflows"], json!(3));
    assert_eq!(v["sources"], json!({ "petstore": "petstore" }));
    let names: Vec<&str> = v["tests"]
        .as_array()
        .expect("tests")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names, vec!["create-and-read", "by-path"], "{v}");
    assert!(state.saved_tests.is_empty(), "a dry run saves nothing");
    let left: Vec<&str> = v["left_out"]
        .as_array()
        .expect("left_out")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // The goto and the retry, the regex criterion, the nested workflow,
    // the end action: each by its place.
    for needle in [
        "workflows.create-and-read.steps.read.onFailure[0] (goto)",
        "workflows.create-and-read.steps.read.onFailure[1] (retry)",
        "workflows.create-and-read.steps.read.successCriteria[1]",
        "workflows.nested (not imported: workflows.nested.steps.inner: a nested workflow",
        "workflows.by-path.steps.list.onSuccess[0] (end)",
    ] {
        assert!(
            left.iter().any(|l| l.starts_with(needle)),
            "{needle}: {left:?}"
        );
    }
    // The step with the goto is imported: it runs without it.
    let t = &v["tests"][0];
    assert_eq!(t["steps"][0]["request"]["method"], json!("POST"));
    assert_eq!(
        t["steps"][0]["request"]["path"],
        json!("/mock/petstore/v1/pets")
    );
    assert_eq!(
        t["steps"][0]["request"]["body"],
        json!("{\"name\":\"{{PETNAME}}\",\"tag\":\"dog\"}")
    );
    assert_eq!(
        t["steps"][0]["assert"],
        json!([{ "status": 201 }, { "json": "$.status", "equals": "available" }])
    );
    assert_eq!(
        t["steps"][0]["outputs"]["where"],
        json!("$response.header.Location")
    );
    assert_eq!(
        t["steps"][1]["request"]["path"],
        json!("/mock/petstore/v1/pets/{{steps.create.outputs.id}}")
    );
    assert_eq!(
        t["steps"][1]["request"]["headers"]["x-trace"],
        json!("pet-{{steps.create.outputs.id}}")
    );
    assert_eq!(t["steps"][1]["assert"], json!([{ "status": 200 }]));
    assert_eq!(
        v["tests"][1]["steps"][0]["request"]["path"],
        json!("/mock/petstore/v1/pets?limit=2")
    );

    // Imported for real, the workflows run as tests.
    let (s, v) = import(&app, json!({ "document": ARAZZO })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["tests"][0]["variables"], json!(["PETNAME"]), "{v}");
    let (s, v) = call(
        &app,
        "POST",
        RUN_PATH,
        "ops",
        Some(json!({ "variables": { "PETNAME": "Rex" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["passed"], json!(true), "{v}");
    assert_eq!(v["total"], json!(2));
    // Again: the names are taken, unless replace.
    let (s, v) = import(&app, json!({ "document": ARAZZO })).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["tests"], json!(["create-and-read", "by-path"]));
    let (s, v) = import(&app, json!({ "document": ARAZZO, "replace": true, "gateway": "https://gateway.example.com/pets" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(
        state
            .saved_tests
            .get("by-path")
            .expect("t")
            .gateway
            .as_deref(),
        Some("https://gateway.example.com/pets")
    );
}

#[tokio::test]
async fn a_source_description_that_is_no_mock_is_refused_by_name() {
    let (state, app) = instance();
    let doc = ARAZZO.replace("name: petstore,", "name: billing,").replace(
        "$sourceDescriptions.petstore",
        "$sourceDescriptions.billing",
    );
    // Not by name, but one mock has the operations the workflows use.
    let (s, v) = import(&app, json!({ "document": doc, "dry_run": true })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["sources"], json!({ "billing": "petstore" }));
    // No mock has them: refused, with the source's name.
    let doc = doc.replace("createPet", "createInvoice");
    let (s, v) = import(&app, json!({ "document": doc })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let e = v["error"].as_str().expect("error");
    assert!(
        e.starts_with("sourceDescriptions.billing: no mock on this instance is named billing"),
        "{e}"
    );
    assert!(
        e.contains("createInvoice") && e.contains("never fetched"),
        "{e}"
    );
    assert!(state.saved_tests.is_empty());
    for (bad, says) in [
        (json!({ "openapi": "3.0.3" }), "not an Arazzo 1.0 document"),
        (
            json!({ "arazzo": "2.0.0", "workflows": [] }),
            "not an Arazzo 1.0 document",
        ),
        (
            json!({ "arazzo": "1.0.0", "sourceDescriptions": [{ "name": "petstore" }] }),
            "workflows",
        ),
    ] {
        let (s, v) = import(&app, json!({ "document": bad })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().expect("e").contains(says), "{v}");
    }
}

#[tokio::test]
async fn a_test_exports_to_hurl_with_captures_and_back_to_arazzo() {
    let (_state, app) = instance();
    let (s, v) = import(&app, json!({ "document": ARAZZO })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let text = |uri: &'static str| {
        let app = app.clone();
        async move {
            let req = Request::builder()
                .uri(uri)
                .header("x-rustybin-session", "ops")
                .header("host", "rustybin.test")
                .body(Body::empty())
                .expect("req");
            let resp = app.oneshot(req).await.expect("resp");
            let status = resp.status();
            let left = resp
                .headers()
                .get(LEFT_OUT_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            (status, crate::test_support::body_string(resp).await, left)
        }
    };
    let (s, hurl, _) = text("/_rustybin/tests/create-and-read/export").await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        hurl.contains("# step create\nPOST {{base_url}}/mock/petstore/v1/pets\n"),
        "{hurl}"
    );
    assert!(
        hurl.contains(
            "[Captures]\ncreate_id: jsonpath \"$.id\"\ncreate_where: header \"Location\"\n"
        ),
        "{hurl}"
    );
    assert!(
        hurl.contains(
            "GET {{base_url}}/mock/petstore/v1/pets/{{create_id}}\nx-trace: pet-{{create_id}}\n"
        ),
        "{hurl}"
    );
    assert!(
        hurl.contains("jsonpath \"$.status\" == \"available\""),
        "{hurl}"
    );
    // Every test in one file has the steps too.
    let (_, all, _) = text("/_rustybin/tests/export").await;
    assert!(all.contains("# step read"), "{all}");

    // Back to Arazzo: the same workflow, importable again.
    let (s, yaml, left) = text("/_rustybin/tests/create-and-read/export?format=arazzo").await;
    assert_eq!(s, StatusCode::OK, "{yaml}");
    assert!(left.is_none(), "{left:?}");
    let doc: Value = serde_yaml::from_str(&yaml).expect("yaml");
    assert_eq!(doc["arazzo"], json!("1.0.1"));
    assert_eq!(doc["sourceDescriptions"][0]["name"], json!("petstore"));
    assert!(
        doc["sourceDescriptions"][0]["url"]
            .as_str()
            .expect("url")
            .ends_with("/mock/petstore/_rustybin/openapi.json"),
        "{doc}"
    );
    let steps = &doc["workflows"][0]["steps"];
    assert_eq!(steps[0]["operationId"], json!("createPet"));
    assert_eq!(
        steps[0]["requestBody"]["payload"]["name"],
        json!("$inputs.PETNAME")
    );
    assert_eq!(
        steps[1]["parameters"][0],
        json!({ "name": "petId", "in": "path", "value": "$steps.create.outputs.id" })
    );
    assert_eq!(
        steps[1]["parameters"][1]["value"],
        json!("pet-{$steps.create.outputs.id}")
    );
    assert_eq!(
        doc["workflows"][0]["inputs"]["properties"]["PETNAME"]["type"],
        json!("string")
    );
    let before = call(&app, "GET", "/_rustybin/tests/create-and-read", "ops", None)
        .await
        .1;
    let (s, v) = import(&app, json!({ "document": yaml, "replace": true })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let after = call(&app, "GET", "/_rustybin/tests/create-and-read", "ops", None)
        .await
        .1;
    assert_eq!(
        before["steps"], after["steps"],
        "the export imports to the same steps"
    );

    // A single-request test of a mock exports; what Arazzo cannot say is named.
    let (s, _) = call(&app, "PUT", "/_rustybin/tests/one", "ops", Some(json!({ "request": { "path": "/mock/petstore/v1/pets/7" }, "assert": [{ "status": 200 }, { "max_ms": 500 }, { "body_contains": "id" }] }))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, yaml, left) = text("/_rustybin/tests/one/export?format=arazzo").await;
    assert_eq!(s, StatusCode::OK, "{yaml}");
    assert!(yaml.contains("operationId: getPetById"), "{yaml}");
    let left = left.expect("left out");
    assert!(
        left.contains("max_ms 500") && left.contains("body contains"),
        "{left}"
    );
    // One that calls no mock cannot be an Arazzo workflow.
    let (s, _) = call(
        &app,
        "PUT",
        "/_rustybin/tests/plain",
        "ops",
        Some(json!({ "request": { "path": "/status/200" } })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, body, _) = text("/_rustybin/tests/plain/export?format=arazzo").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("is not an operation of a mock"), "{body}");
    let (s, _, _) = text("/_rustybin/tests/nope/export").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_harness_imports_the_steps_that_stand_alone() {
    let (_state, app) = instance();
    let req = Request::builder()
        .method("POST")
        .uri("/_rustybin/harness/import")
        .header("content-type", "application/yaml")
        .body(Body::from(ARAZZO))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    let status = resp.status();
    let v = crate::test_support::body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["format"], json!("arazzo"), "{v}");
    // The list needs nothing passed on: it is a request template. The
    // create needs an input and the read an earlier step's output.
    let requests = v["source"]["requests"].as_array().expect("requests");
    assert_eq!(requests.len(), 1, "{v}");
    assert_eq!(requests[0]["path"], json!("/mock/petstore/v1/pets?limit=2"));
    assert_eq!(requests[0]["expect_status"], json!([200]));
    assert_eq!(
        v["variables"],
        json!(["PETNAME", "steps.create.outputs.id"]),
        "{v}"
    );
    assert!(
        v["left_out"].to_string().contains("onFailure[0] (goto)"),
        "{v}"
    );
    assert!(
        v["notes"]
            .to_string()
            .contains("not in the order of a workflow"),
        "{v}"
    );
    // Named, it is read as Arazzo; a source that is no mock is refused.
    let req = Request::builder()
        .method("POST")
        .uri("/_rustybin/harness/import?format=arazzo")
        .body(Body::from(
            ARAZZO
                .replace("name: petstore,", "name: billing,")
                .replace("createPet", "createInvoice")
                .replace(
                    "$sourceDescriptions.petstore",
                    "$sourceDescriptions.billing",
                ),
        ))
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = crate::test_support::body_json(resp).await;
    assert!(
        v["error"]
            .as_str()
            .expect("e")
            .contains("sourceDescriptions.billing"),
        "{v}"
    );
}
