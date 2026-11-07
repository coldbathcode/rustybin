use serde_json::json;

use super::*;
use crate::mocks::compile::tests::compile;
use crate::mocks::model::{McpSettings, MockSettings};
use crate::mocks::{sample_settings, SAMPLE_SPEC};

fn tools_of(spec: &str, settings: MockSettings) -> ToolSet {
    let c = compile(spec, settings);
    (*c.mcp_tools()).clone()
}

fn names(set: &ToolSet) -> Vec<&str> {
    set.tools.iter().map(|t| t.name.as_str()).collect()
}

#[test]
fn petstore_has_one_tool_per_operation_in_spec_order() {
    let set = tools_of(SAMPLE_SPEC, sample_settings());
    assert_eq!(
        names(&set),
        vec![
            "listPets",
            "createPet",
            "getPetById",
            "updatePet",
            "deletePet",
            "placeOrder"
        ]
    );
    assert!(set.skipped.is_empty());

    let list = set.find("listPets").expect("listPets");
    assert_eq!(list.title.as_deref(), Some("List pets"));
    assert!(list.description.ends_with("GET /pets"));
    assert!(list.read_only && list.idempotent && !list.destructive);
    let props = &list.input_schema["properties"];
    assert_eq!(props["limit"]["type"], "integer");
    assert!(props["limit"]["description"]
        .as_str()
        .is_some_and(|d| d.starts_with("How many pets")));
    // The $ref is inlined.
    assert_eq!(
        props["status"]["enum"],
        json!(["available", "pending", "sold"])
    );
    assert_eq!(list.input_schema["required"], json!([]));
    assert_eq!(list.input_schema["additionalProperties"], json!(false));
    assert_eq!(
        list.output_schema.as_ref().expect("output")["properties"]["body"]["type"],
        "array"
    );

    let get = set.find("getPetById").expect("getPetById");
    assert_eq!(get.input_schema["required"], json!(["petId"]));
    assert_eq!(get.args.len(), 1);
    assert_eq!(get.args[0].location, Location::Path);

    let create = set.find("createPet").expect("createPet");
    assert!(!create.read_only && !create.idempotent && !create.destructive);
    assert_eq!(create.input_schema["required"], json!(["body"]));
    let body = &create.input_schema["properties"]["body"];
    assert_eq!(body["required"], json!(["name"]));
    assert!(body["description"]
        .as_str()
        .is_some_and(|d| d.starts_with("Request body (application/json)")));
    assert_eq!(create.body.as_ref().map(|b| b.required), Some(true));
    // 201 is the default status; its schema (Pet) keeps the readOnly id.
    let out = create.output_schema.as_ref().expect("output");
    assert!(out.to_string().contains("\"id\""));

    let delete = set.find("deletePet").expect("deletePet");
    assert!(delete.destructive && delete.idempotent);
    assert!(delete.output_schema.is_none(), "204 has no body");

    // readOnly properties are not request arguments.
    let order = set.find("placeOrder").expect("placeOrder");
    let order_body = &order.input_schema["properties"]["body"]["properties"];
    assert!(order_body.get("id").is_none());
    assert!(order_body.get("status").is_none());
    assert!(order_body.get("petId").is_some());
    assert!(!order.input_schema.to_string().contains("$ref"));
}

const FIXTURE: &str = r##"{
  "openapi": "3.0.3", "info": {"title": "Fixture", "version": "1"},
  "paths": {
    "/items/{id}": {
      "get": {
        "operationId": "get~item",
        "summary": "Get\u0007 an item",
        "deprecated": true,
        "parameters": [
          {"name": "id", "in": "path", "required": true, "schema": {"type": "string"}},
          {"name": "id", "in": "query", "schema": {"type": "integer"}, "description": "Filter"},
          {"name": "X-Trace", "in": "header", "schema": {"type": "string"}},
          {"name": "Authorization", "in": "header", "schema": {"type": "string"}},
          {"name": "X-Key", "in": "header", "schema": {"type": "string"}},
          {"name": "session", "in": "cookie", "schema": {"type": "string"}}
        ],
        "responses": {"200": {"description": "ok", "content": {"text/plain": {"schema": {"type": "string"}}}}}
      },
      "put": {
        "operationId": "get_item",
        "parameters": [{"name": "body", "in": "query", "schema": {"type": "string"}}],
        "requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Node"}}}},
        "responses": {"200": {"description": "ok"}}
      }
    },
    "/upload": {
      "post": {
        "operationId": "upload",
        "requestBody": {"required": true, "content": {"multipart/form-data": {"schema": {"type": "object"}}}},
        "responses": {"201": {"description": "ok"}}
      }
    },
    "/hidden": {
      "get": {"operationId": "hidden", "responses": {"200": {"description": "ok"}}}
    },
    "/plain": {
      "get": {"responses": {"200": {"description": "ok"}}}
    }
  },
  "components": {
    "securitySchemes": {"key": {"type": "apiKey", "in": "header", "name": "X-Key"}},
    "schemas": {
      "Node": {"type": "object", "properties": {
        "name": {"type": "string"},
        "children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}
      }}
    }
  }
}"##;

