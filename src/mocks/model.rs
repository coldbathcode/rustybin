//! Mock settings: the same structure in the control API, `rustybin.yaml`
//! and exports. Every field has a default, so `{}` is a valid configuration.

use axum::http::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Most rules on one operation.
pub const MAX_RULES: usize = 20;
/// Most configured headers on one response.
pub const MAX_HEADERS: usize = 20;
/// Longest `matches` regular expression.
pub const MAX_PATTERN_LEN: usize = 256;

/// Where response bodies come from by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseMode {
    /// Spec examples, falling back to generated data.
    #[default]
    Examples,
    /// Always generated from the response schema.
    Generated,
    /// Only configured bodies; otherwise an empty body.
    Static,
    /// CRUD against an in-memory store (see [`super::state`]); operations
    /// that are not CRUD-shaped answer as in `examples`.
    Stateful,
}

/// What happens to requests that do not match the spec.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMode {
    Off,
    /// Respond normally, flag the problems (header + capture).
    #[default]
    Warn,
    /// Answer 400 with the problems.
    Enforce,
}

impl ValidationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Enforce => "enforce",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResponseSettings {
    pub mode: ResponseMode,
    /// Seed of the generated data (same seed, same data).
    pub seed: u64,
    pub delay_ms: u64,
    pub validate_requests: ValidationMode,
    /// Status answered while the mock is switched off (503 or 404).
    pub disabled_status: u16,
}

impl Default for ResponseSettings {
    fn default() -> Self {
        Self {
            mode: ResponseMode::Examples,
            seed: 1,
            delay_ms: 0,
            validate_requests: ValidationMode::Warn,
            disabled_status: 503,
        }
    }
}

/// How mocked security decides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityMode {
    /// The spec's `security` requirements and `securitySchemes`.
    #[default]
    Spec,
    /// One API key header on every operation.
    ApiKey,
    Basic,
    /// Static tokens or a JWT from the built-in IdP.
    Bearer,
    /// A JWT from the built-in IdP only.
    Jwt,
}

impl SecurityMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spec => "spec",
            Self::ApiKey => "api_key",
            Self::Basic => "basic",
            Self::Bearer => "bearer",
            Self::Jwt => "jwt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiKeySettings {
    /// Header used by the `api_key` mode (spec mode uses the scheme's own name).
    pub header: String,
    pub keys: Vec<String>,
}

