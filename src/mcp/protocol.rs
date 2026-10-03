//! JSON-RPC 2.0 framing, MCP protocol versions and error codes.
//!
//! Versions: `2026-07-28` is "modern" (stateless, per-request `_meta`
//! envelope); `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05` are
//! "legacy" (initialize handshake, sessions). See the specification's
//! `basic/versioning` page for the era model.

use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use serde_json::{json, Map, Value};

/// `_meta` key carrying the protocol version (modern era).
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key carrying the client capabilities (modern era).
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// `_meta` key carrying the client implementation info (modern era).
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// `_meta` key carrying the per-request log level (modern era).
pub const META_LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
/// `_meta` key carrying the server implementation info on results (modern era).
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// `_meta` key tagging notifications delivered on a `subscriptions/listen` stream.
pub const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

pub const HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";
pub const HEADER_SESSION_ID: &str = "mcp-session-id";
pub const HEADER_METHOD: &str = "mcp-method";
pub const HEADER_NAME: &str = "mcp-name";
pub const HEADER_PARAM_PREFIX: &str = "mcp-param-";

// Standard JSON-RPC error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
// MCP error codes (2026-07-28 allocation).
pub const HEADER_MISMATCH: i64 = -32020;
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// Resource not found in 2025-11-25 and earlier (replaced by -32602 in 2026-07-28).
pub const LEGACY_RESOURCE_NOT_FOUND: i64 = -32002;
/// Implementation-defined: session not found (legacy streamable HTTP).
pub const SESSION_NOT_FOUND: i64 = -32001;
/// Implementation-defined: generic server-side refusal (auth/scope).
pub const SERVER_ERROR: i64 = -32000;

/// A protocol revision this server implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Version {
    V2024_11_05,
    V2025_03_26,
    V2025_06_18,
    V2025_11_25,
    V2026_07_28,
}

impl Version {
    /// Every supported revision, newest first.
    pub const ALL: [Version; 5] = [
        Version::V2026_07_28,
        Version::V2025_11_25,
        Version::V2025_06_18,
        Version::V2025_03_26,
        Version::V2024_11_05,
    ];
    /// Newest revision reachable through the initialize handshake.
    pub const LATEST_LEGACY: Version = Version::V2025_11_25;
    pub const LATEST: Version = Version::V2026_07_28;

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "2024-11-05" => Version::V2024_11_05,
            "2025-03-26" => Version::V2025_03_26,
            "2025-06-18" => Version::V2025_06_18,
            "2025-11-25" => Version::V2025_11_25,
            "2026-07-28" => Version::V2026_07_28,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Version::V2024_11_05 => "2024-11-05",
            Version::V2025_03_26 => "2025-03-26",
            Version::V2025_06_18 => "2025-06-18",
            Version::V2025_11_25 => "2025-11-25",
            Version::V2026_07_28 => "2026-07-28",
        }
    }

    /// Stateless per-request-envelope era (no initialize, no sessions).
    pub fn is_modern(self) -> bool {
        self >= Version::V2026_07_28
    }

    /// Tool annotations, audio content, completions, JSON-RPC batching (2025-03-26 only).
    pub fn has_annotations(self) -> bool {
        self >= Version::V2025_03_26
    }

    /// `title`, `outputSchema` / `structuredContent`, `resource_link`, elicitation.
    pub fn has_structured_output(self) -> bool {
        self >= Version::V2025_06_18
    }

    /// Elicitation requests carry `mode` (2025-11-25 and later).
    pub fn has_elicitation_mode(self) -> bool {
        self >= Version::V2025_11_25
    }

    /// The `MCP-Protocol-Version` header exists (2025-06-18 and later).
    pub fn has_version_header(self) -> bool {
        self >= Version::V2025_06_18
    }

    /// Every supported version as strings, newest first.
    pub fn all_strings() -> Vec<&'static str> {
        Self::ALL.iter().map(|v| v.as_str()).collect()
    }
}

/// Is this a version selectable with the initialize handshake?
pub fn is_legacy_version_str(s: &str) -> bool {
    Version::parse(s).is_some_and(|v| !v.is_modern())
}

