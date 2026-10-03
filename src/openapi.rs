use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

// ── Module hooks ────────────────────────────────────────────────────

/// Per-module OpenAPI path fragments (objects of OpenAPI 3.0.3 path items).
/// New modules register `openapi_paths` here; existing hand-written paths
/// stay in [`build_paths`].
const MODULE_PATHS: &[fn() -> Value] = &[
    crate::inspector::openapi_paths,
    crate::control::openapi_paths,
    crate::a2a::openapi_paths,
];

/// Per-module OpenAPI components fragments (e.g. `{"schemas": {...}}`).
const MODULE_COMPONENTS: &[fn() -> Value] = &[crate::inspector::openapi_components];

/// Merge path items: new operations are added to existing paths.
fn merge_paths(paths: &mut serde_json::Map<String, Value>, fragment: Value) {
    let Value::Object(fragment) = fragment else {
        return;
    };
    for (path, item) in fragment {
        match (paths.get_mut(&path), item) {
            (Some(Value::Object(existing)), Value::Object(ops)) => {
                for (method, op) in ops {
                    existing.entry(method).or_insert(op);
                }
            }
            (_, item) => {
                paths.insert(path, item);
            }
        }
    }
}

/// Merge component sections (`schemas`, `securitySchemes`, ...).
fn merge_components(components: &mut Value, fragment: Value) {
    let (Some(target), Value::Object(fragment)) = (components.as_object_mut(), fragment) else {
        return;
    };
    for (section, entries) in fragment {
        let slot = target
            .entry(section)
            .or_insert_with(|| Value::Object(Default::default()));
        if let (Some(slot), Value::Object(entries)) = (slot.as_object_mut(), entries) {
            for (k, v) in entries {
                slot.entry(k).or_insert(v);
            }
        }
    }
}

/// Replace `{param}` / `{*param}` with `{}` for name-agnostic comparison.
fn normalize_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_param = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push_str("{}");
            }
            '}' => in_param = false,
            _ if in_param => {}
            _ => out.push(c),
        }
    }
    out
}

/// Make the documented methods match the catalogue: routes registered with
/// `any()` get get/post/put/patch/delete, and any catalogue method missing
/// from a path item is added as a copy of an existing operation.
fn align_methods_with_catalogue(paths: &mut serde_json::Map<String, Value>) {
    let index: std::collections::HashMap<String, String> = paths
        .keys()
        .map(|k| (normalize_path(k), k.clone()))
        .collect();
    for ep in crate::catalog::all() {
        let Some(key) = index.get(&normalize_path(ep.path)) else {
            continue;
        };
        let Some(Value::Object(item)) = paths.get_mut(key) else {
            continue;
        };
        let template = ["get", "post", "put", "patch", "delete"]
            .iter()
            .find_map(|m| item.get(*m).cloned());
        let Some(template) = template else {
            continue;
        };
        for method in ep.expanded_methods() {
            let m = method.to_ascii_lowercase();
            if item.contains_key(&m) {
                continue;
            }
            let mut op = template.clone();
            if let Some(obj) = op.as_object_mut() {
                if let Some(Value::String(id)) = obj.get("operationId").cloned() {
                    obj.insert("operationId".into(), Value::String(format!("{id}_{m}")));
                }
                if m == "get" || m == "delete" {
                    obj.remove("requestBody");
                }
            }
            item.insert(m, op);
        }
    }
}

// ── Spec builder ────────────────────────────────────────────────────

/// The complete OpenAPI 3.0.3 document.
pub fn build_spec() -> Value {
    let mut paths = match build_paths() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for fragment in MODULE_PATHS {
        merge_paths(&mut paths, fragment());
    }
    align_methods_with_catalogue(&mut paths);

    let mut components = build_components();
    for fragment in MODULE_COMPONENTS {
        merge_components(&mut components, fragment());
    }

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Rustybin",
            "description": "A high-performance, all-in-one HTTP stub service for API gateway testing. Built in Rust with axum. Designed to exercise every category of API gateway feature - auth, routing, transformation, rate limiting, AI proxying, and more.",
            "version": env!("CARGO_PKG_VERSION"),
            "contact": { "name": "Rustybin" }
        },
        "servers": [
            { "url": "http://localhost", "description": "HTTP" },
            { "url": "https://localhost", "description": "HTTPS / mTLS" }
        ],
        "tags": [
            { "name": "Utility", "description": "Health, identity, image, and flaky endpoints" },
            { "name": "Echo", "description": "Echo and anything endpoints" },
            { "name": "Status", "description": "HTTP status code responses" },
            { "name": "Response Shaping", "description": "Delay, cache, and response-headers" },
            { "name": "Redirects & Cookies", "description": "Redirect chains and cookie management" },
            { "name": "Info", "description": "IP, date, and time information" },
            { "name": "Random", "description": "UUID, random numbers, and lorem ipsum" },
            { "name": "Auth", "description": "Authentication endpoints (basic, api-key, JWT, mTLS, OIDC)" },
            { "name": "AI Gateway", "description": "OpenAI-compatible AI endpoints" },
            { "name": "GraphQL", "description": "GraphQL API with playground" },
            { "name": "Orchestration", "description": "Multi-step orchestration pipeline" },
            { "name": "SOAP", "description": "SOAP/XML web service" },
            { "name": "WebSocket", "description": "WebSocket echo and server-push endpoints" },
            { "name": "Control Plane", "description": "Request inspector, configuration and version under /_rustybin" }
        ],
        "paths": Value::Object(paths),
        "components": components
    })
}

