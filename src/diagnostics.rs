//! Instance diagnostics: one answer to "is this instance healthy, and if not,
//! what do I do" (`GET /_rustybin/diagnostics`, `rustybin doctor`, the
//! `diagnose_instance` tool, Management, Diagnostics). Not readiness
//! (`/_rustybin/ready`, which never fails on these) and not the demo toggle
//! (`/health`). Every check says what it saw and how to fix it, never a
//! credential.

use std::sync::Mutex;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const PATH: &str = "/_rustybin/diagnostics";

/// Startup problems kept for the diagnostics (listeners that did not bind).
const MAX_NOTES: usize = 32;

/// What the server recorded while starting (in `AppState.diagnostics`).
#[derive(Default)]
pub struct Diagnostics {
    notes: Mutex<Vec<(String, String)>>,
}

impl Diagnostics {
    /// Record a startup problem under `what` (`https`, `grpc`, ...).
    pub fn note(&self, what: &str, problem: impl Into<String>) {
        let mut notes = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        if notes.len() < MAX_NOTES {
            notes.push((what.to_string(), problem.into()));
        }
    }

    pub fn notes(&self) -> Vec<(String, String)> {
        self.notes.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// One check's result.
fn check(id: &str, status: &str, message: impl Into<String>, fix: Option<&str>) -> Value {
    json!({ "id": id, "status": status, "message": message.into(), "fix": fix })
}

/// Every check, in a fixed order.
pub fn report(state: &AppState) -> Value {
    let config = &state.config;
    let mut checks = Vec::new();

    // The state directory: configured, present, writable.
    checks.push(match &config.state_dir {
        None => check(
            "state_dir",
            "warn",
            "no RUSTYBIN_STATE_DIR: settings, secrets, sessions, saved mocks and the git working copy last until the process ends",
            Some("set RUSTYBIN_STATE_DIR to a writable directory (a volume in a container)"),
        ),
        Some(dir) => {
            let probe = dir.join(format!(".diagnostics-{}", uuid::Uuid::new_v4().simple()));
            match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&probe, b"ok")) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    check("state_dir", "ok", format!("{} is writable", dir.display()), None)
                }
                Err(e) => check(
                    "state_dir",
                    "fail",
                    format!("{} is configured but cannot be written: {e}", dir.display()),
                    Some("give the process write access to the directory, or point RUSTYBIN_STATE_DIR elsewhere"),
                ),
            }
        }
    });

    // The secrets key.
    checks.push(match state.settings.key_source() {
        crate::settings::KeySource::Process => check(
            "secrets_key",
            "warn",
            "the secrets key lives only in this process: stored secrets cannot be opened after a restart",
            Some("set RUSTYBIN_SECRETS_KEY, or RUSTYBIN_STATE_DIR (the key is kept in <state>/secrets.key)"),
        ),
        source => check(
            "secrets_key",
            "ok",
            format!("the secrets key comes from {}", source.as_str()),
            None,
        ),
    });
    let unreadable = state.settings.unreadable_secrets();
    checks.push(if unreadable.is_empty() {
        check("secrets_unreadable", "ok", "every stored secret opens", None)
    } else {
        check(
            "secrets_unreadable",
            "fail",
            format!(
                "stored secrets sealed under another key: {}",
                unreadable.join(", ")
            ),
            Some("restore the secrets key they were sealed with, or set the secrets again (Management)"),
        )
    });

    // What failed to load.
    checks.push(match state.settings.load_error() {
        None => check("settings_file", "ok", "the settings document loaded", None),
        Some(e) => check(
            "settings_file",
            "fail",
            format!("{e}: the instance started with empty settings"),
            Some("move the file aside and restore a good copy (or apply a rustybin.yaml); the next settings change writes over it"),
        ),
    });
    let load_errors = state.mocks.load_errors();
    checks.push(if load_errors.is_empty() {
        check("mock_load_errors", "ok", "every mock loaded", None)
    } else {
        check(
            "mock_load_errors",
            "fail",
            format!(
                "{} load error(s): {}",
                load_errors.len(),
                load_errors.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
            ),
            Some("fix the files named (the mocks directory, RUSTYBIN_CONFIG_FILE, the saved mocks), then reload (POST /_rustybin/mocks/reload)"),
        )
    });
    checks.push(match &config.config_file {
        None => check("config_file", "ok", "no RUSTYBIN_CONFIG_FILE", None),
        Some(path) if std::path::Path::new(path).is_file() => {
            check("config_file", "ok", format!("{path} is readable"), None)
        }
        Some(path) => check(
            "config_file",
            "fail",
            format!("RUSTYBIN_CONFIG_FILE {path} cannot be read"),
            Some("check the path and its permissions"),
        ),
    });

    // Listeners that did not start.
    let notes = state.diagnostics.notes();
    checks.push(if notes.is_empty() {
        check("listeners", "ok", "every configured listener started", None)
    } else {
        check(
            "listeners",
            "warn",
            notes
                .iter()
                .map(|(w, p)| format!("{w}: {p}"))
                .collect::<Vec<_>>()
                .join("; "),
            Some("free the port or change it (RUSTYBIN_HTTPS_PORT, RUSTYBIN_GRPC_PORT), or set it to 0 to turn the listener off"),
        )
    });

    // Outbound calls refused in the last hour.
    let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
    let mut refused: std::collections::BTreeMap<&'static str, usize> = Default::default();
    for d in state.egress.decisions() {
        let recent = chrono::DateTime::parse_from_rfc3339(&d.at)
            .map(|t| t.with_timezone(&chrono::Utc) >= hour_ago)
            .unwrap_or(false);
        if recent && !d.allowed {
            *refused.entry(d.purpose.as_str()).or_default() += 1;
        }
    }
    checks.push(if refused.is_empty() {
        check("egress_refusals", "ok", "no outbound call refused in the last hour", None)
    } else {
        check(
            "egress_refusals",
            "warn",
            format!(
                "outbound calls refused in the last hour: {}",
                refused
                    .iter()
                    .map(|(p, n)| format!("{p} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some("see the decisions under Management, Network (GET /_rustybin/egress); allow the hosts that should be reached"),
        )
    });

    // Source control's last operation.
    checks.push(match state.git.last_op() {
        Some(op) if !op.ok => check(
            "source_control",
            "warn",
            format!("the last git operation ({}) failed: {}", op.op, op.detail),
            Some("see Management, Source control"),
        ),
        Some(op) => check(
            "source_control",
            "ok",
            format!("the last git operation ({}) succeeded", op.op),
            None,
        ),
        None => check("source_control", "ok", "no git operation since start", None),
    });
    // A diverged branch waits for a person: the instance never merges and
    // the automatic mode never resolves.
    if let Some((ahead, behind)) = state.git.diverged() {
        checks.push(check(
            "source_control_diverged",
            "warn",
            format!(
                "the repository and this instance have diverged ({ahead} commit(s) here, {behind} there): the instance never merges and the automatic mode does not resolve it"
            ),
            Some("Management, Source control shows what differs: resolve with the instance's copy, the repository's, or put the instance's side in a pull request (rustybin git resolve)"),
        ));
    }
    // Signed commits: on, and able to work.
    if state.git.signing_enabled() {
        checks.push(match state.git.signing_problem() {
            Some(problem) => check(
                "source_control_signing",
                "warn",
                problem,
                Some("generate a signing key (Management, Source control, or rustybin git signing-key --generate) and register its public key at the git host, or turn git.sign off; commits are refused until then"),
            ),
            None => check(
                "source_control_signing",
                "ok",
                "commits are signed with the signing key",
                None,
            ),
        });
    }

    // The sign-in.
    checks.push(
        if crate::login::enabled(config) && crate::login::default_password(state) {
            check(
                "default_password",
                "warn",
                "the console still accepts the default password (rustybin / rustybin)",
                Some("change it under Management, Sign-in, or set RUSTYBIN_CONSOLE_PASSWORD"),
            )
        } else {
            check(
                "default_password",
                "ok",
                "the default password is not in use",
                None,
            )
        },
    );

    // Whose address the limits and visits count, behind a proxy.
    if config.public_mode && config.trust_forward {
        checks.push(
            if config.forward_header.get() == crate::session::ForwardHeader::Any {
                check(
                    "client_address",
                    "warn",
                    "a client can name its own address: with RUSTYBIN_TRUST_FORWARD every \
                     forwarding header is believed, and the per-address limits count what it says",
                    Some(
                        "have your proxy set one header (X-Real-IP from its verified client \
                         address) and name it with RUSTYBIN_TRUST_FORWARD_HEADER",
                    ),
                )
            } else {
                check(
                    "client_address",
                    "ok",
                    format!(
                        "the client address comes from {} alone",
                        config.forward_header.get().as_str()
                    ),
                    None,
                )
            },
        );
    }

    // The user store (password mode).
    if let Some((status, message, fix)) = crate::rbac::diagnostics(state) {
        checks.push(check("users", status, message, fix));
    }

    // Single sign-on (password mode): set up when it is on, and a way in
    // when it is the only one.
    if let Some((status, message, fix)) = crate::sso::diagnostics(state) {
        checks.push(check("sso", status, message, fix));
    }

    // The harness engine.
    if let Some(h) = &state.harness {
        checks.push(match h.engine_problem() {
            Some(e) => check(
                "harness",
                "fail",
                format!("harness mode is on but its engine is not running: {e}"),
                Some("see the harness section of the configuration (threads, descriptor limit)"),
            ),
            None => check("harness", "ok", "the load engine is running", None),
        });
    }

    // AI assist: chosen but not usable.
    let assist = state.assist.status();
    if assist.kind.is_some() && !assist.enabled {
        checks.push(check(
            "assist",
            "warn",
            format!(
                "AI assist has a provider but is off: {}",
                assist.reason.unwrap_or_default()
            ),
            Some("finish the setup under Management, AI"),
        ));
    }

    let worst = if checks.iter().any(|c| c["status"] == "fail") {
        "fail"
    } else if checks.iter().any(|c| c["status"] == "warn") {
        "warn"
    } else {
        "ok"
    };
    json!({
        "status": worst,
        "checked_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "checks": checks,
    })
}

async fn handler(State(state): State<AppState>) -> Response {
    // The state directory probe touches the disk: off the request threads.
    let s = state.clone();
    match tokio::task::spawn_blocking(move || report(&s)).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "the diagnostics did not finish" })),
        )
            .into_response(),
    }
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route(PATH, get(handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        PATH,
        &["GET"],
        category::CONTROL,
        "Instance diagnostics: storage, the secrets key, load errors, listeners, outbound refusals, source control, the sign-in (admin)",
    )
.permission(crate::rbac::Permission::SettingsRead)
    .description(
        "Each check has an id, a status (ok, warn, fail), a message and a fix; the answer's \
         status is the worst. Not readiness (/_rustybin/ready) and not the demo toggle (/health).",
    )
    .example(Example::get("Diagnostics", PATH))]
}

