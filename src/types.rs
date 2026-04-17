use serde::{Deserialize, Serialize};

/// Standard error response used across all endpoints.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Shared auth response used by all auth modules.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthResponse {
    pub authenticated: bool,
    pub auth_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claims: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_header: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_dn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ca: Option<String>,
}

/// Auth failure response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthFailure {
    pub authenticated: bool,
    pub error: String,
}