fn build_paths() -> Value {
    let mut paths = serde_json::Map::new();

    // ── Utility ─────────────────────────────────────────────────
    paths.insert("/".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Landing page",
            "description": "HTML landing page with endpoint directory and quick-start examples.",
            "operationId": "getLanding",
            "responses": { "200": { "description": "HTML landing page", "content": { "text/html": {} } } }
        }
    }));

    paths.insert(
        "/health".into(),
        json!({
            "get": {
                "tags": ["Utility"],
                "summary": "Health check",
                "description": "Returns service health status with version and instance info.",
                "operationId": "getHealth",
                "responses": {
                    "200": {
                        "description": "Service is healthy",
                        "content": json_xml_content(json!({
                            "type": "object",
                            "properties": {
                                "status": { "type": "string", "example": "healthy" },
                                "service": { "type": "string", "example": "rustybin" },
                                "version": { "type": "string", "example": "0.1.0" },
                                "instance_id": { "type": "string", "format": "uuid" }
                            }
                        }))
                    }
                }
            }
        }),
    );

    paths.insert("/identity".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Instance identity",
            "description": "Returns hostname, instance ID, uptime, request count, and port information. Useful for load-balancer testing.",
            "operationId": "getIdentity",
            "responses": {
                "200": {
                    "description": "Instance identity information",
                    "content": json_xml_content(json!({ "$ref": "#/components/schemas/IdentityResponse" }))
                }
            }
        }
    }));

    // Health toggle (runtime liveness control for active health-check demos)
    for (path, op, summary, desc) in [
        ("/health/healthy", "markHealthy", "Mark instance healthy", "Sets the instance health state to healthy. Subsequent GET /health returns 200."),
        ("/health/unhealthy", "markUnhealthy", "Mark instance unhealthy", "Sets the instance health state to unhealthy. GET /health then returns 503 - useful for gateway upstream active health-check failover demos."),
        ("/health/toggle", "toggleHealth", "Toggle health state", "Flips the current health state between healthy and unhealthy."),
    ] {
        paths.insert(path.into(), json!({
            "post": {
                "tags": ["Utility"],
                "summary": summary,
                "description": desc,
                "operationId": op,
                "responses": {
                    "200": { "description": "Instance is now healthy", "content": json_xml_content(json!({ "type": "object" })) },
                    "503": { "description": "Instance is now unhealthy", "content": json_xml_content(json!({ "type": "object" })) }
                }
            }
        }));
    }

    // HMAC auth
    paths.insert("/auth/hmac".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "HMAC authentication (default credentials)",
            "description": "Validates a gateway hmac-auth style `Authorization: hmac ...` header. Default credentials: username `alice`, secret `secret`. The signature is base64(HMAC(secret, signing-string)) where the signing string is built from the listed `headers` (default `date`), joined by newlines. Supports hmac-sha1/sha256/sha384/sha512.",
            "operationId": "authHmac",
            "responses": {
                "200": { "description": "Signature valid", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Missing or invalid signature", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) }
            }
        }
    }));
    paths.insert("/auth/hmac/{username}/{secret}".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "HMAC authentication (custom credentials)",
            "description": "Same as /auth/hmac but validates against the username and secret supplied in the path.",
            "operationId": "authHmacCustom",
            "parameters": [
                { "name": "username", "in": "path", "required": true, "schema": { "type": "string" } },
                { "name": "secret", "in": "path", "required": true, "schema": { "type": "string" } }
            ],
            "responses": {
                "200": { "description": "Signature valid", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Missing or invalid signature", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) }
            }
        }
    }));

    // Anthropic-compatible AI messages
    paths.insert("/ai/anthropic/v1/messages".into(), json!({
        "post": {
            "tags": ["AI Gateway"],
            "summary": "Anthropic Messages API (mock)",
            "description": "Anthropic `/v1/messages`-compatible endpoint. Returns a canned assistant message with input/output token usage. When `stream: true`, emits the native Anthropic SSE event sequence (message_start, content_block_delta, message_stop, …). Use to test the ai-proxy plugin against the Anthropic provider format.",
            "operationId": "anthropicMessages",
            "requestBody": {
                "required": true,
                "content": { "application/json": { "schema": json!({
                    "type": "object",
                    "required": ["messages"],
                    "properties": {
                        "model": { "type": "string", "example": "rustybin-claude" },
                        "max_tokens": { "type": "integer", "example": 256 },
                        "system": { "type": "string" },
                        "stream": { "type": "boolean", "default": false },
                        "messages": { "type": "array", "items": { "type": "object", "properties": {
                            "role": { "type": "string", "example": "user" },
                            "content": { "type": "string", "example": "hello" }
                        }}}
                    }
                }) } }
            },
            "responses": {
                "200": { "description": "Assistant message (JSON) or text/event-stream when stream=true" },
                "400": { "description": "Invalid request" }
            }
        }
    }));

    // WebSocket
    paths.insert("/ws".into(), json!({
        "get": {
            "tags": ["WebSocket"],
            "summary": "WebSocket echo",
            "description": "Upgrade to a WebSocket connection that echoes every text and binary frame back to the client. Requires the standard WebSocket upgrade headers; a plain GET returns 426 Upgrade Required.",
            "operationId": "wsEcho",
            "responses": {
                "101": { "description": "Switching Protocols (WebSocket established)" },
                "426": { "description": "Upgrade Required (missing WebSocket headers)" }
            }
        }
    }));
    paths.insert("/ws/time".into(), json!({
        "get": {
            "tags": ["WebSocket"],
            "summary": "WebSocket timestamp ticker",
            "description": "Upgrade to a WebSocket that pushes the current timestamp on a fixed interval, then closes. Exercises server-initiated frames through the gateway.",
            "operationId": "wsTime",
            "parameters": [
                { "name": "interval_ms", "in": "query", "required": false, "schema": { "type": "integer", "default": 1000, "minimum": 100, "maximum": 60000 }, "description": "Tick interval in milliseconds" },
                { "name": "count", "in": "query", "required": false, "schema": { "type": "integer", "default": 10, "minimum": 1, "maximum": 1000 }, "description": "Number of ticks before close" }
            ],
            "responses": {
                "101": { "description": "Switching Protocols (WebSocket established)" },
                "426": { "description": "Upgrade Required (missing WebSocket headers)" }
            }
        }
    }));

    // Images
    for fmt in &["png", "jpeg", "gif"] {
        let ct = match *fmt {
            "png" => "image/png",
            "jpeg" => "image/jpeg",
            _ => "image/gif",
        };
        paths.insert(format!("/image/{fmt}"), json!({
            "get": {
                "tags": ["Utility"],
                "summary": format!("Get a {fmt} image"),
                "description": format!("Returns a minimal {fmt} test image."),
                "operationId": format!("getImage{}", fmt.to_uppercase()),
                "responses": {
                    "200": {
                        "description": format!("{fmt} image"),
                        "content": { ct: { "schema": { "type": "string", "format": "binary" } } }
                    }
                }
            }
        }));
    }

    // OpenAPI spec endpoints
    paths.insert(
        "/openapi.json".into(),
        json!({
            "get": {
                "tags": ["Utility"],
                "summary": "OpenAPI spec (JSON)",
                "description": "Returns the full OpenAPI 3.0.3 specification as JSON.",
                "operationId": "getOpenApiJson",
                "responses": { "200": { "description": "OpenAPI JSON spec" } }
            }
        }),
    );
    paths.insert(
        "/openapi.yaml".into(),
        json!({
            "get": {
                "tags": ["Utility"],
                "summary": "OpenAPI spec (YAML)",
                "description": "Returns the full OpenAPI 3.0.3 specification as YAML.",
                "operationId": "getOpenApiYaml",
                "responses": { "200": { "description": "OpenAPI YAML spec" } }
            }
        }),
    );
    paths.insert("/docs".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "API documentation UI",
            "description": "Serves a Scalar-powered interactive API documentation page.",
            "operationId": "getDocs",
            "responses": { "200": { "description": "HTML documentation page", "content": { "text/html": {} } } }
        }
    }));

    paths.insert("/export/postman.json".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Postman Collection export",
            "description": "Downloads a Postman Collection v2.1 JSON file with pre-configured requests for all Rustybin endpoints.",
            "operationId": "getExportPostman",
            "responses": {
                "200": {
                    "description": "Postman Collection v2.1 JSON",
                    "content": { "application/json": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin-postman.json\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/insomnia.json".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Insomnia Collection export",
            "description": "Downloads an Insomnia Export v4 JSON file with pre-configured requests for all Rustybin endpoints.",
            "operationId": "getExportInsomnia",
            "responses": {
                "200": {
                    "description": "Insomnia Export v4 JSON",
                    "content": { "application/json": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin-insomnia.json\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/curl.sh".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "cURL shell script export",
            "description": "Downloads a Bash script with curl commands for all Rustybin endpoints. Set BASE_URL env var to target your instance.",
            "operationId": "getExportCurl",
            "responses": {
                "200": {
                    "description": "Bash script with curl commands",
                    "content": { "text/x-shellscript": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin-curl.sh\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/bruno.json".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Bruno Collection export",
            "description": "Downloads a Bruno collection JSON file with pre-configured requests for all Rustybin endpoints.",
            "operationId": "getExportBruno",
            "responses": {
                "200": {
                    "description": "Bruno Collection JSON",
                    "content": { "application/json": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin-bruno.json\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/requests.http".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": ".http file export",
            "description": "Downloads an HTTP requests file compatible with JetBrains HTTP Client and VS Code REST Client.",
            "operationId": "getExportHttpFile",
            "responses": {
                "200": {
                    "description": ".http file with request definitions",
                    "content": { "text/plain": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin.http\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/requests.hurl".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Hurl file export",
            "description": "Downloads a Hurl file with HTTP request definitions and response assertions for all Rustybin endpoints.",
            "operationId": "getExportHurl",
            "responses": {
                "200": {
                    "description": "Hurl file with request definitions",
                    "content": { "text/plain": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin.hurl\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/k6.js".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "k6 load test script export",
            "description": "Downloads a Grafana k6 JavaScript load test script for all Rustybin endpoints. Set BASE_URL env var to target your instance.",
            "operationId": "getExportK6",
            "responses": {
                "200": {
                    "description": "k6 JavaScript load test script",
                    "content": { "application/javascript": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin-k6.js\"" } } }
                }
            }
        }
    }));

    paths.insert("/export/har.json".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "HAR 1.2 archive export",
            "description": "Downloads an HTTP Archive (HAR) 1.2 JSON file with request entries for all Rustybin endpoints.",
            "operationId": "getExportHar",
            "responses": {
                "200": {
                    "description": "HAR 1.2 JSON archive",
                    "content": { "application/json": {} },
                    "headers": { "Content-Disposition": { "schema": { "type": "string", "example": "attachment; filename=\"rustybin.har.json\"" } } }
                }
            }
        }
    }));

    // ── Flaky ────────────────────────────────────────────────────
    paths.insert("/flaky/{fail_rate}".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Random failure by rate",
            "description": "Fails fail_rate% of the time with 503. Useful for circuit-breaker and health-check testing.",
            "operationId": "getFlakyRate",
            "parameters": [{ "name": "fail_rate", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 0, "maximum": 100 }, "description": "Failure percentage (0=never fail, 100=always fail)" }],
            "responses": {
                "200": { "description": "Request succeeded", "headers": flaky_headers_spec(),
                    "content": json_xml_content(json!({ "$ref": "#/components/schemas/FlakySuccess" })) },
                "400": { "description": "Invalid fail_rate", "content": json_xml_content(json!({ "$ref": "#/components/schemas/ErrorResponse" })) },
                "503": { "description": "Simulated failure", "headers": flaky_503_headers_spec(),
                    "content": json_xml_content(json!({ "$ref": "#/components/schemas/FlakyFailure" })) }
            }
        }
    }));

    paths.insert("/flaky/pattern/{pattern}".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Deterministic success/fail pattern",
            "description": "Follows a repeating pattern of S (success/200) and F (fail/503). E.g. SSFSF cycles through: success, success, fail, success, fail.",
            "operationId": "getFlakyPattern",
            "parameters": [{ "name": "pattern", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[SsFf]+$" }, "example": "SSFSF", "description": "Pattern of S (success) and F (fail) characters" }],
            "responses": {
                "200": { "description": "Pattern position is S (success)" },
                "400": { "description": "Invalid pattern (must contain only S and F)", "content": json_xml_content(json!({ "$ref": "#/components/schemas/ErrorResponse" })) },
                "503": { "description": "Pattern position is F (fail)" }
            }
        }
    }));

    paths.insert("/flaky/after/{n}".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Succeed then fail after N requests",
            "description": "Succeeds for the first N requests, then fails forever (until reset). Simulates an upstream that goes down.",
            "operationId": "getFlakyAfter",
            "parameters": [{ "name": "n", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 0 }, "description": "Number of successful requests before failing" }],
            "responses": {
                "200": { "description": "Request succeeded (within first N requests)" },
                "503": { "description": "Service unavailable (after N requests)" }
            }
        }
    }));

    paths.insert("/flaky/recover/{n}".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Fail then recover after N requests",
            "description": "Fails for the first N requests, then succeeds forever. Simulates an upstream recovering.",
            "operationId": "getFlakyRecover",
            "parameters": [{ "name": "n", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 0 }, "description": "Number of failing requests before recovering" }],
            "responses": {
                "200": { "description": "Request succeeded (after N requests)" },
                "503": { "description": "Service unavailable (within first N requests)" }
            }
        }
    }));

    paths.insert("/flaky/reset".into(), json!({
        "post": {
            "tags": ["Utility"],
            "summary": "Reset all flaky counters",
            "description": "Resets all counters for after, recover, pattern, and random flaky endpoints back to zero.",
            "operationId": "resetFlaky",
            "responses": {
                "200": { "description": "All counters reset",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "status": { "type": "string", "example": "reset" },
                            "message": { "type": "string", "example": "All flaky counters reset" }
                        }
                    }))
                }
            }
        }
    }));

    paths.insert("/flaky/status".into(), json!({
        "get": {
            "tags": ["Utility"],
            "summary": "Flaky endpoint counter status",
            "description": "Returns current values of all flaky endpoint counters for debugging.",
            "operationId": "getFlakyStatus",
            "responses": {
                "200": { "description": "Current counter values",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "pattern_counter": { "type": "integer" },
                            "after_counter": { "type": "integer" },
                            "recover_counter": { "type": "integer" },
                            "random_counter": { "type": "integer" }
                        }
                    }))
                }
            }
        }
    }));

    // ── Echo ─────────────────────────────────────────────────────
    for path in &["/echo", "/anything"] {
        let tag = "Echo";
        let name = path.trim_start_matches('/');
        paths.insert((*path).to_string(), json!({
            "get": {
                "tags": [tag],
                "summary": format!("{name} - echo request details"),
                "description": format!("Returns all details of the incoming request: method, headers, query params, body. Supports all HTTP methods."),
                "operationId": format!("get{}", capitalize(name)),
                "responses": {
                    "200": { "description": "Request details", "content": json_xml_content(json!({ "$ref": "#/components/schemas/EchoResponse" })) }
                }
            },
            "post": {
                "tags": [tag],
                "summary": format!("{name} - echo request with body"),
                "operationId": format!("post{}", capitalize(name)),
                "requestBody": { "content": { "application/json": { "schema": {} }, "text/plain": { "schema": { "type": "string" } } } },
                "responses": {
                    "200": { "description": "Request details with body", "content": json_xml_content(json!({ "$ref": "#/components/schemas/EchoResponse" })) }
                }
            },
            "put": { "tags": [tag], "summary": format!("{name} - PUT"), "operationId": format!("put{}", capitalize(name)),
                "responses": { "200": { "description": "Request details" } } },
            "delete": { "tags": [tag], "summary": format!("{name} - DELETE"), "operationId": format!("delete{}", capitalize(name)),
                "responses": { "200": { "description": "Request details" } } },
            "patch": { "tags": [tag], "summary": format!("{name} - PATCH"), "operationId": format!("patch{}", capitalize(name)),
                "responses": { "200": { "description": "Request details" } } }
        }));

        // Sub-paths
        paths.insert(format!("{path}/{{subpath}}"), json!({
            "get": {
                "tags": [tag],
                "summary": format!("{name} with sub-path"),
                "operationId": format!("get{}SubPath", capitalize(name)),
                "parameters": [{ "name": "subpath", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Arbitrary sub-path (captured in response)" }],
                "responses": { "200": { "description": "Request details", "content": json_xml_content(json!({ "$ref": "#/components/schemas/EchoResponse" })) } }
            }
        }));
    }

    // ── Status ───────────────────────────────────────────────────
    paths.insert("/status/{code}".into(), json!({
        "get": {
            "tags": ["Status"],
            "summary": "Return a specific HTTP status code",
            "description": "Returns the specified HTTP status code (use 200-599: 1xx codes are interim responses and cannot be returned as a final response). Redirect codes (301, 302, 307, 308) include a Location header pointing to /echo. 204 and 304 return empty bodies.",
            "operationId": "getStatus",
            "parameters": [{ "name": "code", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 100, "maximum": 599 }, "example": 200, "description": "HTTP status code to return" }],
            "responses": {
                "200": { "description": "Success response", "content": json_xml_content(json!({ "type": "object", "properties": { "status": { "type": "integer" } } })) },
                "400": { "description": "Invalid status code", "content": json_xml_content(json!({ "$ref": "#/components/schemas/ErrorResponse" })) }
            }
        }
    }));

    // ── Response Shaping ─────────────────────────────────────────
    paths.insert("/delay/{ms}".into(), json!({
        "get": {
            "tags": ["Response Shaping"],
            "summary": "Delay response by N milliseconds",
            "description": "Waits the specified number of milliseconds before responding. Max 60,000ms.",
            "operationId": "getDelay",
            "parameters": [
                { "name": "ms", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 0, "maximum": 60000 }, "description": "Delay in milliseconds" },
                { "name": "jitter", "in": "query", "required": false, "schema": { "type": "boolean" }, "description": "Apply ±20% random jitter to the delay" }
            ],
            "responses": {
                "200": { "description": "Delayed response with request details" },
                "400": { "description": "Invalid delay value", "content": json_xml_content(json!({ "$ref": "#/components/schemas/ErrorResponse" })) }
            }
        }
    }));

    paths.insert("/response-headers".into(), json!({
        "get": {
            "tags": ["Response Shaping"],
            "summary": "Set arbitrary response headers",
            "description": "Any query parameters are set as response headers and returned in the JSON body.",
            "operationId": "getResponseHeaders",
            "parameters": [{ "name": "X-Custom-Header", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Example: any query parameter becomes a response header" }],
            "responses": {
                "200": { "description": "Response with custom headers" }
            }
        }
    }));

    paths.insert("/cache/{ttl}".into(), json!({
        "get": {
            "tags": ["Response Shaping"],
            "summary": "Cacheable response with TTL",
            "description": "Returns a response with Cache-Control, ETag, and Last-Modified headers. Supports conditional requests via If-None-Match and If-Modified-Since.",
            "operationId": "getCache",
            "parameters": [
                { "name": "ttl", "in": "path", "required": true, "schema": { "type": "integer" }, "description": "Cache TTL in seconds" },
                { "name": "If-None-Match", "in": "header", "required": false, "schema": { "type": "string" }, "description": "ETag for conditional request" },
                { "name": "If-Modified-Since", "in": "header", "required": false, "schema": { "type": "string" }, "description": "Date for conditional request" }
            ],
            "responses": {
                "200": { "description": "Cacheable response with ETag and Cache-Control headers" },
                "304": { "description": "Not Modified - conditional request matched" }
            }
        }
    }));

    // ── Redirects & Cookies ──────────────────────────────────────
    paths.insert("/redirect/{n}".into(), json!({
        "get": {
            "tags": ["Redirects & Cookies"],
            "summary": "Relative redirect chain",
            "description": "Performs N relative 302 redirects, ending at /echo. Max 20 hops.",
            "operationId": "getRedirect",
            "parameters": [{ "name": "n", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 1, "maximum": 20 }, "description": "Number of redirects" }],
            "responses": {
                "302": { "description": "Redirect", "headers": { "Location": { "schema": { "type": "string" } } } },
                "400": { "description": "Invalid redirect count" }
            }
        }
    }));

    paths.insert("/cookies".into(), json!({
        "get": {
            "tags": ["Redirects & Cookies"],
            "summary": "Get cookies",
            "description": "Returns all cookies sent with the request.",
            "operationId": "getCookies",
            "responses": {
                "200": { "description": "Cookies from the request",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": { "cookies": { "type": "object", "additionalProperties": { "type": "string" } } }
                    }))
                }
            }
        }
    }));

    paths.insert("/cookies/set".into(), json!({
        "get": {
            "tags": ["Redirects & Cookies"],
            "summary": "Set cookies via query parameters",
            "description": "Sets cookies from query parameters and redirects to /cookies. Use _path, _domain, _secure, _httponly, _samesite, _maxage prefixed params for cookie options.",
            "operationId": "setCookies",
            "parameters": [
                { "name": "name", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Cookie name=value (any query param becomes a cookie)" },
                { "name": "_path", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Cookie Path attribute" },
                { "name": "_secure", "in": "query", "required": false, "schema": { "type": "boolean" }, "description": "Cookie Secure flag" },
                { "name": "_httponly", "in": "query", "required": false, "schema": { "type": "boolean" }, "description": "Cookie HttpOnly flag" }
            ],
            "responses": { "302": { "description": "Redirect to /cookies with Set-Cookie headers" } }
        }
    }));

    paths.insert("/cookies/set/{name}/{value}".into(), json!({
        "get": {
            "tags": ["Redirects & Cookies"],
            "summary": "Set a single cookie",
            "description": "Sets a single cookie with the given name and value, then redirects to /cookies.",
            "operationId": "setCookie",
            "parameters": [
                { "name": "name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Cookie name" },
                { "name": "value", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Cookie value" }
            ],
            "responses": { "302": { "description": "Redirect to /cookies with Set-Cookie header" } }
        }
    }));

    paths.insert("/cookies/delete".into(), json!({
        "get": {
            "tags": ["Redirects & Cookies"],
            "summary": "Delete cookies",
            "description": "Deletes cookies named in the query parameters by setting Max-Age=0.",
            "operationId": "deleteCookies",
            "parameters": [{ "name": "name", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Cookie name to delete (any query param name)" }],
            "responses": { "302": { "description": "Redirect to /cookies with expired Set-Cookie headers" } }
        }
    }));

    // ── Info ─────────────────────────────────────────────────────
    paths.insert("/ip".into(), json!({
        "get": {
            "tags": ["Info"],
            "summary": "Client IP address",
            "description": "Returns the client's IP address. Respects X-Forwarded-For when RUSTYBIN_TRUST_FORWARD=true.",
            "operationId": "getIp",
            "responses": {
                "200": { "description": "IP address",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "ipv4": { "type": "string", "example": "127.0.0.1", "nullable": true },
                            "ipv6": { "type": "string", "nullable": true }
                        }
                    }))
                }
            }
        }
    }));

    paths.insert(
        "/ip/v4".into(),
        json!({
            "get": { "tags": ["Info"], "summary": "Client IPv4 address", "operationId": "getIpV4",
                "responses": { "200": { "description": "IPv4 address" } } }
        }),
    );

    paths.insert(
        "/ip/v6".into(),
        json!({
            "get": { "tags": ["Info"], "summary": "Client IPv6 address", "operationId": "getIpV6",
                "responses": { "200": { "description": "IPv6 address" } } }
        }),
    );

    paths.insert("/date".into(), json!({
        "get": {
            "tags": ["Info"],
            "summary": "Current date (UTC)",
            "description": "Returns the current date in UTC.",
            "operationId": "getDate",
            "responses": {
                "200": { "description": "Date",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "date": { "type": "string", "format": "date", "example": "2024-01-01" },
                            "timezone": { "type": "string", "example": "UTC" }
                        }
                    }))
                }
            }
        }
    }));

    paths.insert("/date/{timezone}".into(), json!({
        "get": {
            "tags": ["Info"],
            "summary": "Current date in timezone",
            "operationId": "getDateTz",
            "parameters": [{ "name": "timezone", "in": "path", "required": true, "schema": { "type": "string" }, "example": "America/New_York", "description": "IANA timezone name" }],
            "responses": {
                "200": { "description": "Date in timezone" },
                "404": { "description": "Unknown timezone" }
            }
        }
    }));

    paths.insert(
        "/time".into(),
        json!({
            "get": {
                "tags": ["Info"],
                "summary": "Current time (UTC)",
                "description": "Returns the current date and time in UTC (RFC 3339).",
                "operationId": "getTime",
                "responses": {
                    "200": { "description": "Time",
                        "content": json_xml_content(json!({
                            "type": "object",
                            "properties": {
                                "time": { "type": "string", "format": "date-time" },
                                "timezone": { "type": "string", "example": "UTC" }
                            }
                        }))
                    }
                }
            }
        }),
    );

    paths.insert("/time/{timezone}".into(), json!({
        "get": {
            "tags": ["Info"],
            "summary": "Current time in timezone",
            "operationId": "getTimeTz",
            "parameters": [{ "name": "timezone", "in": "path", "required": true, "schema": { "type": "string" }, "example": "Europe/London", "description": "IANA timezone name" }],
            "responses": {
                "200": { "description": "Time in timezone" },
                "404": { "description": "Unknown timezone" }
            }
        }
    }));

    // ── Random ──────────────────────────────────────────────────
    paths.insert(
        "/uuid".into(),
        json!({
            "get": {
                "tags": ["Random"],
                "summary": "Generate a UUID v4",
                "operationId": "getUuid",
                "responses": {
                    "200": { "description": "UUID",
                        "content": json_xml_content(json!({
                            "type": "object",
                            "properties": { "uuid": { "type": "string", "format": "uuid" } }
                        }))
                    }
                }
            }
        }),
    );

    paths.insert("/guuid".into(), json!({
        "get": {
            "tags": ["Random"],
            "summary": "Generate a GUID (braced UUID)",
            "operationId": "getGuuid",
            "responses": {
                "200": { "description": "GUID",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": { "guuid": { "type": "string", "example": "{550e8400-e29b-41d4-a716-446655440000}" } }
                    }))
                }
            }
        }
    }));

    paths.insert(
        "/random".into(),
        json!({
            "get": {
                "tags": ["Random"],
                "summary": "Random data bundle",
                "description": "Returns random int, uint, uuid, guuid, and lorem ipsum text.",
                "operationId": "getRandom",
                "responses": { "200": { "description": "Random data" } }
            }
        }),
    );

    paths.insert(
        "/random/int".into(),
        json!({
            "get": {
                "tags": ["Random"],
                "summary": "Random integer (-32000 to 32000)",
                "operationId": "getRandomInt",
                "responses": {
                    "200": { "description": "Random integer",
                        "content": json_xml_content(json!({
                            "type": "object",
                            "properties": { "value": { "type": "integer" } }
                        }))
                    }
                }
            }
        }),
    );

    paths.insert("/random/int/{lower}/{upper}".into(), json!({
        "get": {
            "tags": ["Random"],
            "summary": "Random integer in range",
            "operationId": "getRandomIntRange",
            "parameters": [
                { "name": "lower", "in": "path", "required": true, "schema": { "type": "integer" }, "description": "Lower bound (inclusive)" },
                { "name": "upper", "in": "path", "required": true, "schema": { "type": "integer" }, "description": "Upper bound (inclusive)" }
            ],
            "responses": {
                "200": { "description": "Random integer in range" },
                "400": { "description": "Invalid range (lower >= upper)" }
            }
        }
    }));

    paths.insert(
        "/random/uint".into(),
        json!({
            "get": {
                "tags": ["Random"],
                "summary": "Random unsigned integer (0-65535)",
                "operationId": "getRandomUint",
                "responses": { "200": { "description": "Random unsigned integer" } }
            }
        }),
    );

    paths.insert("/random/lorem-ipsum".into(), json!({
        "get": {
            "tags": ["Random"],
            "summary": "Lorem ipsum text (1 paragraph)",
            "operationId": "getLoremIpsum",
            "responses": {
                "200": { "description": "Lorem ipsum paragraph(s)",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": { "paragraphs": { "type": "array", "items": { "type": "string" } } }
                    }))
                }
            }
        }
    }));

    paths.insert("/random/lorem-ipsum/{count}".into(), json!({
        "get": {
            "tags": ["Random"],
            "summary": "Lorem ipsum text (N paragraphs)",
            "operationId": "getLoremIpsumCount",
            "parameters": [{ "name": "count", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 1, "maximum": 32 }, "description": "Number of paragraphs" }],
            "responses": {
                "200": { "description": "Lorem ipsum paragraphs" },
                "400": { "description": "Invalid count" }
            }
        }
    }));

    // ── Auth ─────────────────────────────────────────────────────
    paths.insert("/auth/basic-auth".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "HTTP Basic authentication (default credentials)",
            "description": "Validates HTTP Basic auth with default credentials (basic:password).",
            "operationId": "getBasicAuth",
            "security": [{ "basicAuth": [] }],
            "responses": {
                "200": { "description": "Authenticated", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Unauthorized", "headers": { "WWW-Authenticate": { "schema": { "type": "string" } } },
                    "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" })) }
            }
        }
    }));

    paths.insert("/auth/basic-auth/{username}/{password}".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "HTTP Basic authentication (custom credentials)",
            "description": "Validates HTTP Basic auth with custom username and password.",
            "operationId": "getBasicAuthCustom",
            "security": [{ "basicAuth": [] }],
            "parameters": [
                { "name": "username", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Expected username" },
                { "name": "password", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Expected password" }
            ],
            "responses": {
                "200": { "description": "Authenticated", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Unauthorized", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" })) }
            }
        }
    }));

    paths.insert("/auth/api-key".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "API key authentication (default header)",
            "description": "Validates API key in the 'apikey' header with default value 'my-key'.",
            "operationId": "getApiKey",
            "security": [{ "apiKeyAuth": [] }],
            "responses": {
                "200": { "description": "Authenticated", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Unauthorized", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" })) }
            }
        }
    }));

    paths.insert("/auth/api-key/{header_name}/{key_value}".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "API key authentication (custom header and value)",
            "description": "Validates API key in a custom header with a custom value.",
            "operationId": "getApiKeyCustom",
            "parameters": [
                { "name": "header_name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Header name to check" },
                { "name": "key_value", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Expected key value" }
            ],
            "responses": {
                "200": { "description": "Authenticated", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Unauthorized", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" })) }
            }
        }
    }));

    paths.insert("/auth/jwt".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "JWT validation",
            "description": "Decodes and returns JWT claims from the Authorization Bearer token. Validates structure but does not verify signature.",
            "operationId": "getJwt",
            "security": [{ "bearerAuth": [] }],
            "responses": {
                "200": { "description": "Token decoded", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Invalid token", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthFailure" })) }
            }
        }
    }));

    paths.insert("/auth/jwt/exchange".into(), json!({
        "post": {
            "tags": ["Auth"],
            "summary": "JWT token exchange",
            "description": "Accepts a JWT, inherits its claims, and returns a new JWT signed by Rustybin (HS256). Adds iss, iat, jti, exp claims.",
            "operationId": "postJwtExchange",
            "security": [{ "bearerAuth": [] }],
            "responses": {
                "200": { "description": "Exchanged token",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "authenticated": { "type": "boolean" },
                            "auth_type": { "type": "string" },
                            "claims": { "type": "object" },
                            "exchanged_token": { "type": "string" }
                        }
                    }))
                },
                "401": { "description": "Invalid token" }
            }
        }
    }));

    // OIDC
    paths.insert("/.well-known/openid-configuration".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "OIDC discovery document",
            "description": "Returns the OpenID Connect discovery document with all endpoint URLs.",
            "operationId": "getOidcDiscovery",
            "responses": { "200": { "description": "OIDC configuration" } }
        }
    }));

    paths.insert(
        "/oauth/jwks".into(),
        json!({
            "get": {
                "tags": ["Auth"],
                "summary": "JWKS endpoint",
                "description": "Returns the JSON Web Key Set containing the RS256 public key.",
                "operationId": "getJwks",
                "responses": { "200": { "description": "JWKS" } }
            }
        }),
    );

    paths.insert("/oauth/token".into(), json!({
        "post": {
            "tags": ["Auth"],
            "summary": "OAuth2 token endpoint",
            "description": "Issues tokens for client_credentials, password, authorization_code, and RFC 8693 token-exchange grants.",
            "operationId": "postOAuthToken",
            "requestBody": {
                "required": true,
                "content": {
                    "application/x-www-form-urlencoded": {
                        "schema": {
                            "type": "object",
                            "required": ["grant_type"],
                            "properties": {
                                "grant_type": { "type": "string", "enum": ["client_credentials", "password", "authorization_code", "urn:ietf:params:oauth:grant-type:token-exchange"] },
                                "client_id": { "type": "string" },
                                "client_secret": { "type": "string" },
                                "username": { "type": "string", "description": "For password grant" },
                                "password": { "type": "string", "description": "For password grant" },
                                "scope": { "type": "string" },
                                "code": { "type": "string", "description": "For authorization_code grant" },
                                "redirect_uri": { "type": "string", "description": "For authorization_code grant" },
                                "subject_token": { "type": "string", "description": "RFC 8693: the token to exchange" },
                                "subject_token_type": { "type": "string", "description": "RFC 8693: token type URI (e.g. urn:ietf:params:oauth:token-type:access_token)" },
                                "actor_token": { "type": "string", "description": "RFC 8693: optional acting party token" },
                                "actor_token_type": { "type": "string", "description": "RFC 8693: required if actor_token is present" },
                                "audience": { "type": "string", "description": "RFC 8693: intended audience for the new token" },
                                "resource": { "type": "string", "description": "RFC 8693: target service URI" },
                                "requested_token_type": { "type": "string", "description": "RFC 8693: desired token type for the new token" }
                            }
                        }
                    }
                }
            },
            "responses": {
                "200": { "description": "Token response",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "access_token": { "type": "string" },
                            "token_type": { "type": "string", "example": "Bearer" },
                            "expires_in": { "type": "integer", "example": 3600 },
                            "id_token": { "type": "string" },
                            "scope": { "type": "string" }
                        }
                    }))
                },
                "400": { "description": "Invalid grant type or parameters" }
            }
        }
    }));

    paths.insert(
        "/oauth/authorize".into(),
        json!({
            "get": {
                "tags": ["Auth"],
                "summary": "OAuth2 authorization endpoint (login form)",
                "description": "Returns an HTML login form for the authorization code flow.",
                "operationId": "getOAuthAuthorize",
                "parameters": [
                    { "name": "client_id", "in": "query", "schema": { "type": "string" } },
                    { "name": "redirect_uri", "in": "query", "schema": { "type": "string" } },
                    { "name": "response_type", "in": "query", "schema": { "type": "string" } },
                    { "name": "scope", "in": "query", "schema": { "type": "string" } },
                    { "name": "state", "in": "query", "schema": { "type": "string" } }
                ],
                "responses": { "200": { "description": "HTML login form" } }
            },
            "post": {
                "tags": ["Auth"],
                "summary": "OAuth2 authorization endpoint (submit login)",
                "description": "Processes login and redirects with authorization code.",
                "operationId": "postOAuthAuthorize",
                "responses": { "303": { "description": "Redirect with authorization code" } }
            }
        }),
    );

    paths.insert(
        "/oauth/userinfo".into(),
        json!({
            "get": {
                "tags": ["Auth"],
                "summary": "OIDC UserInfo endpoint",
                "description": "Returns user claims from the access token.",
                "operationId": "getOAuthUserinfo",
                "security": [{ "bearerAuth": [] }],
                "responses": {
                    "200": { "description": "User info",
                        "content": json_xml_content(json!({
                            "type": "object",
                            "properties": {
                                "sub": { "type": "string" },
                                "name": { "type": "string" },
                                "email": { "type": "string" },
                                "email_verified": { "type": "boolean" }
                            }
                        }))
                    },
                    "401": { "description": "Invalid or missing token" }
                }
            }
        }),
    );

    paths.insert(
        "/oauth/introspect".into(),
        json!({
            "post": {
                "tags": ["Auth"],
                "summary": "OAuth2 token introspection",
                "description": "Introspects a token and returns its active status and claims.",
                "operationId": "postOAuthIntrospect",
                "requestBody": {
                    "required": true,
                    "content": {
                        "application/x-www-form-urlencoded": {
                            "schema": {
                                "type": "object",
                                "required": ["token"],
                                "properties": {
                                    "token": { "type": "string" },
                                    "client_id": { "type": "string" },
                                    "client_secret": { "type": "string" }
                                }
                            }
                        }
                    }
                },
                "responses": {
                    "200": { "description": "Introspection result",
                        "content": json_xml_content(json!({
                            "type": "object",
                            "properties": {
                                "active": { "type": "boolean" },
                                "sub": { "type": "string" },
                                "scope": { "type": "string" },
                                "exp": { "type": "integer" },
                                "client_id": { "type": "string" }
                            }
                        }))
                    }
                }
            }
        }),
    );

    // mTLS
    paths.insert("/auth/mtls".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "Mutual TLS authentication",
            "description": "Validates client certificate presented via TLS or header (RUSTYBIN_MTLS_IN_HEADER).",
            "operationId": "getMtls",
            "responses": {
                "200": { "description": "Authenticated", "content": json_xml_content(json!({ "$ref": "#/components/schemas/AuthResponse" })) },
                "401": { "description": "Missing or invalid client certificate" }
            }
        }
    }));

    paths.insert("/auth/mtls/get-client-cert".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "Get demo client certificate",
            "description": "Returns a demo client certificate and private key for testing mTLS.",
            "operationId": "getMtlsClientCert",
            "responses": {
                "200": { "description": "Client cert and key PEM",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "cert_pem": { "type": "string" },
                            "key_pem": { "type": "string" },
                            "usage": { "type": "string" }
                        }
                    }))
                }
            }
        }
    }));

    paths.insert("/auth/mtls/get-ca-cert".into(), json!({
        "get": {
            "tags": ["Auth"],
            "summary": "Get demo CA certificate",
            "description": "Returns the demo CA certificate for configuring trust in your API gateway.",
            "operationId": "getMtlsCaCert",
            "responses": {
                "200": { "description": "CA certificate PEM",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "ca_cert_pem": { "type": "string" },
                            "usage": { "type": "string" }
                        }
                    }))
                }
            }
        }
    }));

    // ── AI Gateway ──────────────────────────────────────────────
    paths.insert("/ai/v1/chat/completions".into(), json!({
        "post": {
            "tags": ["AI Gateway"],
            "summary": "Chat completions (OpenAI-compatible)",
            "description": "OpenAI-compatible chat completions endpoint. Returns canned responses based on prompt keywords. Supports streaming via SSE.",
            "operationId": "postChatCompletions",
            "parameters": [{ "name": "delay", "in": "query", "required": false, "schema": { "type": "integer", "maximum": 60000 }, "description": "Delay in ms before responding" }],
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": { "$ref": "#/components/schemas/ChatCompletionRequest" }
                    }
                }
            },
            "responses": {
                "200": { "description": "Chat completion",
                    "content": {
                        "application/json": { "schema": { "$ref": "#/components/schemas/ChatCompletionResponse" } },
                        "text/event-stream": { "schema": { "type": "string", "description": "Server-Sent Events stream" } }
                    }
                },
                "400": { "description": "Invalid request" }
            }
        }
    }));

    paths.insert("/ai/v1/completions".into(), json!({
        "post": {
            "tags": ["AI Gateway"],
            "summary": "Text completions (OpenAI-compatible)",
            "description": "OpenAI-compatible text completions endpoint with canned responses.",
            "operationId": "postCompletions",
            "parameters": [{ "name": "delay", "in": "query", "required": false, "schema": { "type": "integer" }, "description": "Delay in ms" }],
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "required": ["model", "prompt"],
                            "properties": {
                                "model": { "type": "string", "example": "rustybin-gpt" },
                                "prompt": { "type": "string" },
                                "max_tokens": { "type": "integer" }
                            }
                        }
                    }
                }
            },
            "responses": {
                "200": { "description": "Text completion" },
                "400": { "description": "Invalid request" }
            }
        }
    }));

    paths.insert("/ai/v1/embeddings".into(), json!({
        "post": {
            "tags": ["AI Gateway"],
            "summary": "Embeddings (OpenAI-compatible)",
            "description": "Returns deterministic 1536-dimensional embedding vectors. Same input always produces the same output.",
            "operationId": "postEmbeddings",
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "required": ["model", "input"],
                            "properties": {
                                "model": { "type": "string", "example": "rustybin-embed" },
                                "input": {
                                    "oneOf": [
                                        { "type": "string" },
                                        { "type": "array", "items": { "type": "string" } }
                                    ]
                                }
                            }
                        }
                    }
                }
            },
            "responses": {
                "200": { "description": "Embedding vectors" },
                "400": { "description": "Invalid request" }
            }
        }
    }));

    paths.insert("/ai/v1/models".into(), json!({
        "get": {
            "tags": ["AI Gateway"],
            "summary": "List available models",
            "description": "Returns the list of available AI models (rustybin-gpt, rustybin-gpt-fast, rustybin-embed).",
            "operationId": "getModels",
            "responses": {
                "200": { "description": "Model list",
                    "content": json_xml_content(json!({
                        "type": "object",
                        "properties": {
                            "object": { "type": "string", "example": "list" },
                            "data": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "object": { "type": "string" },
                                        "created": { "type": "integer" },
                                        "owned_by": { "type": "string" }
                                    }
                                }
                            }
                        }
                    }))
                }
            }
        }
    }));

    // ── GraphQL ──────────────────────────────────────────────────
    paths.insert("/graphql".into(), json!({
        "get": {
            "tags": ["GraphQL"],
            "summary": "GraphQL Playground",
            "description": "Serves an interactive GraphiQL playground for exploring the schema.",
            "operationId": "getGraphqlPlayground",
            "responses": { "200": { "description": "GraphiQL HTML page", "content": { "text/html": {} } } }
        },
        "post": {
            "tags": ["GraphQL"],
            "summary": "Execute GraphQL query",
            "description": "Executes a GraphQL query against the hardcoded dataset (users, products, orders, reviews).",
            "operationId": "postGraphql",
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "required": ["query"],
                            "properties": {
                                "query": { "type": "string", "example": "{ users { id name email } }" },
                                "variables": { "type": "object" },
                                "operationName": { "type": "string" }
                            }
                        }
                    }
                }
            },
            "responses": {
                "200": { "description": "GraphQL response",
                    "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": {
                            "data": { "type": "object" },
                            "errors": { "type": "array", "items": { "type": "object" } }
                        }
                    } } }
                }
            }
        }
    }));

    paths.insert("/graphql/schema".into(), json!({
        "get": {
            "tags": ["GraphQL"],
            "summary": "GraphQL SDL schema",
            "description": "Returns the GraphQL schema in SDL format.",
            "operationId": "getGraphqlSchema",
            "responses": { "200": { "description": "GraphQL SDL", "content": { "text/plain": { "schema": { "type": "string" } } } } }
        }
    }));

    // ── Orchestration ───────────────────────────────────────────
    for step in 1..=4u8 {
        let (name, desc) = match step {
            1 => (
                "authenticate",
                "Validate API key and return merchant metadata with correlation token",
            ),
            2 => (
                "enrich",
                "Enrich transaction with risk scoring and card details",
            ),
            3 => (
                "validate",
                "Apply business rules and validate the transaction",
            ),
            _ => (
                "process",
                "Execute the payment and return transaction result",
            ),
        };
        let mut params = vec![
            json!({ "name": "X-Correlation-Id", "in": "header", "required": step > 1, "schema": { "type": "string" }, "description": "Correlation ID from step 1" }),
        ];
        if step == 1 {
            params = vec![
                json!({ "name": "X-Api-Key", "in": "header", "required": true, "schema": { "type": "string" }, "description": "API key (any non-empty value)" }),
            ];
        }
        if step == 4 {
            params.push(json!({ "name": "X-Validation-Result", "in": "header", "required": true, "schema": { "type": "string", "enum": ["approved"] }, "description": "Must be 'approved'" }));
        }

        paths.insert(format!("/orchestration/step/{step}"), json!({
            "post": {
                "tags": ["Orchestration"],
                "summary": format!("Step {step}: {name}"),
                "description": desc,
                "operationId": format!("postOrchestrationStep{step}"),
                "parameters": params,
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                "responses": {
                    "200": { "description": format!("Step {step} result") },
                    "400": { "description": "Missing required headers or body fields" },
                    "401": { "description": "Missing API key (step 1 only)" },
                    "403": { "description": "Validation not approved (step 4 only)" }
                }
            }
        }));
    }

    paths.insert("/orchestration/status".into(), json!({
        "get": {
            "tags": ["Orchestration"],
            "summary": "Orchestration pipeline overview",
            "description": "Returns documentation of all orchestration steps with required headers and example bodies.",
            "operationId": "getOrchestrationStatus",
            "responses": { "200": { "description": "Pipeline documentation" } }
        }
    }));

    // ── SOAP ────────────────────────────────────────────────────
    paths.insert("/soap".into(), json!({
        "post": {
            "tags": ["SOAP"],
            "summary": "SOAP web service",
            "description": "Accepts SOAP XML envelopes and routes to operations: GetUser, CreateOrder, GetStatus. Returns SOAP responses.",
            "operationId": "postSoap",
            "parameters": [{ "name": "SOAPAction", "in": "header", "required": false, "schema": { "type": "string" }, "description": "SOAP action URI (echoed in response)" }],
            "requestBody": {
                "required": true,
                "content": {
                    "text/xml": {
                        "schema": { "type": "string" },
                        "example": "<soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"><soap:Body><GetUser xmlns=\"http://rustybin.local/users\"><userId>123</userId></GetUser></soap:Body></soap:Envelope>"
                    }
                }
            },
            "responses": {
                "200": { "description": "SOAP response", "content": { "text/xml": { "schema": { "type": "string" } } } },
                "400": { "description": "SOAP fault (invalid envelope or unknown operation)", "content": { "text/xml": { "schema": { "type": "string" } } } }
            }
        }
    }));

    paths.insert("/soap/wsdl".into(), json!({
        "get": {
            "tags": ["SOAP"],
            "summary": "WSDL service description",
            "description": "Returns the WSDL document defining the SOAP service operations (GetUser, CreateOrder, GetStatus).",
            "operationId": "getSoapWsdl",
            "responses": { "200": { "description": "WSDL document", "content": { "text/xml": { "schema": { "type": "string" } } } } }
        }
    }));

    Value::Object(paths)
}