#[test]
fn names_are_sanitised_and_collisions_numbered() {
    let settings: MockSettings =
        serde_json::from_value(json!({ "mcp": { "exclude": ["hidden"] } })).expect("settings");
    let set = tools_of(FIXTURE, settings);
    // "get~item" sanitises to get_item, which the second operation's id
    // then collides with; the synthesised id of /plain is used as is.
    assert_eq!(names(&set), vec!["get_item", "get_item_2", "get_plain"]);
    assert!(set
        .find("get_item_2")
        .is_some_and(|t| t.operation == "get_item"));
    let reasons: Vec<(&str, &str)> = set
        .skipped
        .iter()
        .map(|s| (s.operation.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(reasons.len(), 2);
    assert_eq!(reasons[0].0, "upload");
    assert!(reasons[0].1.contains("multipart/form-data"));
    assert_eq!(reasons[1].0, "hidden");
    assert!(reasons[1].1.contains("mcp.exclude"));
}

#[test]
fn parameters_map_to_flat_arguments() {
    let set = tools_of(FIXTURE, MockSettings::default());
    let get = set.find("get_item").expect("tool");
    let arg_names: Vec<&str> = get.args.iter().map(|a| a.name.as_str()).collect();
    // Authorization and the API key header of the security scheme are not
    // arguments; the second "id" is renamed by location.
    assert_eq!(arg_names, vec!["id", "query_id", "X-Trace", "session"]);
    let query_id = &get.input_schema["properties"]["query_id"];
    assert!(query_id["description"]
        .as_str()
        .is_some_and(|d| d.contains("Query parameter \"id\"") && d.starts_with("Filter")));
    assert_eq!(get.args[1].param, "id");
    assert_eq!(get.args[1].location, Location::Query);
    assert_eq!(get.input_schema["required"], json!(["id"]));
    // Control characters are removed, deprecation comes first.
    assert_eq!(get.title.as_deref(), Some("Get an item"));
    assert!(get.description.starts_with("Deprecated. Get an item"));
    assert!(get.deprecated);
    // A text/plain response has no output schema.
    assert!(get.output_schema.is_none());

    let put = set.find("get_item_2").expect("tool");
    // "body" is the request body; the query parameter of that name moves.
    let arg_names: Vec<&str> = put.args.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(arg_names, vec!["id", "query_body"]);
    assert_eq!(put.input_schema["required"], json!(["id"]));
    let body = &put.input_schema["properties"]["body"];
    let children = &body["properties"]["children"]["items"];
    assert!(children["description"]
        .as_str()
        .is_some_and(|d| d.contains("Recursive")));
}

#[test]
fn long_names_and_descriptions_are_capped() {
    let long_id = "x".repeat(100);
    let long_text = "word ".repeat(400);
    let spec = json!({
        "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
        "paths": {
            "/a": {"get": {"operationId": long_id, "summary": long_text, "description": long_text,
                           "responses": {"200": {"description": "ok"}}}},
            "/b": {"get": {"operationId": long_id.clone() + "y", "responses": {"200": {"description": "ok"}}}}
        }
    })
    .to_string();
    let set = tools_of(&spec, MockSettings::default());
    assert_eq!(set.tools[0].name.len(), MAX_NAME);
    assert_eq!(set.tools[1].name.len(), MAX_NAME);
    assert!(set.tools[1].name.ends_with("_2"));
    assert!(set.tools[0].description.chars().count() <= MAX_DESCRIPTION);
    assert!(set.tools[0].description.ends_with("..."));
    assert!(set.tools[0]
        .title
        .as_ref()
        .is_some_and(|t| t.chars().count() <= MAX_TITLE));
}

#[test]
fn tools_are_cached_per_revision() {
    let c = compile(SAMPLE_SPEC, sample_settings());
    let a = c.mcp_tools();
    let b = c.mcp_tools();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}

#[test]
fn settings_round_trip_and_defaults() {
    let s = MockSettings::default();
    assert!(s.mcp.enabled);
    // The default block is not written out.
    let v = serde_json::to_value(&s).expect("json");
    assert!(v.get("mcp").is_none());
    let off: MockSettings =
        serde_json::from_value(json!({ "mcp": { "enabled": false } })).expect("settings");
    assert!(!off.mcp.enabled);
    assert_eq!(
        serde_json::to_value(&off).expect("json")["mcp"],
        json!({ "enabled": false })
    );
    let bad: Result<MockSettings, _> = serde_json::from_value(json!({ "mcp": { "nope": 1 } }));
    assert!(bad.is_err());
    let limits = crate::mocks::compile::tests::LIMITS.settings;
    let too_many = MockSettings {
        mcp: McpSettings {
            enabled: true,
            exclude: vec!["x".into(); crate::mocks::model::MAX_MCP_EXCLUDE + 1],
        },
        ..MockSettings::default()
    };
    assert!(too_many.validate(&limits).is_err());
}

/// Many operations that all name one wide schema: a small document whose
/// expansion is large (each tool holds its own copy).
fn one_wide_schema_everywhere(operations: usize, width: usize) -> String {
    let properties: serde_json::Map<String, Value> = (0..width)
        .map(|i| (format!("p{i}"), json!({ "type": "integer" })))
        .collect();
    let wide = json!({ "$ref": "#/components/schemas/Wide" });
    let paths: serde_json::Map<String, Value> = (0..operations)
        .map(|i| {
            let op = json!({ "post": {
                "operationId": format!("op{i}"),
                "requestBody": { "content": { "application/json": { "schema": wide } } },
                "responses": { "200": { "description": "ok",
                    "content": { "application/json": { "schema": wide } } } },
            }});
            (format!("/x{i}"), op)
        })
        .collect();
    json!({
        "openapi": "3.0.3",
        "info": { "title": "wide", "version": "1" },
        "paths": paths,
        "components": { "schemas": { "Wide": { "type": "object", "properties": properties } } },
    })
    .to_string()
}

fn nodes_in(v: &Value) -> usize {
    match v {
        Value::Object(o) => 1 + o.values().map(nodes_in).sum::<usize>(),
        Value::Array(a) => a.iter().map(nodes_in).sum(),
        _ => 0,
    }
}

fn visitor_compile(spec: &str) -> Compiled {
    let limits = crate::mocks::compile::CompileLimits {
        parse: crate::mocks::spec::ParseLimits {
            max_bytes: 1024 * 1024,
            max_operations: 500,
        },
        settings: crate::mocks::model::SettingsLimits {
            max_delay_ms: 10_000,
            max_body_bytes: 64 * 1024,
            max_rules: 10,
            visitor: true,
            stateful: false,
        },
    };
    Compiled::build(
        1,
        std::sync::Arc::from(spec),
        MockSettings::default(),
        &limits,
    )
    .expect("compiles")
}

#[test]
fn a_visitors_tools_expand_a_bounded_number_of_schema_nodes() {
    // 1,000 properties in 100 operations, twice each: 200,000 nodes if every
    // tool kept its own copy of the schema.
    let c = visitor_compile(&one_wide_schema_everywhere(100, 1_000));
    let set = c.mcp_tools();
    assert_eq!(set.tools.len(), 100, "every operation stays a tool");
    assert!(set.nodes <= VISITOR_TOOL_NODES, "spent {}", set.nodes);
    let held: usize = set
        .tools
        .iter()
        .map(|t| nodes_in(&t.input_schema) + t.output_schema.as_ref().map_or(0, nodes_in))
        .sum();
    assert!(held < 2 * VISITOR_TOOL_NODES, "held {held} nodes");
    // The first tool keeps its schema, the last says what happened to it.
    let first = set.tools.first().expect("tool");
    assert!(first.input_schema["properties"]["body"]["properties"]["p0"].is_object());
    let last = set.tools.last().expect("tool");
    assert!(
        last.input_schema.to_string().contains("Not expanded"),
        "{}",
        last.input_schema
    );
}