impl Default for ApiKeySettings {
    fn default() -> Self {
        Self {
            header: "x-api-key".into(),
            keys: vec!["demo-key".into()],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BasicSettings {
    pub users: BTreeMap<String, String>,
}

impl Default for BasicSettings {
    fn default() -> Self {
        Self {
            users: BTreeMap::from([("demo".to_string(), "demo".to_string())]),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BearerSettings {
    /// Opaque tokens accepted as is (they carry every scope).
    pub tokens: Vec<String>,
}

impl Default for BearerSettings {
    fn default() -> Self {
        Self {
            tokens: vec!["demo-token".into()],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JwtSettings {
    /// Required `iss` claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// Required `aud` claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecuritySettings {
    pub enabled: bool,
    pub mode: SecurityMode,
    pub api_key: ApiKeySettings,
    pub basic: BasicSettings,
    pub bearer: BearerSettings,
    pub jwt: JwtSettings,
}

/// A response chosen by an override or a rule. Without `body` or `example`
/// the body comes from the mode for the chosen status.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResponseOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Name of a spec example of the chosen status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    /// Literal body: JSON values are serialised, strings sent as is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// One condition on a request value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Condition {
    /// Compared as text (`0` equals `"0"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_equals: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// Regular expression on the text value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

/// `when` keys: `path.<name>`, `query.<name>`, `header.<name>` or
/// `body.<dotted.path>` (`body` alone is the whole body). All must hold.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub when: BTreeMap<String, Condition>,
    pub respond: ResponseOverride,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OperationSettings {
    /// Default response of this operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub respond: Option<ResponseOverride>,
    /// Evaluated in order before `respond`; the first match wins.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ResponseMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    /// No mocked security on this operation.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub public: bool,
}

impl OperationSettings {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MockSettings {
    /// Serve the spec paths without the `servers[0]` path prefix.
    pub strip_server_prefix: bool,
    pub responses: ResponseSettings,
    pub security: SecuritySettings,
    /// Per-operation settings keyed by operationId.
    pub operations: BTreeMap<String, OperationSettings>,
}

/// Validation limits that depend on the instance.
#[derive(Clone, Copy, Debug)]
pub struct SettingsLimits {
    pub max_delay_ms: u64,
    pub max_body_bytes: usize,
    pub max_rules: usize,
}

/// Mock names: `[a-z0-9-]`, 1 to 40 characters, no leading or trailing dash.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 40
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
}

fn check_response(
    r: &ResponseOverride,
    where_: &str,
    limits: &SettingsLimits,
) -> Result<(), String> {
    if let Some(s) = r.status {
        if !(100..=599).contains(&s) {
            return Err(format!("{where_}: status {s} is not between 100 and 599"));
        }
    }
    if r.headers.len() > MAX_HEADERS {
        return Err(format!("{where_}: at most {MAX_HEADERS} headers"));
    }
    for (name, value) in &r.headers {
        let parsed = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("{where_}: invalid header name {name:?}"))?;
        if HeaderValue::from_str(value).is_err() {
            return Err(format!("{where_}: invalid value for header {name:?}"));
        }
        if matches!(
            parsed.as_str(),
            "content-length" | "transfer-encoding" | "connection" | "upgrade" | "trailer"
        ) {
            return Err(format!("{where_}: header {name:?} cannot be configured"));
        }
    }
    if let Some(body) = &r.body {
        let size = match body {
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        };
        if size > limits.max_body_bytes {
            return Err(format!(
                "{where_}: body is {size} bytes, the limit is {}",
                limits.max_body_bytes
            ));
        }
    }
    Ok(())
}

fn check_condition_key(key: &str) -> bool {
    key == "body"
        || ["path.", "query.", "header.", "body."]
            .iter()
            .any(|p| key.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
}

impl MockSettings {
    /// Reject settings that cannot work (bad headers, statuses, patterns,
    /// limits). Mismatches with the spec are deploy-check findings instead.
    pub fn validate(&self, limits: &SettingsLimits) -> Result<(), String> {
        let r = &self.responses;
        if r.delay_ms > limits.max_delay_ms {
            return Err(format!(
                "responses.delay_ms must be at most {}",
                limits.max_delay_ms
            ));
        }
        if !(400..=599).contains(&r.disabled_status) {
            return Err("responses.disabled_status must be a 4xx or 5xx status".into());
        }
        let s = &self.security;
        if HeaderName::from_bytes(s.api_key.header.as_bytes()).is_err() {
            return Err(format!(
                "security.api_key.header {:?} is not a valid header name",
                s.api_key.header
            ));
        }
        for (what, list) in [
            ("api_key.keys", &s.api_key.keys),
            ("bearer.tokens", &s.bearer.tokens),
        ] {
            if list.len() > 50 {
                return Err(format!("security.{what}: at most 50 entries"));
            }
            if list.iter().any(|k| k.is_empty() || k.len() > 512) {
                return Err(format!(
                    "security.{what}: entries must be 1 to 512 characters"
                ));
            }
        }
        if s.basic.users.len() > 50 {
            return Err("security.basic.users: at most 50 users".into());
        }
        if self.operations.len() > 1000 {
            return Err("at most 1000 operation entries".into());
        }
        for (op, cfg) in &self.operations {
            if let Some(d) = cfg.delay_ms {
                if d > limits.max_delay_ms {
                    return Err(format!(
                        "operations.{op}.delay_ms must be at most {}",
                        limits.max_delay_ms
                    ));
                }
            }
            if let Some(resp) = &cfg.respond {
                check_response(resp, &format!("operations.{op}.respond"), limits)?;
            }
            if cfg.rules.len() > limits.max_rules {
                return Err(format!(
                    "operations.{op}: at most {} rules",
                    limits.max_rules
                ));
            }
            for (i, rule) in cfg.rules.iter().enumerate() {
                let at = format!("operations.{op}.rules[{i}]");
                if rule.when.is_empty() {
                    return Err(format!("{at}: \"when\" needs at least one condition"));
                }
                for (key, cond) in &rule.when {
                    if !check_condition_key(key) {
                        return Err(format!(
                            "{at}: condition key {key:?} must start with path., query., header. or body."
                        ));
                    }
                    if let Some(p) = &cond.matches {
                        if p.len() > MAX_PATTERN_LEN {
                            return Err(format!("{at}: pattern longer than {MAX_PATTERN_LEN}"));
                        }
                        regex::RegexBuilder::new(p)
                            .size_limit(1 << 20)
                            .build()
                            .map_err(|e| format!("{at}: invalid pattern {p:?}: {e}"))?;
                    }
                }
                check_response(&rule.respond, &format!("{at}.respond"), limits)?;
            }
        }
        Ok(())
    }

    /// Drop empty operation entries (keeps exports tidy).
    pub fn normalised(mut self) -> Self {
        self.operations.retain(|_, v| !v.is_empty());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LIMITS: SettingsLimits = SettingsLimits {
        max_delay_ms: 10_000,
        max_body_bytes: 1024,
        max_rules: 2,
    };

    #[test]
    fn defaults_round_trip() {
        let s: MockSettings = serde_json::from_value(json!({})).expect("defaults");
        assert_eq!(s.responses.disabled_status, 503);
        assert_eq!(s.security.api_key.keys, vec!["demo-key"]);
        assert!(s.validate(&LIMITS).is_ok());
        let again: MockSettings =
            serde_json::from_value(serde_json::to_value(&s).expect("ser")).expect("de");
        assert_eq!(s, again);
    }

    #[test]
    fn invalid_settings_are_rejected() {
        for bad in [
            json!({"responses": {"delay_ms": 999_999}}),
            json!({"responses": {"disabled_status": 200}}),
            json!({"security": {"api_key": {"header": "bad header"}}}),
            json!({"operations": {"x": {"respond": {"status": 99}}}}),
            json!({"operations": {"x": {"respond": {"headers": {"content-length": "1"}}}}}),
            json!({"operations": {"x": {"respond": {"body": "y".repeat(2000)}}}}),
            json!({"operations": {"x": {"rules": [{"when": {}, "respond": {}}]}}}),
            json!({"operations": {"x": {"rules": [{"when": {"cookie.a": {}}, "respond": {}}]}}}),
            json!({"operations": {"x": {"rules": [{"when": {"path.a": {"matches": "("}}, "respond": {}}]}}}),
            json!({"operations": {"x": {"rules": [
                {"when": {"path.a": {}}, "respond": {}},
                {"when": {"path.a": {}}, "respond": {}},
                {"when": {"path.a": {}}, "respond": {}}
            ]}}}),
        ] {
            let s: MockSettings = serde_json::from_value(bad.clone()).expect("shape");
            assert!(s.validate(&LIMITS).is_err(), "{bad}");
        }
        assert!(serde_json::from_value::<MockSettings>(json!({"nope": 1})).is_err());
    }

    #[test]
    fn names() {
        for ok in ["petstore", "a", "pet-store-2"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "Pet", "a_b", "a/b", &"a".repeat(41)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