fn build_components() -> Value {
    json!({
        "schemas": {
            "ErrorResponse": {
                "type": "object",
                "properties": {
                    "error": { "type": "string", "example": "bad_request" },
                    "details": { "type": "string", "example": "Detailed error description", "nullable": true }
                },
                "required": ["error"]
            },
            "AuthResponse": {
                "type": "object",
                "properties": {
                    "authenticated": { "type": "boolean" },
                    "auth_type": { "type": "string", "enum": ["basic-auth", "api-key", "jwt", "mtls"] },
                    "username": { "type": "string", "nullable": true },
                    "header": { "type": "string", "nullable": true },
                    "claims": { "type": "object", "nullable": true },
                    "jwt_header": { "type": "object", "nullable": true },
                    "client_dn": { "type": "string", "nullable": true },
                    "client_ca": { "type": "string", "nullable": true }
                },
                "required": ["authenticated", "auth_type"]
            },
            "AuthFailure": {
                "type": "object",
                "properties": {
                    "authenticated": { "type": "boolean", "example": false },
                    "error": { "type": "string", "example": "unauthorized" }
                },
                "required": ["authenticated", "error"]
            },
            "EchoResponse": {
                "type": "object",
                "properties": {
                    "method": { "type": "string", "example": "GET" },
                    "path": { "type": "string", "example": "/echo" },
                    "path_info": { "type": "array", "items": { "type": "string" } },
                    "query_string": { "type": "string" },
                    "query_params": { "type": "object", "additionalProperties": { "type": "string" } },
                    "headers": { "type": "object", "additionalProperties": { "type": "array", "items": { "type": "string" } } },
                    "host": { "type": "string" },
                    "port": { "type": "integer" },
                    "scheme": { "type": "string" },
                    "remote_ip": { "type": "string" },
                    "body": { "$ref": "#/components/schemas/EchoBody" },
                    "timestamp_unix_ms": { "type": "integer", "format": "int64" }
                }
            },
            "EchoBody": {
                "type": "object",
                "properties": {
                    "present": { "type": "boolean" },
                    "included": { "type": "boolean" },
                    "body": { "type": "string", "nullable": true },
                    "bytes": { "type": "integer" },
                    "truncated": { "type": "boolean" },
                    "utf8": { "type": "boolean" },
                    "reason": { "type": "string", "nullable": true }
                }
            },
            "IdentityResponse": {
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string", "format": "uuid" },
                    "hostname": { "type": "string" },
                    "version": { "type": "string" },
                    "uptime_seconds": { "type": "integer" },
                    "request_count": { "type": "integer" },
                    "port": {
                        "type": "object",
                        "properties": { "http": { "type": "integer" }, "https": { "type": "integer" } }
                    },
                    "environment": {
                        "type": "object",
                        "properties": { "rust_version": { "type": "string" }, "profile": { "type": "string" } }
                    },
                    "request": {
                        "type": "object",
                        "properties": {
                            "remote_ip": { "type": "string" },
                            "forwarded_for": { "type": "string", "nullable": true },
                            "host": { "type": "string" },
                            "via": { "type": "string", "nullable": true }
                        }
                    },
                    "timestamp": { "type": "string", "format": "date-time" }
                }
            },
            "FlakySuccess": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "example": "ok" },
                    "fail_rate": { "type": "integer" },
                    "message": { "type": "string", "example": "Request succeeded" },
                    "request_number": { "type": "integer" }
                }
            },
            "FlakyFailure": {
                "type": "object",
                "properties": {
                    "error": { "type": "string", "example": "service_unavailable" },
                    "fail_rate": { "type": "integer" },
                    "message": { "type": "string", "example": "Simulated failure (50% fail rate)" }
                }
            },
            "ChatCompletionRequest": {
                "type": "object",
                "required": ["model", "messages"],
                "properties": {
                    "model": { "type": "string", "example": "rustybin-gpt" },
                    "messages": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["role", "content"],
                            "properties": {
                                "role": { "type": "string", "enum": ["system", "user", "assistant"] },
                                "content": { "type": "string" }
                            }
                        }
                    },
                    "stream": { "type": "boolean", "default": false },
                    "max_tokens": { "type": "integer" },
                    "temperature": { "type": "number", "minimum": 0, "maximum": 2 }
                }
            },
            "ChatCompletionResponse": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "example": "chatcmpl-abc123" },
                    "object": { "type": "string", "example": "chat.completion" },
                    "created": { "type": "integer" },
                    "model": { "type": "string" },
                    "choices": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "index": { "type": "integer" },
                                "message": {
                                    "type": "object",
                                    "properties": {
                                        "role": { "type": "string" },
                                        "content": { "type": "string" }
                                    }
                                },
                                "finish_reason": { "type": "string" }
                            }
                        }
                    },
                    "usage": {
                        "type": "object",
                        "properties": {
                            "prompt_tokens": { "type": "integer" },
                            "completion_tokens": { "type": "integer" },
                            "total_tokens": { "type": "integer" }
                        }
                    }
                }
            }
        },
        "securitySchemes": {
            "basicAuth": {
                "type": "http",
                "scheme": "basic",
                "description": "HTTP Basic authentication. Default credentials: basic:password"
            },
            "bearerAuth": {
                "type": "http",
                "scheme": "bearer",
                "bearerFormat": "JWT",
                "description": "JWT Bearer token authentication"
            },
            "apiKeyAuth": {
                "type": "apiKey",
                "in": "header",
                "name": "apikey",
                "description": "API key in header. Default key: my-key"
            }
        }
    })
}

