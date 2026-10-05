//! Admin guard for global-state mutations.
//!
//! Rules:
//! - `RUSTYBIN_ADMIN_TOKEN` set: the request must carry
//!   `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>`.
//! - Token unset, normal mode: open (backwards compatible).
//! - Token unset, public mode: global mutations are disabled (403).
//!
//! Usage inside a handler:
//!
//! ```ignore
//! if let Err(resp) = crate::admin::require_admin(&headers, &config) {
//!     return resp;
//! }
//! ```

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::config::Config;
use crate::control_auth::{constant_time_eq, presented_token};

/// Header accepted as an alternative to `Authorization: Bearer`.
pub const ADMIN_TOKEN_HEADER: &str = "x-rustybin-admin-token";

/// Returns `Ok(())` when the caller may perform a global mutation, otherwise
/// the JSON error response to send back (401 or 403).
///
/// With `RUSTYBIN_CONTROL_AUTH=jwt`, a control-plane JWT with the `admin`
/// scope is accepted as well.
#[allow(clippy::result_large_err)]
pub fn require_admin(headers: &HeaderMap, config: &Config) -> Result<(), Response> {
    let token = presented_token(headers);
    if let Some(t) = token {
        if crate::control_auth::verify_jwt(t, config).is_ok_and(|g| g.admin) {
            return Ok(());
        }
    }
    match config.admin_token.as_deref() {
        Some(expected) => {
            if token.is_some_and(|t| constant_time_eq(t, expected)) {
                Ok(())
            } else {
                Err(error(
                    StatusCode::UNAUTHORIZED,
                    "admin token required: send Authorization: Bearer <token> or X-Rustybin-Admin-Token",
                ))
            }
        }
        None if config.public_mode => Err(error(
            StatusCode::FORBIDDEN,
            "this operation is disabled in public mode (no admin token configured)",
        )),
        None => Ok(()),
    }
}

/// True when the request carries a valid admin token (or none is required).
pub fn is_admin(headers: &HeaderMap, config: &Config) -> bool {
    require_admin(headers, config).is_ok()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn cfg(token: Option<&str>, public: bool) -> Config {
        let mut c = Config::for_tests();
        c.admin_token = token.map(str::to_string);
        c.public_mode = public;
        c
    }

    #[test]
    fn open_without_token() {
        assert!(require_admin(&HeaderMap::new(), &cfg(None, false)).is_ok());
    }

    #[test]
    fn public_mode_without_token_is_forbidden() {
        let err = require_admin(&HeaderMap::new(), &cfg(None, true)).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn token_via_bearer_or_header() {
        let c = cfg(Some("s3cret"), true);
        let mut h = HeaderMap::new();
        assert_eq!(
            require_admin(&h, &c).unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
        h.insert("authorization", HeaderValue::from_static("Bearer s3cret"));
        assert!(require_admin(&h, &c).is_ok());
        let mut h = HeaderMap::new();
        h.insert(ADMIN_TOKEN_HEADER, HeaderValue::from_static("s3cret"));
        assert!(require_admin(&h, &c).is_ok());
        h.insert(ADMIN_TOKEN_HEADER, HeaderValue::from_static("wrong"));
        assert!(require_admin(&h, &c).is_err());
    }
}
