//! A2A errors and their binding specific representations (spec section 5.4).

use axum::http::StatusCode;
use serde_json::{json, Map, Value};

use super::model::Version;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    TaskNotFound,
    TaskNotCancelable,
    PushNotificationNotSupported,
    UnsupportedOperation,
    ContentTypeNotSupported,
    InvalidAgentResponse,
    ExtendedAgentCardNotConfigured,
    ExtensionSupportRequired,
    VersionNotSupported,
    /// Missing or invalid credentials (no A2A code: JSON-RPC server error
    /// -32000 with HTTP 401, REST 401 UNAUTHENTICATED).
    Unauthenticated,
    JsonParse,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    Internal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct A2aError {
    pub kind: Kind,
    pub message: String,
    /// Extra `ErrorInfo.metadata` entries (string values per google.rpc).
    pub metadata: Vec<(String, String)>,
}

impl A2aError {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            metadata: Vec::new(),
        }
    }

    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.metadata.push((key.to_string(), value.into()));
        self
    }

    pub fn task_not_found(id: &str) -> Self {
        Self::new(Kind::TaskNotFound, "Task not found").with("taskId", id)
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(Kind::InvalidParams, msg)
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::new(Kind::UnsupportedOperation, msg)
    }

    pub fn version_not_supported(requested: &str) -> Self {
        Self::new(
            Kind::VersionNotSupported,
            format!(
                "A2A version '{requested}' is not supported; supported versions: 1.0, 0.3 \
                 (send A2A-Version: 1.0 for SendMessage-style methods, omit it or send 0.3 for message/send-style methods)"
            ),
        )
        .with("requestedVersion", requested)
        .with("supportedVersions", "1.0,0.3")
    }

    /// JSON-RPC error code.
    pub fn code(&self) -> i64 {
        match self.kind {
            Kind::TaskNotFound => -32001,
            Kind::TaskNotCancelable => -32002,
            Kind::PushNotificationNotSupported => -32003,
            Kind::UnsupportedOperation => -32004,
            Kind::ContentTypeNotSupported => -32005,
            Kind::InvalidAgentResponse => -32006,
            Kind::ExtendedAgentCardNotConfigured => -32007,
            Kind::ExtensionSupportRequired => -32008,
            Kind::VersionNotSupported => -32009,
            Kind::Unauthenticated => -32000,
            Kind::JsonParse => -32700,
            Kind::InvalidRequest => -32600,
            Kind::MethodNotFound => -32601,
            Kind::InvalidParams => -32602,
            Kind::Internal => -32603,
        }
    }

    /// HTTP status for the HTTP+JSON binding.
    pub fn http_status(&self) -> StatusCode {
        match self.kind {
            Kind::TaskNotFound | Kind::MethodNotFound => StatusCode::NOT_FOUND,
            Kind::InvalidAgentResponse | Kind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Kind::Unauthenticated => StatusCode::UNAUTHORIZED,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    /// google.rpc status name.
    pub fn grpc_status(&self) -> &'static str {
        match self.kind {
            Kind::TaskNotFound | Kind::MethodNotFound => "NOT_FOUND",
            Kind::ContentTypeNotSupported
            | Kind::InvalidParams
            | Kind::InvalidRequest
            | Kind::JsonParse => "INVALID_ARGUMENT",
            Kind::InvalidAgentResponse | Kind::Internal => "INTERNAL",
            Kind::Unauthenticated => "UNAUTHENTICATED",
            _ => "FAILED_PRECONDITION",
        }
    }

    /// `ErrorInfo.reason`: the error type in UPPER_SNAKE_CASE without "Error".
    pub fn reason(&self) -> &'static str {
        match self.kind {
            Kind::TaskNotFound => "TASK_NOT_FOUND",
            Kind::TaskNotCancelable => "TASK_NOT_CANCELABLE",
            Kind::PushNotificationNotSupported => "PUSH_NOTIFICATION_NOT_SUPPORTED",
            Kind::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            Kind::ContentTypeNotSupported => "CONTENT_TYPE_NOT_SUPPORTED",
            Kind::InvalidAgentResponse => "INVALID_AGENT_RESPONSE",
            Kind::ExtendedAgentCardNotConfigured => "EXTENDED_AGENT_CARD_NOT_CONFIGURED",
            Kind::ExtensionSupportRequired => "EXTENSION_SUPPORT_REQUIRED",
            Kind::VersionNotSupported => "VERSION_NOT_SUPPORTED",
            Kind::Unauthenticated => "UNAUTHENTICATED",
            Kind::JsonParse => "JSON_PARSE",
            Kind::InvalidRequest => "INVALID_REQUEST",
            Kind::MethodNotFound => "METHOD_NOT_FOUND",
            Kind::InvalidParams => "INVALID_PARAMS",
            Kind::Internal => "INTERNAL_ERROR",
        }
    }

    /// v0.3 error type name (used by v0.3 REST clients).
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            Kind::TaskNotFound => "TaskNotFoundError",
            Kind::TaskNotCancelable => "TaskNotCancelableError",
            Kind::PushNotificationNotSupported => "PushNotificationNotSupportedError",
            Kind::UnsupportedOperation => "UnsupportedOperationError",
            Kind::ContentTypeNotSupported => "ContentTypeNotSupportedError",
            Kind::InvalidAgentResponse => "InvalidAgentResponseError",
            Kind::ExtendedAgentCardNotConfigured => "ExtendedAgentCardNotConfiguredError",
            Kind::ExtensionSupportRequired => "ExtensionSupportRequiredError",
            Kind::VersionNotSupported => "VersionNotSupportedError",
            Kind::Unauthenticated => "AuthenticationError",
            Kind::JsonParse => "JSONParseError",
            Kind::InvalidRequest => "InvalidRequestError",
            Kind::MethodNotFound => "MethodNotFoundError",
            Kind::InvalidParams => "InvalidParamsError",
            Kind::Internal => "InternalError",
        }
    }

    fn error_info(&self) -> Value {
        let mut meta = Map::new();
        for (k, v) in &self.metadata {
            meta.insert(k.clone(), Value::String(v.clone()));
        }
        meta.insert("timestamp".into(), Value::String(super::model::now_ts()));
        json!({
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": self.reason(),
            "domain": "a2a-protocol.org",
            "metadata": meta,
        })
    }

    /// The JSON-RPC `error` object. v1.0 carries `google.rpc.ErrorInfo`
    /// details in `data`; v0.3 uses a plain object.
    pub fn jsonrpc_error(&self, version: Version) -> Value {
        match version {
            Version::V10 => json!({
                "code": self.code(),
                "message": self.message,
                "data": [self.error_info()],
            }),
            Version::V03 => {
                let mut err = json!({ "code": self.code(), "message": self.message });
                if !self.metadata.is_empty() {
                    let mut data = Map::new();
                    for (k, v) in &self.metadata {
                        data.insert(k.clone(), Value::String(v.clone()));
                    }
                    err["data"] = Value::Object(data);
                }
                err
            }
        }
    }

    /// A complete JSON-RPC error response.
    pub fn jsonrpc_response(&self, id: &Value, version: Version) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "error": self.jsonrpc_error(version) })
    }

    /// HTTP+JSON error body (`google.rpc.Status`). v0.3 bodies additionally
    /// carry `type` / `message` at the top level for older clients.
    pub fn rest_body(&self, version: Version) -> Value {
        let mut body = json!({
            "error": {
                "code": self.http_status().as_u16(),
                "status": self.grpc_status(),
                "message": self.message,
                "details": [self.error_info()],
            }
        });
        if version == Version::V03 {
            body["type"] = Value::String(self.type_name().to_string());
            body["message"] = Value::String(self.message.clone());
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_statuses_follow_the_spec_table() {
        let cases = [
            (Kind::TaskNotFound, -32001, 404),
            (Kind::TaskNotCancelable, -32002, 400),
            (Kind::PushNotificationNotSupported, -32003, 400),
            (Kind::UnsupportedOperation, -32004, 400),
            (Kind::ContentTypeNotSupported, -32005, 400),
            (Kind::InvalidAgentResponse, -32006, 500),
            (Kind::ExtendedAgentCardNotConfigured, -32007, 400),
            (Kind::ExtensionSupportRequired, -32008, 400),
            (Kind::VersionNotSupported, -32009, 400),
            (Kind::JsonParse, -32700, 400),
            (Kind::InvalidRequest, -32600, 400),
            (Kind::MethodNotFound, -32601, 404),
            (Kind::InvalidParams, -32602, 400),
            (Kind::Internal, -32603, 500),
        ];
        for (kind, code, status) in cases {
            let e = A2aError::new(kind, "x");
            assert_eq!(e.code(), code, "{kind:?}");
            assert_eq!(e.http_status().as_u16(), status, "{kind:?}");
        }
    }

    #[test]
    fn v1_jsonrpc_error_has_error_info() {
        let e = A2aError::task_not_found("t1");
        let v = e.jsonrpc_error(Version::V10);
        assert_eq!(v["code"], -32001);
        assert_eq!(
            v["data"][0]["@type"],
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
        assert_eq!(v["data"][0]["reason"], "TASK_NOT_FOUND");
        assert_eq!(v["data"][0]["metadata"]["taskId"], "t1");
        let v03 = e.jsonrpc_error(Version::V03);
        assert_eq!(v03["data"]["taskId"], "t1");
        let rest = e.rest_body(Version::V10);
        assert_eq!(rest["error"]["status"], "NOT_FOUND");
        assert!(rest.get("type").is_none());
        assert_eq!(e.rest_body(Version::V03)["type"], "TaskNotFoundError");
    }
}
