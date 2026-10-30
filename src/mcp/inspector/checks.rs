//! "Run checks": a repeatable suite against one MCP server, with a report
//! like the harness's (JSON and JUnit). The console, and later the CLI and
//! the management MCP, run it through `POST /_rustybin/mcp-checks`; targets
//! on this instance are called in process, others through the egress policy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::client::{Conn, Outcome, Target, Transport};
use crate::mocks::schema::{self, Direction, GenOptions};
use crate::state::AppState;

/// Every check, in run order: `(id, title)`.
pub const CHECKS: &[(&str, &str)] = &[
    (
        "connect",
        "Connect (initialize or server/discover) and read the capabilities",
    ),
    ("version", "Protocol version negotiation"),
    (
        "auth_refusal",
        "The server refuses a request without credentials",
    ),
    (
        "tools_list",
        "tools/list answers and every inputSchema is a usable JSON Schema",
    ),
    (
        "tools_call",
        "Every tool answers a call with generated arguments",
    ),
    ("unknown_tool", "An unknown tool gives a JSON-RPC error"),
    (
        "bad_arguments",
        "A call without its required arguments is refused",
    ),
    ("unknown_method", "An unknown method gives -32601"),
    ("resources", "resources/list, a read, and the templates"),
    ("prompts", "prompts/list and a prompt rendered"),
    ("ping", "ping answers"),
    (
        "routing_headers",
        "2026-07-28: a routing header that contradicts the body is refused",
    ),
    (
        "server_stream",
        "Session transports: the server-to-client stream opens or is declined",
    ),
    (
        "session",
        "Session transports: DELETE ends the session and the old id is refused",
    ),
];

/// Default and most tools called.
pub const DEFAULT_MAX_TOOLS: usize = 25;
pub const MAX_TOOLS: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

struct CheckResult {
    id: &'static str,
    status: Status,
    message: String,
    ms: u64,
    details: Value,
}

/// What a run needs.
pub struct RunSpec {
    pub target: Target,
    pub transport: Transport,
    pub auth: Vec<(String, String)>,
    /// Ids to run (`None`: all).
    pub only: Option<Vec<String>>,
    pub include_destructive: bool,
    pub max_tools: usize,
    /// Ask the assist provider for the tool arguments (else the generator).
    pub ai_arguments: bool,
    pub principal: String,
}

