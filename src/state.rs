//! The router state shared by every module.
//!
//! `AppState` is cheap to clone (everything is behind `Arc`). Handlers can
//! extract the whole state (`State<AppState>`) or any single part through
//! [`FromRef`], e.g. `State<Arc<Config>>`, `State<Inspector>`, `State<Arc<Metrics>>`.
//!
//! Module-local state should NOT be added here: create it inside the module's
//! `router(state: &AppState)` function and attach it with `Extension`.
//! Only add a field when several modules (or the UI) must share it.

use axum::extract::FromRef;
use std::sync::{Arc, OnceLock};

use crate::assist::Assist;
use crate::cert_state::CertState;
use crate::config::Config;
use crate::egress::Egress;
use crate::health::HealthState;
use crate::identity::IdentityState;
use crate::inspector::Inspector;
use crate::jwt_state::JwtState;
use crate::keys::Keys;
use crate::limits::{Clock, Limiter, SystemClock};
use crate::metrics::Metrics;
use crate::mocks::store::MockStore;
use crate::settings::Settings;

#[derive(Clone)]
pub struct AppState {
    /// Effective configuration.
    pub config: Arc<Config>,
    /// The instance settings store (`/_rustybin/settings`): persisted
    /// values and sealed secrets, env-seeded, read by the console, the
    /// egress policy and every feature with a registered key.
    pub settings: Arc<Settings>,
    /// The outbound connection policy every feature that reaches out
    /// resolves through (`/_rustybin/egress`).
    pub egress: Arc<Egress>,
    /// AI assist (`/_rustybin/assist*`): the user's provider and key, the
    /// tasks, usage and caps.
    pub assist: Arc<Assist>,
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
    /// Prometheus counters (`/_rustybin/metrics`), fed by the HTTP middleware,
    /// the gRPC services and the mock LLM.
    pub metrics: Arc<Metrics>,
    /// API keys (shared mode, `RUSTYBIN_REQUIRE_KEY`): the registry and the
    /// per-key daily counters, used by the key layer, the limiter, the
    /// inspector and the gRPC interceptor.
    pub keys: Arc<Keys>,
    /// OpenAPI mocks (`/mock/*`, `/_rustybin/mocks*`, startup loading and
    /// SIGHUP reloads in the server).
    pub mocks: MockStore,
    /// The finished application (set by `build_app`): the management MCP
    /// server's `try_request` sends requests through it in process.
    pub router: Arc<OnceLock<axum::Router>>,
    /// Harness mode (`RUSTYBIN_HARNESS`): the node's engine, runs and kill
    /// switch, shared by the control API, the MCP tools, headless mode and
    /// (phase 2) the correlation layer. `None` when harness mode is off.
    pub harness: Option<Arc<crate::harness::Harness>>,
    /// The dev portal (`/portal`, `/_rustybin/portal*`): published items,
    /// insights and the catalogue cache.
    pub portal: Arc<crate::portal::Portal>,
    /// AI agents on the management MCP server (`/_rustybin/agents*`): who
    /// called, what they did, their rights and revocations.
    pub agents: Arc<crate::mcp::manage::agents::Agents>,
    /// The mock MCP registry's published entries and tokens: shared by its
    /// routes and the configuration document (step 09 exports and applies
    /// the published entries).
    pub registry: Arc<crate::mcp::registry::Registry>,
    /// Source control (step 09): the configuration repository's working
    /// copy, operations and timeline; shared by `/_rustybin/git*`, startup
    /// from `RUSTYBIN_CONFIG_GIT` and the automatic push and pull task.
    pub git: Arc<crate::sourcecontrol::SourceControl>,
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

    /// A test state on a given clock (limits and daily key quotas).
    #[doc(hidden)]
    pub fn for_tests_with_clock(config: Config, clock: Arc<dyn Clock>) -> Self {
        Self::with_clock(
            config,
            JwtState::shared_for_tests(),
            CertState::shared_for_tests(),
            clock,
        )
    }

    /// Assemble a state from already-built key material.
    pub fn from_parts(config: Config, jwt: Arc<JwtState>, certs: Arc<CertState>) -> Self {
        Self::with_clock(config, jwt, certs, Arc::new(SystemClock::new()))
    }

    /// [`AppState::from_parts`] on a given clock.
    pub fn with_clock(
        config: Config,
        jwt: Arc<JwtState>,
        certs: Arc<CertState>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let inspector = Inspector::with_keys(
            config.inspector_capacity,
            config.public_mode,
            config.shared_mode,
        );
        let metrics = Arc::new(Metrics::new());
        let settings = Arc::new(Settings::new(&config));
        let egress = Arc::new(Egress::new(&config, settings.clone()));
        let assist = Arc::new(Assist::new(
            &config,
            settings.clone(),
            egress.clone(),
            metrics.clone(),
            clock.clone(),
        ));
        let limits = Arc::new(
            Limiter::with_clock(config.limits.clone(), clock.clone()).with_metrics(metrics.clone()),
        );
        let keys = Arc::new(Keys::new(
            &config,
            metrics.clone(),
            clock.clone(),
            egress.clone(),
        ));
        let mocks =
            crate::mocks::new_store_with(&config, clock.clone(), metrics.clone(), egress.clone());
        let health = Arc::new(HealthState::new());
        let portal = Arc::new(crate::portal::Portal::new(&settings));
        let agents = Arc::new(crate::mcp::manage::agents::Agents::new(
            &settings,
            &config.console_tokens,
            clock.clone(),
        ));
        let harness = crate::harness::Harness::new(&config, Some(metrics.clone()));
        if let Some(h) = &harness {
            h.attach(crate::harness::Resources {
                mocks: mocks.clone(),
                jwt: jwt.clone(),
                health: health.clone(),
            });
        }
        let git = Arc::new(crate::sourcecontrol::SourceControl::new(
            &config,
            settings.clone(),
            egress.clone(),
        ));
        Self {
            keys,
            limits,
            config: Arc::new(config),
            settings,
            egress,
            assist,
            jwt,
            certs,
            identity: Arc::new(IdentityState::new()),
            inspector,
            health,
            metrics,
            mocks,
            router: Arc::new(OnceLock::new()),
            harness,
            portal,
            agents,
            registry: Arc::new(crate::mcp::registry::Registry::default()),
            git,
        }
    }
}

impl FromRef<AppState> for Arc<Config> {
    fn from_ref(state: &AppState) -> Self {
        state.config.clone()
    }
}

impl FromRef<AppState> for Arc<Settings> {
    fn from_ref(state: &AppState) -> Self {
        state.settings.clone()
    }
}

impl FromRef<AppState> for Arc<Egress> {
    fn from_ref(state: &AppState) -> Self {
        state.egress.clone()
    }
}

impl FromRef<AppState> for Arc<Assist> {
    fn from_ref(state: &AppState) -> Self {
        state.assist.clone()
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

impl FromRef<AppState> for Arc<Metrics> {
    fn from_ref(state: &AppState) -> Self {
        state.metrics.clone()
    }
}

impl FromRef<AppState> for MockStore {
    fn from_ref(state: &AppState) -> Self {
        state.mocks.clone()
    }
}

impl FromRef<AppState> for Inspector {
    fn from_ref(state: &AppState) -> Self {
        state.inspector.clone()
    }
}

impl FromRef<AppState> for Arc<Keys> {
    fn from_ref(state: &AppState) -> Self {
        state.keys.clone()
    }
}