pub fn openapi_paths() -> Value {
    json!({
        PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Instance diagnostics",
                "description": "Every check with its id, status (ok, warn, fail), message and fix (never a credential), and the worst status. Admin.",
                "operationId": "getRustybinDiagnostics",
                "responses": {
                    "200": { "description": "The checks", "content": { "application/json": { "schema": { "type": "object" } } } },
                    "401": { "description": "Admin token required" }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_support::{body_json, get_request, test_app_with, test_state_with};
    use tower::ServiceExt;

    fn status_of<'a>(v: &'a Value, id: &str) -> &'a str {
        v["checks"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["id"] == id))
            .and_then(|c| c["status"].as_str())
            .unwrap_or("missing")
    }

    #[test]
    fn a_public_instance_that_believes_every_forwarding_header_is_warned() {
        let mut c = Config::for_tests();
        c.public_mode = true;
        c.trust_forward = true;
        let r = report(&test_state_with(c.clone()));
        assert_eq!(status_of(&r, "client_address"), "warn", "{r}");
        c.forward_header = std::sync::Arc::new(crate::session::TrustedHeader::new(
            crate::session::ForwardHeader::XRealIp,
        ));
        let r = report(&test_state_with(c));
        assert_eq!(status_of(&r, "client_address"), "ok", "{r}");
        // Not behind a proxy, or not public: no such question.
        let r = report(&test_state_with(Config::for_tests()));
        assert_eq!(status_of(&r, "client_address"), "missing", "{r}");
    }

    #[test]
    fn a_configured_but_unwritable_state_dir_fails() {
        let dir = tempfile::tempdir().expect("tmp");
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").expect("file");
        let mut c = Config::for_tests();
        c.state_dir = Some(file.join("state"));
        let state = test_state_with(c);
        let r = report(&state);
        assert_eq!(status_of(&r, "state_dir"), "fail", "{r}");
        assert_eq!(r["status"], "fail");
        // No state directory: a warning, and the process-only secrets key too.
        let r = report(&test_state_with(Config::for_tests()));
        assert_eq!(status_of(&r, "state_dir"), "warn");
        assert_eq!(status_of(&r, "secrets_key"), "warn");
        // A writable one.
        let mut c = Config::for_tests();
        c.state_dir = Some(dir.path().join("ok"));
        let r = report(&test_state_with(c));
        assert_eq!(status_of(&r, "state_dir"), "ok", "{r}");
        assert_eq!(status_of(&r, "secrets_key"), "ok");
    }

    #[test]
    fn startup_problems_and_load_errors_are_reported() {
        let state = test_state_with(Config::for_tests());
        state.diagnostics.note("https", "address in use");
        state
            .mocks
            .record_load_error("orders.yaml: not a spec".into());
        let r = report(&state);
        assert_eq!(status_of(&r, "listeners"), "warn");
        assert_eq!(status_of(&r, "mock_load_errors"), "fail");
        assert!(r.to_string().contains("address in use"));
    }

    #[test]
    fn a_settings_document_that_did_not_load_fails() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("settings.json"), b"{not json").expect("write");
        let mut c = Config::for_tests();
        c.state_dir = Some(dir.path().to_path_buf());
        let r = report(&test_state_with(c));
        assert_eq!(status_of(&r, "settings_file"), "fail");
        assert!(r.to_string().contains("is not a settings document"));
        let r = report(&test_state_with(Config::for_tests()));
        assert_eq!(status_of(&r, "settings_file"), "ok");
    }

    #[tokio::test]
    async fn the_route_is_admin_only() {
        let mut c = Config::for_tests();
        c.admin_token = Some("admin-secret".into());
        let app = test_app_with(test_state_with(c));
        let resp = app.clone().oneshot(get_request(PATH)).await.expect("r");
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
        let resp = app
            .oneshot(
                axum::http::Request::get(PATH)
                    .header("authorization", "Bearer admin-secret")
                    .body(axum::body::Body::empty())
                    .expect("req"),
            )
            .await
            .expect("r");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["checks"].as_array().is_some_and(|a| a.len() >= 9));
    }
}