/// Validate a list of check ids.
pub fn parse_only(v: &Value) -> Result<Option<Vec<String>>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Array(items) => {
            let mut out = Vec::new();
            for i in items {
                let Some(id) = i.as_str() else {
                    return Err("checks: a list of check ids".into());
                };
                if !CHECKS.iter().any(|(c, _)| *c == id) {
                    return Err(format!(
                        "unknown check {id:?}; the checks are {}",
                        CHECKS
                            .iter()
                            .map(|(c, _)| *c)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                out.push(id.to_string());
            }
            Ok(Some(out))
        }
        _ => Err("checks: a list of check ids".into()),
    }
}

fn gen_args(schema_v: &Value, seed: u64) -> Value {
    let root = schema_v.clone();
    let v = schema::generate(
        schema_v,
        &GenOptions {
            root: &root,
            direction: Direction::Request,
            seed,
            use_examples: true,
            echo: &[],
        },
    );
    if v.is_object() {
        v
    } else {
        json!({})
    }
}

fn error_text(o: &Outcome) -> String {
    if let Some(e) = &o.transport_error {
        return e.clone();
    }
    if let Some(e) = &o.error {
        return format!(
            "JSON-RPC error {}: {}",
            e.get("code").map(|c| c.to_string()).unwrap_or_default(),
            e.get("message").and_then(Value::as_str).unwrap_or("")
        );
    }
    if let Some(b) = &o.body {
        return format!(
            "HTTP {}: {}",
            o.status,
            b.chars().take(200).collect::<String>()
        );
    }
    format!("no answer (HTTP {})", o.status)
}

fn error_code(o: &Outcome) -> Option<i64> {
    o.error
        .as_ref()
        .and_then(|e| e.get("code"))
        .and_then(Value::as_i64)
}

/// Is a tool result an error with a text explaining it?
fn is_error_result(r: &Value) -> bool {
    r.get("isError").and_then(Value::as_bool) == Some(true)
}

/// Collect a paginated list (`tools`, `resources`, `prompts`).
async fn list_all(
    conn: &mut Conn,
    method: &str,
    key: &str,
    lat: &mut Vec<(String, u64)>,
) -> Result<Vec<Value>, Box<Outcome>> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let o = conn.request(method, params).await;
        lat.push((method.to_string(), o.ms));
        let Some(r) = &o.result else {
            return Err(Box::new(o));
        };
        items.extend(
            r.get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        cursor = r
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(items)
}

/// Is `s` usable as a tool's inputSchema? (an object schema whose own
/// generated value it accepts)
fn schema_problem(s: &Value) -> Option<String> {
    let Some(obj) = s.as_object() else {
        return Some("inputSchema is not an object".into());
    };
    match obj.get("type") {
        Some(Value::String(t)) if t == "object" => {}
        None => return Some("inputSchema has no \"type\": \"object\"".into()),
        Some(other) => return Some(format!("inputSchema type is {other}, not \"object\"")),
    }
    if let Some(p) = obj.get("properties") {
        let Some(props) = p.as_object() else {
            return Some("properties is not an object".into());
        };
        for (name, spec) in props {
            if !spec.is_object() && !spec.is_boolean() {
                return Some(format!("property {name:?} is not a schema"));
            }
        }
    }
    if let Some(r) = obj.get("required") {
        let Some(list) = r.as_array() else {
            return Some("required is not an array".into());
        };
        if list.iter().any(|x| !x.is_string()) {
            return Some("required holds a non-string".into());
        }
    }
    let sample = gen_args(s, 1);
    let issues = schema::validate(&sample, s, s, Direction::Request);
    if !issues.is_empty() {
        return Some(format!(
            "the schema rejects a value generated from itself ({} {})",
            issues[0].pointer, issues[0].message
        ));
    }
    None
}

/// Arguments for a tool: the assist provider's when asked and available,
/// else the generator's.
async fn arguments_for(
    state: &AppState,
    spec: &RunSpec,
    tool: &Value,
    seed: u64,
) -> (Value, &'static str) {
    let input = tool
        .get("inputSchema")
        .cloned()
        .unwrap_or(json!({ "type": "object" }));
    if spec.ai_arguments {
        if let Some(task) = crate::assist::task::find("tool_arguments") {
            let run = Arc::clone(&state.assist).run(crate::assist::RunParams {
                task,
                inputs: json!({ "tool": { "name": tool["name"], "description": tool.get("description"), "inputSchema": input } }),
                model: None,
                principal: spec.principal.clone(),
            });
            if let Ok(mut stream) = run {
                use futures_util::StreamExt;
                while let Some(out) = stream.next().await {
                    if let crate::assist::Out::Json(v) = out {
                        if let Some(a) = v.get("arguments").filter(|a| a.is_object()) {
                            return (a.clone(), "ai");
                        }
                    }
                }
            }
        }
    }
    (gen_args(&input, seed), "generated")
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Run the suite.
pub async fn run(state: &AppState, spec: RunSpec) -> Value {
    let started = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let wanted = |id: &str| spec.only.as_ref().is_none_or(|o| o.iter().any(|x| x == id));
    let mut results: Vec<CheckResult> = Vec::new();
    let mut lat: Vec<(String, u64)> = Vec::new();
    let mut conn = Conn::new(
        state.clone(),
        spec.target.clone(),
        spec.transport,
        spec.auth.clone(),
    );
    let mut server = Value::Null;

    // connect (always runs: everything needs it).
    let o = conn.connect().await;
    lat.push(("connect".into(), o.ms));
    let connected = o.result.is_some();
    if let Some(r) = &o.result {
        let info = r
            .get("_meta")
            .and_then(|m| m.get("io.modelcontextprotocol/serverInfo"))
            .or_else(|| r.get("serverInfo"))
            .cloned()
            .unwrap_or(Value::Null);
        server = json!({
            "name": info.get("name"), "title": info.get("title"), "version": info.get("version"),
            "capabilities": r.get("capabilities").cloned().unwrap_or(json!({})),
            "protocol_version": r.get("protocolVersion"),
            "supported_versions": r.get("supportedVersions"),
            "instructions": r.get("instructions").and_then(Value::as_str).map(|s| s.chars().take(500).collect::<String>()),
        });
    }
    results.push(CheckResult {
        id: "connect",
        status: if connected {
            Status::Pass
        } else {
            Status::Fail
        },
        message: if connected {
            format!(
                "{} {} answered in {} ms{}",
                server["title"]
                    .as_str()
                    .or(server["name"].as_str())
                    .unwrap_or("the server"),
                server["version"].as_str().unwrap_or(""),
                o.ms,
                conn.session_id
                    .as_ref()
                    .map(|_| ", session started")
                    .unwrap_or("")
            )
        } else {
            let www = o
                .header("www-authenticate")
                .map(|w| format!(" (WWW-Authenticate: {w})"))
                .unwrap_or_default();
            format!("{}{www}", error_text(&o))
        },
        ms: o.ms,
        details: json!({ "status": o.status }),
    });

    if !connected {
        for (id, _) in CHECKS.iter().skip(1) {
            if wanted(id) {
                results.push(CheckResult {
                    id,
                    status: Status::Skip,
                    message: "not connected".into(),
                    ms: 0,
                    details: Value::Null,
                });
            }
        }
        return report(state, &spec, started, started_at, server, results, lat);
    }
    let caps = server["capabilities"].clone();

    // version
    if wanted("version") {
        let requested = spec.transport.version();
        let (status, message) = match spec.transport {
            Transport::Modern => {
                let supported: Vec<String> = server["supported_versions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if supported.iter().any(|v| v == requested) {
                    (
                        Status::Pass,
                        format!("{requested} is supported ({})", supported.join(", ")),
                    )
                } else if supported.is_empty() {
                    (
                        Status::Warn,
                        "server/discover lists no supportedVersions".to_string(),
                    )
                } else {
                    (
                        Status::Fail,
                        format!("{requested} is not among {}", supported.join(", ")),
                    )
                }
            }
            _ => match server["protocol_version"].as_str() {
                Some(v) if v == requested => (Status::Pass, format!("the server accepted {v}")),
                Some(v) => (
                    Status::Warn,
                    format!("asked for {requested}, the server answered {v}"),
                ),
                None => (
                    Status::Fail,
                    "initialize returned no protocolVersion".to_string(),
                ),
            },
        };
        results.push(CheckResult {
            id: "version",
            status,
            message,
            ms: 0,
            details: Value::Null,
        });
    }

    // auth_refusal
    if wanted("auth_refusal") {
        if spec.auth.is_empty() {
            results.push(CheckResult {
                id: "auth_refusal",
                status: Status::Skip,
                message: "no credentials were given".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let mut bare = Conn::new(
                state.clone(),
                spec.target.clone(),
                spec.transport,
                Vec::new(),
            );
            let o = bare.connect().await;
            lat.push(("auth_refusal".into(), o.ms));
            let (status, message) = if o.status == 401 || o.status == 403 {
                let www = o.header("www-authenticate");
                (
                    Status::Pass,
                    format!(
                        "{} without credentials{}",
                        o.status,
                        www.map(|w| format!("; WWW-Authenticate: {w}"))
                            .unwrap_or_default()
                    ),
                )
            } else if o.result.is_some() {
                (
                    Status::Fail,
                    "the server answered without credentials".to_string(),
                )
            } else {
                (
                    Status::Warn,
                    format!("refused, but not with 401 or 403: {}", error_text(&o)),
                )
            };
            results.push(CheckResult {
                id: "auth_refusal",
                status,
                message,
                ms: o.ms,
                details: json!({ "status": o.status }),
            });
            bare.close().await;
        }
    }

    // tools_list (also feeds tools_call and bad_arguments)
    let mut tools: Vec<Value> = Vec::new();
    let tools_cap = caps.get("tools").is_some();
    if wanted("tools_list")
        || wanted("tools_call")
        || wanted("bad_arguments")
        || wanted("routing_headers")
    {
        let t0 = Instant::now();
        match list_all(&mut conn, "tools/list", "tools", &mut lat).await {
            Ok(list) => {
                tools = list;
                for t in &tools {
                    if let (Some(n), Some(s)) =
                        (t.get("name").and_then(Value::as_str), t.get("inputSchema"))
                    {
                        conn.tool_schemas.insert(n.to_string(), s.clone());
                    }
                }
                if wanted("tools_list") {
                    let mut problems = Vec::new();
                    for t in &tools {
                        let name = t.get("name").and_then(Value::as_str).unwrap_or("");
                        if name.is_empty() {
                            problems.push(
                                json!({ "tool": Value::Null, "problem": "a tool without a name" }),
                            );
                            continue;
                        }
                        match t.get("inputSchema") {
                            None => {
                                problems.push(json!({ "tool": name, "problem": "no inputSchema" }))
                            }
                            Some(s) => {
                                if let Some(p) = schema_problem(s) {
                                    problems.push(json!({ "tool": name, "problem": p }));
                                }
                            }
                        }
                    }
                    let status = if !problems.is_empty() {
                        Status::Fail
                    } else if tools.is_empty() && tools_cap {
                        Status::Warn
                    } else {
                        Status::Pass
                    };
                    let message = if problems.is_empty() {
                        format!("{} tool(s), every inputSchema usable", tools.len())
                    } else {
                        format!(
                            "{} of {} tool(s) have a schema problem: {}",
                            problems.len(),
                            tools.len(),
                            problems
                                .iter()
                                .take(3)
                                .map(|p| format!(
                                    "{} ({})",
                                    p["tool"].as_str().unwrap_or("?"),
                                    p["problem"].as_str().unwrap_or("")
                                ))
                                .collect::<Vec<_>>()
                                .join("; ")
                        )
                    };
                    results.push(CheckResult {
                        id: "tools_list",
                        status,
                        message,
                        ms: t0.elapsed().as_millis() as u64,
                        details: json!({ "tools": tools.len(), "problems": problems }),
                    });
                }
            }
            Err(o) => {
                if wanted("tools_list") {
                    let status = if tools_cap {
                        Status::Fail
                    } else {
                        Status::Skip
                    };
                    results.push(CheckResult {
                        id: "tools_list",
                        status,
                        message: if tools_cap {
                            error_text(&o)
                        } else {
                            "the server declares no tools capability".into()
                        },
                        ms: o.ms,
                        details: Value::Null,
                    });
                }
            }
        }
    }

    // tools_call
    if wanted("tools_call") {
        if tools.is_empty() {
            results.push(CheckResult {
                id: "tools_call",
                status: Status::Skip,
                message: "no tools to call".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let t0 = Instant::now();
            let mut rows = Vec::new();
            let (mut ok, mut tool_errors, mut rpc_errors, mut broken, mut skipped) =
                (0, 0, 0, 0, 0);
            for (i, t) in tools.iter().enumerate() {
                let name = t
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let destructive = t
                    .get("annotations")
                    .and_then(|a| a.get("destructiveHint"))
                    .and_then(Value::as_bool)
                    == Some(true);
                if (destructive && !spec.include_destructive) || i >= spec.max_tools {
                    skipped += 1;
                    rows.push(json!({ "tool": name, "outcome": "skipped", "reason": if destructive { "destructive (include_destructive to call it)" } else { "over max_tools" } }));
                    continue;
                }
                let (args, source) = arguments_for(state, &spec, t, 100 + i as u64).await;
                let o = conn
                    .request("tools/call", json!({ "name": name, "arguments": args }))
                    .await;
                lat.push(("tools/call".into(), o.ms));
                let (outcome, message) = match (&o.result, &o.error, &o.transport_error) {
                    (Some(r), _, _)
                        if r.get("resultType").and_then(Value::as_str)
                            == Some("input_required") =>
                    {
                        ok += 1;
                        (
                            "input_required",
                            "asked for input (elicitation): the tool reacted".to_string(),
                        )
                    }
                    (Some(r), _, _) if is_error_result(r) => {
                        tool_errors += 1;
                        let text = r
                            .get("content")
                            .and_then(Value::as_array)
                            .and_then(|c| c.first())
                            .and_then(|c| c.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        (
                            "tool_error",
                            format!("isError: {}", text.chars().take(160).collect::<String>()),
                        )
                    }
                    (Some(_), _, _) => {
                        ok += 1;
                        ("ok", String::new())
                    }
                    (None, Some(_), _) => {
                        rpc_errors += 1;
                        ("rpc_error", error_text(&o))
                    }
                    _ => {
                        broken += 1;
                        ("transport_error", error_text(&o))
                    }
                };
                rows.push(json!({ "tool": name, "outcome": outcome, "message": message, "ms": o.ms, "arguments": args, "arguments_source": source }));
            }
            let status = if broken > 0 {
                Status::Fail
            } else if rpc_errors > 0 {
                Status::Warn
            } else {
                Status::Pass
            };
            let message = format!(
                "{ok} answered, {tool_errors} with isError, {rpc_errors} JSON-RPC error(s), {broken} transport error(s), {skipped} skipped"
            );
            results.push(CheckResult {
                id: "tools_call",
                status,
                message,
                ms: t0.elapsed().as_millis() as u64,
                details: json!({ "calls": rows }),
            });
        }
    }

    // unknown_tool
    if wanted("unknown_tool") {
        let o = conn
            .request(
                "tools/call",
                json!({ "name": "rustybin-check-no-such-tool", "arguments": {} }),
            )
            .await;
        lat.push(("tools/call".into(), o.ms));
        let (status, message) = match (&o.result, error_code(&o)) {
            (_, Some(c)) => (Status::Pass, format!("JSON-RPC error {c}")),
            (Some(r), None) if is_error_result(r) => (
                Status::Warn,
                "answered isError instead of a JSON-RPC error (-32602)".to_string(),
            ),
            (Some(_), None) => (
                Status::Fail,
                "the server answered a call to a tool that does not exist".to_string(),
            ),
            _ => (Status::Fail, error_text(&o)),
        };
        results.push(CheckResult {
            id: "unknown_tool",
            status,
            message,
            ms: o.ms,
            details: Value::Null,
        });
    }

    // bad_arguments
    if wanted("bad_arguments") {
        let candidate = tools.iter().find(|t| {
            let req = t
                .get("inputSchema")
                .and_then(|s| s.get("required"))
                .and_then(Value::as_array);
            let destructive = t
                .get("annotations")
                .and_then(|a| a.get("destructiveHint"))
                .and_then(Value::as_bool)
                == Some(true);
            req.is_some_and(|r| !r.is_empty()) && !destructive
        });
        match candidate {
            None => results.push(CheckResult {
                id: "bad_arguments",
                status: Status::Skip,
                message: "no tool declares required arguments".into(),
                ms: 0,
                details: Value::Null,
            }),
            Some(t) => {
                let name = t["name"].as_str().unwrap_or("").to_string();
                let o = conn
                    .request("tools/call", json!({ "name": name, "arguments": {} }))
                    .await;
                lat.push(("tools/call".into(), o.ms));
                let (status, message) = match (&o.result, error_code(&o)) {
                    (_, Some(c)) => (Status::Pass, format!("{name}: JSON-RPC error {c}")),
                    (Some(r), None) if is_error_result(r) => {
                        (Status::Pass, format!("{name}: isError"))
                    }
                    (Some(_), None) => (
                        Status::Warn,
                        format!("{name} accepted a call without its required arguments"),
                    ),
                    _ => (Status::Fail, error_text(&o)),
                };
                results.push(CheckResult {
                    id: "bad_arguments",
                    status,
                    message,
                    ms: o.ms,
                    details: json!({ "tool": name }),
                });
            }
        }
    }

    // unknown_method
    if wanted("unknown_method") {
        let o = conn.request("rustybin/no-such-method", json!({})).await;
        lat.push(("unknown".into(), o.ms));
        let (status, message) = match error_code(&o) {
            Some(-32601) => (Status::Pass, "-32601 method not found".to_string()),
            Some(c) => (Status::Warn, format!("JSON-RPC error {c}, not -32601")),
            None if o.result.is_some() => (
                Status::Fail,
                "the server answered an unknown method".to_string(),
            ),
            None => (Status::Warn, error_text(&o)),
        };
        results.push(CheckResult {
            id: "unknown_method",
            status,
            message,
            ms: o.ms,
            details: Value::Null,
        });
    }

    // resources
    if wanted("resources") {
        if caps.get("resources").is_none() {
            results.push(CheckResult {
                id: "resources",
                status: Status::Skip,
                message: "the server declares no resources capability".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let t0 = Instant::now();
            match list_all(&mut conn, "resources/list", "resources", &mut lat).await {
                Err(o) => results.push(CheckResult {
                    id: "resources",
                    status: Status::Fail,
                    message: format!("resources/list: {}", error_text(&o)),
                    ms: o.ms,
                    details: Value::Null,
                }),
                Ok(list) => {
                    let templates = conn.request("resources/templates/list", json!({})).await;
                    lat.push(("resources/templates/list".into(), templates.ms));
                    let n_templates = templates
                        .result
                        .as_ref()
                        .and_then(|r| r.get("resourceTemplates"))
                        .and_then(Value::as_array)
                        .map(Vec::len);
                    let (status, message) = match list
                        .iter()
                        .find_map(|r| r.get("uri").and_then(Value::as_str))
                    {
                        None => (
                            Status::Pass,
                            format!(
                                "no resources listed{}",
                                n_templates
                                    .map(|n| format!(", {n} template(s)"))
                                    .unwrap_or_default()
                            ),
                        ),
                        Some(uri) => {
                            let o = conn.request("resources/read", json!({ "uri": uri })).await;
                            lat.push(("resources/read".into(), o.ms));
                            match &o.result {
                                Some(r)
                                    if r.get("contents")
                                        .and_then(Value::as_array)
                                        .is_some_and(|c| !c.is_empty()) =>
                                {
                                    (
                                        Status::Pass,
                                        format!(
                                            "{} resource(s); {uri} read{}",
                                            list.len(),
                                            n_templates
                                                .map(|n| format!(", {n} template(s)"))
                                                .unwrap_or_default()
                                        ),
                                    )
                                }
                                Some(_) => (Status::Warn, format!("{uri} read with no contents")),
                                None => {
                                    (Status::Fail, format!("reading {uri}: {}", error_text(&o)))
                                }
                            }
                        }
                    };
                    results.push(CheckResult {
                        id: "resources",
                        status,
                        message,
                        ms: t0.elapsed().as_millis() as u64,
                        details: json!({ "resources": list.len(), "templates": n_templates }),
                    });
                }
            }
        }
    }

    // prompts
    if wanted("prompts") {
        if caps.get("prompts").is_none() {
            results.push(CheckResult {
                id: "prompts",
                status: Status::Skip,
                message: "the server declares no prompts capability".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let t0 = Instant::now();
            match list_all(&mut conn, "prompts/list", "prompts", &mut lat).await {
                Err(o) => results.push(CheckResult {
                    id: "prompts",
                    status: Status::Fail,
                    message: format!("prompts/list: {}", error_text(&o)),
                    ms: o.ms,
                    details: Value::Null,
                }),
                Ok(list) => {
                    let (status, message) = match list.first() {
                        None => (Status::Pass, "no prompts listed".to_string()),
                        Some(p) => {
                            let name = p
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let mut args = Map::new();
                            for a in p
                                .get("arguments")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default()
                            {
                                if a.get("required").and_then(Value::as_bool) == Some(true) {
                                    if let Some(n) = a.get("name").and_then(Value::as_str) {
                                        args.insert(n.to_string(), json!("example"));
                                    }
                                }
                            }
                            let o = conn
                                .request("prompts/get", json!({ "name": name, "arguments": args }))
                                .await;
                            lat.push(("prompts/get".into(), o.ms));
                            match &o.result {
                                Some(r)
                                    if r.get("messages")
                                        .and_then(Value::as_array)
                                        .is_some_and(|m| !m.is_empty()) =>
                                {
                                    (
                                        Status::Pass,
                                        format!("{} prompt(s); {name} rendered", list.len()),
                                    )
                                }
                                Some(_) => (Status::Warn, format!("{name} rendered no messages")),
                                None => (
                                    Status::Warn,
                                    format!(
                                        "{name}: {} (the generated arguments may not fit)",
                                        error_text(&o)
                                    ),
                                ),
                            }
                        }
                    };
                    results.push(CheckResult {
                        id: "prompts",
                        status,
                        message,
                        ms: t0.elapsed().as_millis() as u64,
                        details: json!({ "prompts": list.len() }),
                    });
                }
            }
        }
    }

    // ping
    if wanted("ping") {
        let o = conn.request("ping", json!({})).await;
        lat.push(("ping".into(), o.ms));
        let (status, message) = match (&o.result, error_code(&o)) {
            (Some(_), _) => (Status::Pass, format!("{} ms", o.ms)),
            (None, Some(-32601)) => (Status::Warn, "ping is not implemented (-32601)".to_string()),
            _ => (Status::Fail, error_text(&o)),
        };
        results.push(CheckResult {
            id: "ping",
            status,
            message,
            ms: o.ms,
            details: Value::Null,
        });
    }

    // routing_headers (modern only)
    if wanted("routing_headers") {
        if spec.transport != Transport::Modern {
            results.push(CheckResult {
                id: "routing_headers",
                status: Status::Skip,
                message: "2026-07-28 only".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let params = json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": super::client::MODERN, "io.modelcontextprotocol/clientCapabilities": {} } });
            let mut headers = conn.request_headers("tools/list", &params);
            for (k, v) in headers.iter_mut() {
                if k == "mcp-method" {
                    *v = "prompts/list".into();
                }
            }
            let msg =
                json!({ "jsonrpc": "2.0", "id": 9001, "method": "tools/list", "params": params });
            let o = conn.raw(&msg, headers).await;
            lat.push(("tools/list".into(), o.ms));
            let (status, message) = match (o.status, error_code(&o)) {
                (400, _) | (_, Some(-32020)) => (Status::Pass, format!("refused (HTTP {}{})", o.status, error_code(&o).map(|c| format!(", {c}")).unwrap_or_default())),
                _ if o.result.is_some() => (Status::Warn, "the server answered despite Mcp-Method: prompts/list on a tools/list body (gateways route on the header)".to_string()),
                _ => (Status::Warn, error_text(&o)),
            };
            results.push(CheckResult {
                id: "routing_headers",
                status,
                message,
                ms: o.ms,
                details: Value::Null,
            });
        }
    }

    // server_stream and session (session transports only)
    let is_session = matches!(spec.transport, Transport::Session(_));
    if wanted("server_stream") {
        if !is_session {
            results.push(CheckResult {
                id: "server_stream",
                status: Status::Skip,
                message: "session transports only".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let (status, message) = match conn.open_stream().await {
                Ok((200, ct)) if ct.contains("text/event-stream") => {
                    (Status::Pass, "GET opened an event stream".to_string())
                }
                Ok((405, _)) => (
                    Status::Pass,
                    "405: the server offers no stream (allowed)".to_string(),
                ),
                Ok((s, ct)) => (Status::Warn, format!("GET answered {s} {ct}")),
                Err(e) => (Status::Warn, e),
            };
            results.push(CheckResult {
                id: "server_stream",
                status,
                message,
                ms: 0,
                details: Value::Null,
            });
        }
    }
    if wanted("session") {
        if !is_session {
            results.push(CheckResult {
                id: "session",
                status: Status::Skip,
                message: "session transports only".into(),
                ms: 0,
                details: Value::Null,
            });
        } else {
            let (status, message) = match conn.session_id.clone() {
                None => (
                    Status::Fail,
                    "initialize returned no Mcp-Session-Id".to_string(),
                ),
                Some(_) => match conn.close().await {
                    Some(s) if s < 300 => {
                        let o = conn.request("ping", json!({})).await;
                        match o.status {
                            404 => (
                                Status::Pass,
                                format!("DELETE {s}; the old session id answers 404"),
                            ),
                            other => (
                                Status::Warn,
                                format!("DELETE {s}, but the old session id answered {other}"),
                            ),
                        }
                    }
                    Some(405) => (
                        Status::Warn,
                        "DELETE 405: the server does not let clients end sessions".to_string(),
                    ),
                    Some(s) => (Status::Fail, format!("DELETE answered {s}")),
                    None => (Status::Fail, "DELETE failed".to_string()),
                },
            };
            results.push(CheckResult {
                id: "session",
                status,
                message,
                ms: 0,
                details: Value::Null,
            });
        }
    } else if is_session {
        conn.close().await;
    }

    report(state, &spec, started, started_at, server, results, lat)
}

fn report(
    _state: &AppState,
    spec: &RunSpec,
    started: Instant,
    started_at: String,
    server: Value,
    results: Vec<CheckResult>,
    lat: Vec<(String, u64)>,
) -> Value {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &results {
        *counts.entry(r.status.as_str()).or_insert(0) += 1;
    }
    let mut all: Vec<u64> = lat.iter().map(|(_, ms)| *ms).collect();
    all.sort_unstable();
    let mut by_method: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (m, ms) in &lat {
        by_method.entry(m.clone()).or_default().push(*ms);
    }
    let by_method: Map<String, Value> = by_method
        .into_iter()
        .map(|(m, mut v)| {
            v.sort_unstable();
            (m, json!({ "calls": v.len(), "p50_ms": percentile(&v, 0.5), "max_ms": v.last().copied().unwrap_or(0) }))
        })
        .collect();
    let failed = counts.get("fail").copied().unwrap_or(0);
    json!({
        "id": format!("run-{}", uuid::Uuid::new_v4().simple()),
        "started_at": started_at,
        "duration_ms": started.elapsed().as_millis() as u64,
        "target": spec.target.as_str(),
        "relayed": matches!(spec.target, Target::Remote(_)),
        "transport": spec.transport.label(),
        "version": spec.transport.version(),
        "server": server,
        "ok": failed == 0,
        "summary": {
            "total": results.len(),
            "passed": counts.get("pass").copied().unwrap_or(0),
            "warnings": counts.get("warn").copied().unwrap_or(0),
            "failed": failed,
            "skipped": counts.get("skip").copied().unwrap_or(0),
        },
        "checks": results.iter().map(|r| json!({
            "id": r.id,
            "title": CHECKS.iter().find(|(c, _)| *c == r.id).map(|(_, t)| *t).unwrap_or(""),
            "status": r.status.as_str(),
            "message": r.message,
            "duration_ms": r.ms,
            "details": r.details,
        })).collect::<Vec<_>>(),
        "latency": {
            "calls": all.len(),
            "p50_ms": percentile(&all, 0.5),
            "p95_ms": percentile(&all, 0.95),
            "max_ms": all.last().copied().unwrap_or(0),
            "by_method": by_method,
        },
    })
}

/// The report as JUnit XML (warnings in `system-out`, skips as skipped).
pub fn junit(report: &Value) -> String {
    let esc = crate::content_negotiation::xml_escape;
    let checks = report["checks"].as_array().cloned().unwrap_or_default();
    let failures = checks.iter().filter(|c| c["status"] == "fail").count();
    let skipped = checks.iter().filter(|c| c["status"] == "skip").count();
    let secs = report["duration_ms"].as_u64().unwrap_or(0) as f64 / 1000.0;
    let mut cases = String::new();
    for c in &checks {
        let id = c["id"].as_str().unwrap_or("");
        let title = c["title"].as_str().unwrap_or("");
        let message = c["message"].as_str().unwrap_or("");
        let t = c["duration_ms"].as_u64().unwrap_or(0) as f64 / 1000.0;
        cases.push_str(&format!(
            "    <testcase classname=\"mcp.{}\" name=\"{}: {}\" time=\"{t:.3}\">",
            esc(report["transport"].as_str().unwrap_or("")),
            esc(id),
            esc(title)
        ));
        match c["status"].as_str() {
            Some("fail") => cases.push_str(&format!("<failure message=\"{}\"/>", esc(message))),
            Some("skip") => cases.push_str(&format!("<skipped message=\"{}\"/>", esc(message))),
            Some("warn") => cases.push_str(&format!(
                "<system-out>warning: {}</system-out>",
                esc(message)
            )),
            _ => cases.push_str(&format!("<system-out>{}</system-out>", esc(message))),
        }
        cases.push_str("</testcase>\n");
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n  <testsuite name=\"mcp-checks {}\" tests=\"{}\" failures=\"{failures}\" errors=\"0\" skipped=\"{skipped}\" time=\"{secs:.3}\" timestamp=\"{}\">\n{cases}  </testsuite>\n</testsuites>\n",
        esc(report["target"].as_str().unwrap_or("")),
        checks.len(),
        esc(report["started_at"].as_str().unwrap_or("")),
    )
}