// ── Helpers ─────────────────────────────────────────────────────

fn json_xml_content(schema: Value) -> Value {
    json!({
        "application/json": { "schema": schema },
        "application/xml": { "schema": schema }
    })
}

fn flaky_headers_spec() -> Value {
    json!({
        "X-Rustybin-Flaky": { "schema": { "type": "string", "example": "true" } },
        "X-Rustybin-Fail-Rate": { "schema": { "type": "integer" } }
    })
}

fn flaky_503_headers_spec() -> Value {
    json!({
        "X-Rustybin-Flaky": { "schema": { "type": "string", "example": "true" } },
        "X-Rustybin-Fail-Rate": { "schema": { "type": "integer" } },
        "Retry-After": { "schema": { "type": "integer", "example": 1 } }
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// ── Handlers ────────────────────────────────────────────────────

async fn openapi_json_handler() -> Response {
    let spec = build_spec();
    match serde_json::to_string_pretty(&spec) {
        Ok(json) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            json,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize OpenAPI spec: {e}"),
        )
            .into_response(),
    }
}

async fn openapi_yaml_handler() -> Response {
    let spec = build_spec();
    match serde_yaml::to_string(&spec) {
        Ok(yaml) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/yaml; charset=utf-8"),
            )],
            yaml,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize OpenAPI spec: {e}"),
        )
            .into_response(),
    }
}

