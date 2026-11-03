//! Permission checks outside the control plane's route check.
//!
//! Control routes (`/_rustybin/*`) declare their permission in the
//! catalogue and `control_auth::check_route` checks it before the handler
//! runs. This module is for the rest:
//! - [`require`]: a data-plane route that changes something instance-wide
//!   (the `/health` toggle, the global `/flaky` reset) names the permission
//!   it needs; the caller is the admin token or a control JWT as it
//!   resolves, else nobody, who holds every permission on an instance
//!   without an admin token outside public mode (backwards compatible) and
//!   the anonymous reads otherwise;
//! - [`admin_check`]: every permission (the admin role), for shared mode's
//!   gate between the instance's operator and an API key.
//!
//! ```ignore
//! if let Err(resp) = crate::admin::require(&headers, &config, Permission::SettingsWrite) {
//!     return resp;
//! }
//! ```

use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use crate::config::Config;
use crate::rbac::{Permission, Principal};

/// Header accepted as an alternative to `Authorization: Bearer`.
pub const ADMIN_TOKEN_HEADER: &str = "x-rustybin-admin-token";

/// `Ok(())` when the request's caller holds `p`, otherwise the refusal
/// (`401` without a credential when an admin token is configured, `403`
/// otherwise, naming the permission).
#[allow(clippy::result_large_err)]
pub fn require(headers: &HeaderMap, config: &Config, p: Permission) -> Result<(), Response> {
    crate::rbac::require(&crate::control_auth::open_caller(headers, config), p)
}

/// Does the request's caller hold every permission (the admin token, a
/// control JWT with the admin role, or nobody on an instance without an
/// admin token outside public mode)? The refusal as data otherwise.
pub fn admin_check(headers: &HeaderMap, config: &Config) -> Result<(), (StatusCode, &'static str)> {
    let caller = crate::control_auth::open_caller(headers, config);
    if caller.permissions.is_all() {
        return Ok(());
    }
    Err(match caller.principal {
        Principal::Anonymous { admin_token: true } => (
            StatusCode::UNAUTHORIZED,
            "admin token required: send Authorization: Bearer <token> or X-Rustybin-Admin-Token",
        ),
        Principal::Anonymous { admin_token: false } => (
            StatusCode::FORBIDDEN,
            "this operation is disabled in public mode (no admin token configured)",
        ),
        // Signed in with a control token that lacks permissions:
        // authenticated, not allowed (a 401 would sign the console out).
        _ => (
            StatusCode::FORBIDDEN,
            "this operation needs the admin token or a control token with the admin role",
        ),
    })
}

/// True when the request's caller holds every permission.
pub fn is_admin(headers: &HeaderMap, config: &Config) -> bool {
    admin_check(headers, config).is_ok()
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

    const P: Permission = Permission::SettingsWrite;

    #[test]
    fn open_without_token() {
        assert!(require(&HeaderMap::new(), &cfg(None, false), P).is_ok());
    }

    #[test]
    fn public_mode_without_token_is_forbidden() {
        let err = require(&HeaderMap::new(), &cfg(None, true), P).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn token_via_bearer_or_header() {
        let c = cfg(Some("s3cret"), true);
        let mut h = HeaderMap::new();
        assert_eq!(
            require(&h, &c, P).unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
        h.insert("authorization", HeaderValue::from_static("Bearer s3cret"));
        assert!(require(&h, &c, P).is_ok());
        let mut h = HeaderMap::new();
        h.insert(ADMIN_TOKEN_HEADER, HeaderValue::from_static("s3cret"));
        assert!(require(&h, &c, P).is_ok());
        h.insert(ADMIN_TOKEN_HEADER, HeaderValue::from_static("wrong"));
        assert!(require(&h, &c, P).is_err());
    }

    #[test]
    fn a_control_jwt_holds_what_its_roles_grant_without_an_admin_token() {
        let mut c = crate::test_support::jwt_config();
        c.admin_token = None;
        let mut h = HeaderMap::new();
        let console = crate::test_support::control_jwt(Some("console"));
        h.insert(
            "authorization",
            format!("Bearer {console}").parse().expect("header"),
        );
        let e = admin_check(&h, &c).expect_err("not admin");
        assert_eq!(e.0, StatusCode::FORBIDDEN);
        assert_eq!(
            require(&h, &c, P).unwrap_err().status(),
            StatusCode::FORBIDDEN
        );
        assert!(require(&h, &c, Permission::SettingsRead).is_ok());
        let admin = crate::test_support::control_jwt(Some("admin"));
        h.insert(
            "authorization",
            format!("Bearer {admin}").parse().expect("header"),
        );
        assert!(admin_check(&h, &c).is_ok());
        // No credential, no admin token, normal mode: open, as before.
        assert!(admin_check(&HeaderMap::new(), &c).is_ok());
    }
}
