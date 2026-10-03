//! Canonical (version independent) A2A data model.
//!
//! Every binding parses its wire format into these types and serialises them
//! back with [`super::wire`]. Field names follow the v1.0 proto
//! (`lf.a2a.v1`); the v0.3 differences are handled in the wire layer.

use serde_json::{Map, Value};

/// Optional free-form metadata object.
pub type Meta = Option<Map<String, Value>>;

/// A2A protocol version (Major.Minor) negotiated with `A2A-Version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V03,
    V10,
}

impl Version {
    pub fn as_str(self) -> &'static str {
        match self {
            Version::V03 => "0.3",
            Version::V10 => "1.0",
        }
    }

    /// Parse an `A2A-Version` value. Empty means 0.3 (spec section 3.6.2);
    /// patch versions are ignored. `None` means unsupported.
    pub fn parse(raw: &str) -> Option<Version> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Some(Version::V03);
        }
        let mut it = raw.split('.');
        let major = it.next()?.trim().parse::<u32>().ok()?;
        let minor = match it.next() {
            Some(m) => m.trim().parse::<u32>().ok()?,
            None => 0,
        };
        if let Some(patch) = it.next() {
            patch.trim().parse::<u32>().ok()?;
        }
        if it.next().is_some() {
            return None;
        }
        match (major, minor) {
            (0, 3) => Some(Version::V03),
            (1, 0) => Some(Version::V10),
            _ => None,
        }
    }
}

/// The three JSON shapes this module speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// v1.0 ProtoJSON (JSON-RPC and HTTP+JSON bindings).
    V1,
    /// v0.3 JSON-RPC (`kind` discriminators, lowercase enums).
    V03Rpc,
    /// v0.3 HTTP+JSON (ProtoJSON of the v0.3 proto: `content`, `file`, ...).
    V03Rest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskState {
    Submitted,
    Working,
    Completed,
    Failed,
    Canceled,
    InputRequired,
    Rejected,
    AuthRequired,
}

impl TaskState {
    pub const ALL: [TaskState; 8] = [
        TaskState::Submitted,
        TaskState::Working,
        TaskState::Completed,
        TaskState::Failed,
        TaskState::Canceled,
        TaskState::InputRequired,
        TaskState::Rejected,
        TaskState::AuthRequired,
    ];

    /// v1.0 enum name.
    pub fn v1(self) -> &'static str {
        match self {
            TaskState::Submitted => "TASK_STATE_SUBMITTED",
            TaskState::Working => "TASK_STATE_WORKING",
            TaskState::Completed => "TASK_STATE_COMPLETED",
            TaskState::Failed => "TASK_STATE_FAILED",
            TaskState::Canceled => "TASK_STATE_CANCELED",
            TaskState::InputRequired => "TASK_STATE_INPUT_REQUIRED",
            TaskState::Rejected => "TASK_STATE_REJECTED",
            TaskState::AuthRequired => "TASK_STATE_AUTH_REQUIRED",
        }
    }

    /// v0.3 JSON-RPC value (kebab case).
    pub fn v03(self) -> &'static str {
        match self {
            TaskState::Submitted => "submitted",
            TaskState::Working => "working",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Canceled => "canceled",
            TaskState::InputRequired => "input-required",
            TaskState::Rejected => "rejected",
            TaskState::AuthRequired => "auth-required",
        }
    }

    /// v0.3 proto enum name (note the British `CANCELLED`).
    pub fn v03_proto(self) -> &'static str {
        match self {
            TaskState::Canceled => "TASK_STATE_CANCELLED",
            other => other.v1(),
        }
    }

    pub fn wire(self, w: Wire) -> &'static str {
        match w {
            Wire::V1 => self.v1(),
            Wire::V03Rpc => self.v03(),
            Wire::V03Rest => self.v03_proto(),
        }
    }

    /// Accepts every spelling used by either version.
    pub fn parse(s: &str) -> Option<TaskState> {
        let up = s.trim().to_ascii_uppercase().replace('-', "_");
        let bare = up.strip_prefix("TASK_STATE_").unwrap_or(&up);
        Some(match bare {
            "SUBMITTED" => TaskState::Submitted,
            "WORKING" => TaskState::Working,
            "COMPLETED" => TaskState::Completed,
            "FAILED" => TaskState::Failed,
            "CANCELED" | "CANCELLED" => TaskState::Canceled,
            "INPUT_REQUIRED" => TaskState::InputRequired,
            "REJECTED" => TaskState::Rejected,
            "AUTH_REQUIRED" => TaskState::AuthRequired,
            _ => return None,
        })
    }

    /// COMPLETED, FAILED, CANCELED, REJECTED.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
        )
    }

    /// INPUT_REQUIRED, AUTH_REQUIRED.
    pub fn is_interrupted(self) -> bool {
        matches!(self, TaskState::InputRequired | TaskState::AuthRequired)
    }

    /// Streams and blocking sends stop at terminal or interrupted states.
    pub fn ends_stream(self) -> bool {
        self.is_terminal() || self.is_interrupted()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
}