/// Version negotiation for `initialize`: echo a supported legacy version,
/// otherwise answer with the latest legacy one.
pub fn negotiate_legacy(requested: Option<&str>) -> Version {
    match requested.and_then(Version::parse) {
        Some(v) if !v.is_modern() => v,
        _ => Version::LATEST_LEGACY,
    }
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse_error() -> Self {
        Self::new(PARSE_ERROR, "Parse error")
    }

    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(INVALID_REQUEST, msg)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, msg)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, msg)
    }

    pub fn header_mismatch(msg: impl Into<String>) -> Self {
        Self::new(HEADER_MISMATCH, msg)
    }

    pub fn unsupported_version(requested: &str) -> Self {
        Self::new(UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version").with_data(json!({
            "supported": Version::all_strings(),
            "requested": requested,
        }))
    }

    pub fn missing_capability(required: Value) -> Self {
        Self::new(
            MISSING_REQUIRED_CLIENT_CAPABILITY,
            "Missing required client capability",
        )
        .with_data(json!({ "requiredCapabilities": required }))
    }

    /// Resource not found: -32002 up to 2025-11-25, -32602 from 2026-07-28.
    pub fn resource_not_found(uri: &str, version: Version) -> Self {
        let code = if version.is_modern() {
            INVALID_PARAMS
        } else {
            LEGACY_RESOURCE_NOT_FOUND
        };
        Self::new(code, "Resource not found").with_data(json!({ "uri": uri }))
    }

    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("code".into(), json!(self.code));
        obj.insert("message".into(), json!(self.message));
        if let Some(data) = &self.data {
            obj.insert("data".into(), data.clone());
        }
        Value::Object(obj)
    }

    /// HTTP status for a modern-era JSON (non-SSE) error response
    /// (Streamable HTTP 2026-07-28: 400 for envelope problems, 404 for unknown methods).
    pub fn modern_http_status(&self) -> StatusCode {
        match self.code {
            PARSE_ERROR
            | INVALID_REQUEST
            | INVALID_PARAMS
            | HEADER_MISMATCH
            | MISSING_REQUIRED_CLIENT_CAPABILITY
            | UNSUPPORTED_PROTOCOL_VERSION => StatusCode::BAD_REQUEST,
            METHOD_NOT_FOUND => StatusCode::NOT_FOUND,
            _ => StatusCode::OK,
        }
    }
}

/// `{"jsonrpc":"2.0","id":..,"result":..}`
pub fn result_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// `{"jsonrpc":"2.0","id":..|null,"error":{..}}`
pub fn error_response(id: Option<&Value>, err: &RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.cloned().unwrap_or(Value::Null), "error": err.to_json() })
}

/// `{"jsonrpc":"2.0","method":..,"params":..}`
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A decoded JSON-RPC message.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// A response (result or error) from the client to a server-initiated request.
    Response {
        id: Value,
        body: Value,
    },
}

/// Classify one JSON value as a JSON-RPC message.
/// On failure returns the error plus the request id when one was readable.
pub fn classify(v: &Value) -> Result<Message, (RpcError, Option<Value>)> {
    let Some(obj) = v.as_object() else {
        return Err((
            RpcError::invalid_request("Invalid Request: expected a JSON-RPC object"),
            None,
        ));
    };
    let id = obj.get("id").cloned();
    let id_ok = |id: &Value| id.is_string() || id.is_i64() || id.is_u64();
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err((
            RpcError::invalid_request("Invalid Request: jsonrpc must be \"2.0\""),
            id.filter(id_ok),
        ));
    }
    if let Some(method) = obj.get("method") {
        let Some(method) = method.as_str() else {
            return Err((
                RpcError::invalid_request("Invalid Request: method must be a string"),
                id.filter(id_ok),
            ));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        if !(params.is_null() || params.is_object()) {
            return Err((
                RpcError::invalid_request("Invalid Request: params must be an object"),
                id.filter(id_ok),
            ));
        }
        return match id {
            None => Ok(Message::Notification {
                method: method.to_string(),
                params,
            }),
            Some(id) if id_ok(&id) => Ok(Message::Request {
                id,
                method: method.to_string(),
                params,
            }),
            Some(_) => Err((
                RpcError::invalid_request("Invalid Request: id must be a string or an integer"),
                None,
            )),
        };
    }
    if obj.contains_key("result") || obj.contains_key("error") {
        if let Some(id) = id.clone().filter(id_ok) {
            return Ok(Message::Response {
                id,
                body: v.clone(),
            });
        }
    }
    Err((
        RpcError::invalid_request("Invalid Request: not a request, notification or response"),
        id.filter(id_ok),
    ))
}

