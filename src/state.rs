//! The router state shared by every module.
//!
//! `AppState` is cheap to clone (everything is behind `Arc`). Handlers can
//! extract the whole state (`State<AppState>`) or any single part through
//! [`FromRef`], e.g. `State<Arc<Config>>`, `State<Inspector>`.
//!
//! Module-local state should NOT be added here: create it inside the module's
//! `router(state: &AppState)` function and attach it with `Extension`.
//! Only add a field when several modules (or the UI) must share it.

use axum::extract::FromRef;
use std::sync::Arc;

use crate::cert_state::CertState;
use crate::config::Config;
use crate::health::HealthState;
use crate::identity::IdentityState;
use crate::inspector::Inspector;
use crate::jwt_state::JwtState;
use crate::limits::Limiter;

#[derive(Clone)]
pub struct AppState {
    /// Effective configuration.
    pub config: Arc<Config>,
    /// HS256 secret and RS256 key pair (JWT + OIDC modules).
    pub jwt: Arc<JwtState>,
    /// Demo PKI (mTLS module, HTTPS listener).
    pub certs: Arc<CertState>,
    /// Start time and request counter for `/identity`.
    pub identity: Arc<IdentityState>,
    /// Captured request ring buffer + live feed.
    pub inspector: Inspector,
    /// `/health` toggle, shared with the gRPC health service.
    pub health: Arc<HealthState>,
    /// Plan limits (HTTP middleware, gRPC interceptor, `/_rustybin/usage`).
    pub limits: Arc<Limiter>,
}

impl AppState {
    /// Build the production state: generates fresh JWT keys and demo PKI.
    pub fn new(config: Config) -> Self {
        let jwt = Arc::new(JwtState::generate());
        let certs = Arc::new(CertState::generate());
        Self::from_parts(config, jwt, certs)
    }

    /// Build a state for tests: reuses process-wide keys (no per-test RSA
    /// key generation) and never touches the filesystem.
    #[doc(hidden)]
    pub fn for_tests(config: Config) -> Self {
        Self::from_parts(
            config,
            JwtState::shared_for_tests(),
            CertState::shared_for_tests(),
        )
    }

    /// Assemble a state from already-built key material.
    pub fn from_parts(config: Config, jwt: Arc<JwtState>, certs: Arc<CertState>) -> Self {
        let inspector = Inspector::new(config.inspector_capacity, config.public_mode);
        let limits = Arc::new(Limiter::new(config.limits.clone()));
        Self {
            limits,
            config: Arc::new(config),
            jwt,
            certs,
            identity: Arc::new(IdentityState::new()),
            inspector,
            health: Arc::new(HealthState::new()),
        }
    }
}

impl FromRef<AppState> for Arc<Config> {
    fn from_ref(state: &AppState) -> Self {
        state.config.clone()
    }
}

impl FromRef<AppState> for Arc<JwtState> {
    fn from_ref(state: &AppState) -> Self {
        state.jwt.clone()
    }
}

impl FromRef<AppState> for Arc<CertState> {
    fn from_ref(state: &AppState) -> Self {
        state.certs.clone()
    }
}

impl FromRef<AppState> for Arc<IdentityState> {
    fn from_ref(state: &AppState) -> Self {
        state.identity.clone()
    }
}

impl FromRef<AppState> for Arc<Limiter> {
    fn from_ref(state: &AppState) -> Self {
        state.limits.clone()
    }
}

impl FromRef<AppState> for Inspector {
    fn from_ref(state: &AppState) -> Self {
        state.inspector.clone()
    }
}