impl Role {
    pub fn wire(self, w: Wire) -> &'static str {
        match (self, w) {
            (Role::User, Wire::V03Rpc) => "user",
            (Role::Agent, Wire::V03Rpc) => "agent",
            (Role::User, _) => "ROLE_USER",
            (Role::Agent, _) => "ROLE_AGENT",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        let up = s.trim().to_ascii_uppercase();
        match up.strip_prefix("ROLE_").unwrap_or(&up) {
            "USER" => Some(Role::User),
            "AGENT" => Some(Role::Agent),
            _ => None,
        }
    }
}

/// The `oneof content` of a part.
#[derive(Clone, Debug, PartialEq)]
pub enum PartContent {
    Text(String),
    /// Inline bytes, kept as standard base64.
    Raw(String),
    Url(String),
    Data(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub content: PartContent,
    pub metadata: Meta,
    pub filename: Option<String>,
    pub media_type: Option<String>,
}

impl Part {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: PartContent::Text(s.into()),
            metadata: None,
            filename: None,
            media_type: None,
        }
    }

    pub fn data(v: Value) -> Self {
        Self {
            content: PartContent::Data(v),
            metadata: None,
            filename: None,
            media_type: Some("application/json".to_string()),
        }
    }

    pub fn url(url: impl Into<String>, filename: &str, media_type: &str) -> Self {
        Self {
            content: PartContent::Url(url.into()),
            metadata: None,
            filename: Some(filename.to_string()),
            media_type: Some(media_type.to_string()),
        }
    }

    pub fn raw(bytes: &[u8], filename: &str, media_type: &str) -> Self {
        use base64::Engine;
        Self {
            content: PartContent::Raw(base64::engine::general_purpose::STANDARD.encode(bytes)),
            metadata: None,
            filename: Some(filename.to_string()),
            media_type: Some(media_type.to_string()),
        }
    }

    /// Media type used for content negotiation checks.
    pub fn effective_media_type(&self) -> String {
        if let Some(m) = &self.media_type {
            return m.to_ascii_lowercase();
        }
        match self.content {
            PartContent::Text(_) => "text/plain".to_string(),
            PartContent::Data(_) => "application/json".to_string(),
            PartContent::Raw(_) | PartContent::Url(_) => "application/octet-stream".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub message_id: String,
    pub context_id: Option<String>,
    pub task_id: Option<String>,
    pub role: Role,
    pub parts: Vec<Part>,
    pub metadata: Meta,
    pub extensions: Vec<String>,
    pub reference_task_ids: Vec<String>,
}

impl Message {
    /// A new agent message.
    pub fn agent(parts: Vec<Part>, context_id: &str, task_id: Option<&str>) -> Self {
        Self {
            message_id: new_id(),
            context_id: Some(context_id.to_string()),
            task_id: task_id.map(str::to_string),
            role: Role::Agent,
            parts,
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        }
    }

    /// Concatenated text of all text parts.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let PartContent::Text(t) = &p.content {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(t);
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Artifact {
    pub artifact_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub parts: Vec<Part>,
    pub metadata: Meta,
    pub extensions: Vec<String>,
}

impl Artifact {
    pub fn new(id: &str, name: &str, description: &str, parts: Vec<Part>) -> Self {
        Self {
            artifact_id: id.to_string(),
            name: Some(name.to_string()),
            description: if description.is_empty() {
                None
            } else {
                Some(description.to_string())
            },
            parts,
            metadata: None,
            extensions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskStatus {
    pub state: TaskState,
    pub message: Option<Message>,
    pub timestamp: String,
}

impl TaskStatus {
    pub fn new(state: TaskState, message: Option<Message>) -> Self {
        Self {
            state,
            message,
            timestamp: now_ts(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub context_id: String,
    pub status: TaskStatus,
    pub artifacts: Vec<Artifact>,
    pub history: Vec<Message>,
    pub metadata: Meta,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusUpdate {
    pub task_id: String,
    pub context_id: String,
    pub status: TaskStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactUpdate {
    pub task_id: String,
    pub context_id: String,
    pub artifact: Artifact,
    pub append: bool,
    pub last_chunk: bool,
}

/// One `StreamResponse` payload.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Task(Task),
    Message(Message),
    Status(StatusUpdate),
    Artifact(ArtifactUpdate),
}

impl StreamEvent {
    /// Whether a stream carrying this event must close after it.
    pub fn ends_stream(&self) -> bool {
        match self {
            StreamEvent::Message(_) => true,
            StreamEvent::Task(t) => t.status.state.ends_stream(),
            StreamEvent::Status(s) => s.status.state.ends_stream(),
            StreamEvent::Artifact(_) => false,
        }
    }
}

/// Push notification authentication (`AuthenticationInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct PushAuth {
    pub scheme: String,
    pub credentials: Option<String>,
}

/// A stored push notification configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct PushConfig {
    pub id: String,
    pub task_id: String,
    pub url: String,
    pub token: Option<String>,
    pub authentication: Option<PushAuth>,
    /// Version the config was created with (selects the payload format).
    pub version: Version,
    /// Set when the URL points at this server's own webhook sink: delivery
    /// then happens in-process instead of over the network.
    pub sink_id: Option<String>,
}

/// A fresh random identifier.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// ISO 8601 UTC timestamp with millisecond precision.
pub fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(Version::parse(""), Some(Version::V03));
        assert_eq!(Version::parse("0.3"), Some(Version::V03));
        assert_eq!(Version::parse("0.3.0"), Some(Version::V03));
        assert_eq!(Version::parse("1.0"), Some(Version::V10));
        assert_eq!(Version::parse("1"), Some(Version::V10));
        assert_eq!(Version::parse("1.0.2"), Some(Version::V10));
        assert_eq!(Version::parse("0.5"), None);
        assert_eq!(Version::parse("2.0"), None);
        assert_eq!(Version::parse("abc"), None);
        assert_eq!(Version::parse("1.0.0.0"), None);
    }

    #[test]
    fn task_state_spellings() {
        for s in TaskState::ALL {
            assert_eq!(TaskState::parse(s.v1()), Some(s));
            assert_eq!(TaskState::parse(s.v03()), Some(s));
            assert_eq!(TaskState::parse(s.v03_proto()), Some(s));
        }
        assert_eq!(TaskState::parse("TASK_STATE_RUNNING"), None);
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::AuthRequired.is_interrupted());
        assert!(!TaskState::Working.ends_stream());
    }

    #[test]
    fn roles() {
        assert_eq!(Role::parse("ROLE_USER"), Some(Role::User));
        assert_eq!(Role::parse("user"), Some(Role::User));
        assert_eq!(Role::parse("agent"), Some(Role::Agent));
        assert_eq!(Role::parse("bot"), None);
        assert_eq!(Role::User.wire(Wire::V03Rpc), "user");
        assert_eq!(Role::Agent.wire(Wire::V1), "ROLE_AGENT");
    }
}