/// Decode an `Mcp-Name` / `Mcp-Param-*` header value: verbatim unless it uses
/// the `=?base64?...?=` sentinel. `None` for a malformed sentinel.
pub fn decode_header_value(raw: &str) -> Option<String> {
    let Some(payload) = raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    else {
        return Some(raw.to_string());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Single header value as a string; `Err` when it appears more than once or
/// is not visible ASCII.
pub fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, String> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(format!("{name} header appears more than once"));
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| format!("{name} header contains invalid characters"))
}

/// Log levels (RFC 5424 severities), lowest first.
pub const LOG_LEVELS: &[&str] = &[
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

/// Severity rank of a log level name.
pub fn log_level_rank(level: &str) -> Option<usize> {
    LOG_LEVELS.iter().position(|l| *l == level)
}

/// Extract `params._meta.progressToken` (string or integer).
pub fn progress_token(params: &Value) -> Option<Value> {
    params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .filter(|t| t.is_string() || t.is_i64() || t.is_u64())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_messages() {
        let req = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
        assert!(matches!(classify(&req), Ok(Message::Request { .. })));
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(matches!(classify(&note), Ok(Message::Notification { .. })));
        let resp = json!({"jsonrpc":"2.0","id":"x","result":{}});
        assert!(matches!(classify(&resp), Ok(Message::Response { .. })));
        let bad = json!({"jsonrpc":"1.0","id":1,"method":"ping"});
        let (err, id) = classify(&bad).expect_err("invalid");
        assert_eq!(err.code, INVALID_REQUEST);
        assert_eq!(id, Some(json!(1)));
        let null_id = json!({"jsonrpc":"2.0","id":null,"method":"ping"});
        assert!(classify(&null_id).is_err());
        let bad_params = json!({"jsonrpc":"2.0","id":1,"method":"x","params":[1]});
        assert!(classify(&bad_params).is_err());
    }

    #[test]
    fn versions_and_negotiation() {
        assert_eq!(negotiate_legacy(Some("2025-06-18")), Version::V2025_06_18);
        assert_eq!(negotiate_legacy(Some("1999-01-01")), Version::V2025_11_25);
        assert_eq!(negotiate_legacy(Some("2026-07-28")), Version::V2025_11_25);
        assert!(Version::V2026_07_28.is_modern());
        assert!(!Version::V2025_11_25.is_modern());
        assert!(is_legacy_version_str("2024-11-05"));
        assert!(!is_legacy_version_str("2026-07-28"));
    }

    #[test]
    fn header_value_sentinel() {
        assert_eq!(decode_header_value("plain").as_deref(), Some("plain"));
        assert_eq!(
            decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, \u{4e16}\u{754c}")
        );
        assert_eq!(decode_header_value("=?base64?***?="), None);
    }

    #[test]
    fn modern_status_mapping() {
        assert_eq!(
            RpcError::method_not_found("x").modern_http_status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            RpcError::unsupported_version("x").modern_http_status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(RpcError::internal("x").modern_http_status(), StatusCode::OK);
        let err = RpcError::resource_not_found("a://b", Version::V2025_06_18);
        assert_eq!(err.code, LEGACY_RESOURCE_NOT_FOUND);
        let err = RpcError::resource_not_found("a://b", Version::V2026_07_28);
        assert_eq!(err.code, INVALID_PARAMS);
    }
}