async fn docs_handler() -> Html<&'static str> {
    Html(DOCS_HTML)
}

const DOCS_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
    <title>Rustybin API Docs</title>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
</head>
<body>
    <script id="api-reference" data-url="/openapi.json"></script>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
</body>
</html>"#;

// ── Router ──────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(openapi_json_handler))
        .route("/openapi.yaml", get(openapi_yaml_handler))
        .route("/docs", get(docs_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/openapi.json",
            &["GET"],
            category::DOCS,
            "OpenAPI 3.0.3 specification (JSON)",
        )
        .example(Example::get("OpenAPI JSON", "/openapi.json")),
        Endpoint::new(
            "/openapi.yaml",
            &["GET"],
            category::DOCS,
            "OpenAPI 3.0.3 specification (YAML)",
        )
        .example(Example::get("OpenAPI YAML", "/openapi.yaml")),
        Endpoint::new(
            "/docs",
            &["GET"],
            category::DOCS,
            "Interactive API reference (Scalar)",
        )
        .example(Example::get("API docs", "/docs")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn body_string(resp: axum::http::Response<Body>) -> String {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(body.to_vec()).expect("utf8")
    }

    #[tokio::test]
    async fn openapi_json_returns_valid_spec() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/json"
        );

        let body = body_string(resp).await;
        let spec: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(spec["openapi"], "3.0.3");
        assert_eq!(spec["info"]["title"], "Rustybin");
        assert!(spec["paths"].as_object().unwrap().len() >= 40);
    }

    #[tokio::test]
    async fn openapi_yaml_returns_valid_spec() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.yaml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("yaml"));

        let body = body_string(resp).await;
        let spec: Value = serde_yaml::from_str(&body).expect("valid YAML");
        assert_eq!(spec["openapi"], "3.0.3");
    }

    #[tokio::test]
    async fn docs_returns_html() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/docs")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("Rustybin API Docs"));
        assert!(body.contains("@scalar/api-reference"));
        assert!(body.contains("/openapi.json"));
    }

    #[tokio::test]
    async fn spec_has_security_schemes() {
        let spec = build_spec();
        let schemes = &spec["components"]["securitySchemes"];
        assert!(schemes["basicAuth"].is_object());
        assert!(schemes["bearerAuth"].is_object());
        assert!(schemes["apiKeyAuth"].is_object());
    }

    #[tokio::test]
    async fn spec_has_key_endpoints() {
        let spec = build_spec();
        let paths = spec["paths"].as_object().unwrap();

        // Spot-check key endpoints
        assert!(
            paths.contains_key("/ai/v1/chat/completions"),
            "missing AI chat"
        );
        assert!(paths.contains_key("/auth/basic-auth"), "missing basic auth");
        assert!(paths.contains_key("/delay/{ms}"), "missing delay");
        assert!(paths.contains_key("/graphql"), "missing graphql");
        assert!(paths.contains_key("/soap"), "missing soap");
        assert!(paths.contains_key("/flaky/{fail_rate}"), "missing flaky");
        assert!(paths.contains_key("/identity"), "missing identity");
        assert!(
            paths.contains_key("/.well-known/openid-configuration"),
            "missing OIDC"
        );
    }

    #[tokio::test]
    async fn spec_has_component_schemas() {
        let spec = build_spec();
        let schemas = spec["components"]["schemas"].as_object().unwrap();
        assert!(schemas.contains_key("ErrorResponse"));
        assert!(schemas.contains_key("AuthResponse"));
        assert!(schemas.contains_key("EchoResponse"));
        assert!(schemas.contains_key("ChatCompletionRequest"));
        assert!(schemas.contains_key("ChatCompletionResponse"));
        assert!(schemas.contains_key("IdentityResponse"));
    }

    #[tokio::test]
    async fn ai_chat_has_request_response_schemas() {
        let spec = build_spec();
        let chat = &spec["paths"]["/ai/v1/chat/completions"]["post"];
        assert!(
            chat["requestBody"]["content"]["application/json"]["schema"]["$ref"]
                .as_str()
                .unwrap()
                .contains("ChatCompletionRequest")
        );
    }

    #[tokio::test]
    async fn basic_auth_has_security_requirement() {
        let spec = build_spec();
        let basic = &spec["paths"]["/auth/basic-auth"]["get"];
        let security = basic["security"].as_array().unwrap();
        assert!(!security.is_empty());
        assert!(security[0]["basicAuth"].is_array());
    }

    #[tokio::test]
    async fn delay_has_parameter_validation() {
        let spec = build_spec();
        let delay = &spec["paths"]["/delay/{ms}"]["get"];
        let params = delay["parameters"].as_array().unwrap();
        let ms_param = &params[0];
        assert_eq!(ms_param["name"], "ms");
        assert_eq!(ms_param["schema"]["maximum"], 60000);
    }
}
