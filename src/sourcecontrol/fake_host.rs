//! Tests only: an in-process fake of each git host's API on loopback, as
//! far as `host.rs` uses one (the call that opens a pull request, and how
//! it is refused). No test calls a real host.
//!
//! A fake knows one repository (a bare repository on disk) and looks there
//! for the two branches a pull request names, as a host would. A token is
//! [`WRITER`] (may open), [`READER`] (may not), [`ECHO`] (refused in words
//! that repeat the credential, as a careless host or proxy might) or
//! unknown. The paths and bodies are each host's own:
//!
//! - `github`: `POST /api/v3/repos/{owner}/{repo}/pulls` with `{title,
//!   head, base, body}`, answering `201 {number, html_url}`, `401 Bad
//!   credentials`, `403`, `404 Not Found`, `422 Validation Failed`.
//! - `gitlab`: `POST /api/v4/projects/{url-encoded path}/merge_requests`
//!   with `{source_branch, target_branch, title, description}`, answering
//!   `201 {iid, web_url}`, `401`, `403`, `404 Project Not Found`, `400`.

use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

/// A token that may open a pull request.
pub const WRITER: &str = "writer-token-4f1c9a7e2b";
/// A token that may read the repository only.
pub const READER: &str = "reader-token-83d02c6f51";
/// A token the host refuses in words that repeat it.
pub const ECHO: &str = "echoed-token-c07be19a44";

/// One fake host.
#[derive(Clone)]
pub struct Fake {
    /// The API's base (`git.host_api`).
    pub api: String,
    /// The same API behind a redirect, which must never be followed.
    pub redirecting_api: String,
    /// Every call that arrived: `{path, authorization, body}`.
    pub calls: Arc<Mutex<Vec<Value>>>,
    inner: Arc<Inner>,
}

struct Inner {
    /// The repository the host knows, as its API names it (`acme/remote`).
    repository: String,
    /// Where the host keeps it.
    bare: Mutex<PathBuf>,
    /// How long the host takes to answer.
    delay: Mutex<Duration>,
    /// What an accepted call answers instead of the usual body.
    answer: Mutex<Option<Value>>,
}

impl Fake {
    /// Start a fake of `kind` (`github` or `gitlab`) that knows
    /// `repository`, kept in the bare repository at `bare`.
    pub async fn start(kind: &'static str, repository: &str, bare: &FsPath) -> Fake {
        let inner = Arc::new(Inner {
            repository: repository.to_string(),
            bare: Mutex::new(bare.to_path_buf()),
            delay: Mutex::new(Duration::ZERO),
            answer: Mutex::new(None),
        });
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = (inner.clone(), calls.clone());
        let (prefix, route) = match kind {
            "github" => ("/api/v3", "/repos/{owner}/{repo}/pulls"),
            _ => ("/api/v4", "/projects/{id}/merge_requests"),
        };
        let api = Router::new().route(
            route,
            match kind {
                "github" => post(github),
                _ => post(gitlab),
            },
        );
        let app = Router::new()
            .nest(prefix, api)
            .route(MOVED, post(moved))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake host");
        let addr = listener.local_addr().expect("the fake host's address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Fake {
            api: format!("http://{addr}{prefix}"),
            redirecting_api: format!("http://{addr}/moved{prefix}"),
            calls,
            inner,
        }
    }

    /// The host takes this long to answer from now on.
    pub fn slow(&self, delay: Duration) {
        *self.inner.delay.lock().expect("delay") = delay;
    }

    /// The host keeps its repository somewhere else from now on (one that
    /// lacks the branches of the instance's remote).
    pub fn keeps(&self, bare: &FsPath) {
        *self.inner.bare.lock().expect("bare") = bare.to_path_buf();
    }

    /// An accepted call answers this body from now on.
    pub fn answers(&self, body: Value) {
        *self.inner.answer.lock().expect("answer") = Some(body);
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("calls").len()
    }

    pub fn last_call(&self) -> Value {
        self.calls
            .lock()
            .expect("calls")
            .last()
            .cloned()
            .unwrap_or(Value::Null)
    }
}

type Shared = (Arc<Inner>, Arc<Mutex<Vec<Value>>>);

/// Where the API "moved" from (a constant: the catalogue's scan of route
/// literals is for the instance's own routes).
const MOVED: &str = "/moved/{*rest}";

fn has_branch(bare: &FsPath, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// What every call goes through first: recorded, delayed, the token read.
async fn arrive(shared: &Shared, path: String, headers: &HeaderMap, body: &Value) -> String {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    shared.1.lock().expect("calls").push(json!({
        "path": path,
        "authorization": authorization,
        "accept": headers.get("accept").and_then(|v| v.to_str().ok()),
        "api_version": headers.get("x-github-api-version").and_then(|v| v.to_str().ok()),
        "user_agent": headers.get("user-agent").and_then(|v| v.to_str().ok()),
        "body": body,
    }));
    let delay = *shared.0.delay.lock().expect("delay");
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    authorization
        .strip_prefix("Bearer ")
        .unwrap_or("")
        .to_string()
}

fn answer(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

async fn github(
    State(shared): State<Shared>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let token = arrive(
        &shared,
        format!("/repos/{owner}/{repo}/pulls"),
        &headers,
        &body,
    )
    .await;
    let docs = "https://docs.github.com/rest/pulls/pulls#create-a-pull-request";
    match token.as_str() {
        WRITER => {}
        READER => {
            return answer(
                StatusCode::FORBIDDEN,
                json!({ "message": "Resource not accessible by personal access token", "documentation_url": docs, "status": "403" }),
            )
        }
        ECHO => {
            return answer(
                StatusCode::FORBIDDEN,
                json!({ "message": format!("Bearer {ECHO} may not do this"), "documentation_url": docs, "status": "403" }),
            )
        }
        _ => {
            return answer(
                StatusCode::UNAUTHORIZED,
                json!({ "message": "Bad credentials", "documentation_url": "https://docs.github.com/rest", "status": "401" }),
            )
        }
    }
    if format!("{owner}/{repo}") != shared.0.repository {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "message": "Not Found", "documentation_url": docs, "status": "404" }),
        );
    }
    let invalid = |field: &str| {
        answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Validation Failed",
                "errors": [{ "resource": "PullRequest", "field": field, "code": "invalid" }],
                "documentation_url": docs,
                "status": "422",
            }),
        )
    };
    let (Some(head), Some(base), Some(title)) = (
        body["head"].as_str(),
        body["base"].as_str(),
        body["title"].as_str(),
    ) else {
        return invalid("title");
    };
    let bare = shared.0.bare.lock().expect("bare").clone();
    if !has_branch(&bare, base) {
        return invalid("base");
    }
    if !has_branch(&bare, head) {
        return invalid("head");
    }
    if let Some(custom) = shared.0.answer.lock().expect("answer").clone() {
        return answer(StatusCode::CREATED, custom);
    }
    let number = shared.1.lock().expect("calls").len();
    answer(
        StatusCode::CREATED,
        json!({
            "id": 1_000 + number,
            "number": number,
            "state": "open",
            "title": title,
            "html_url": format!("https://github.example/{}/pull/{number}", shared.0.repository),
            "head": { "ref": head },
            "base": { "ref": base },
        }),
    )
}

async fn gitlab(
    State(shared): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let token = arrive(
        &shared,
        format!("/projects/{id}/merge_requests"),
        &headers,
        &body,
    )
    .await;
    match token.as_str() {
        WRITER => {}
        READER => return answer(StatusCode::FORBIDDEN, json!({ "message": "403 Forbidden" })),
        ECHO => {
            return answer(
                StatusCode::FORBIDDEN,
                json!({ "message": format!("403 Forbidden for {ECHO}") }),
            )
        }
        _ => {
            return answer(
                StatusCode::UNAUTHORIZED,
                json!({ "message": "401 Unauthorized" }),
            )
        }
    }
    // The path parameter arrives decoded: `acme%2Fremote` is one segment.
    if id != shared.0.repository {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "message": "404 Project Not Found" }),
        );
    }
    let (Some(head), Some(base), Some(title)) = (
        body["source_branch"].as_str(),
        body["target_branch"].as_str(),
        body["title"].as_str(),
    ) else {
        return answer(
            StatusCode::BAD_REQUEST,
            json!({ "error": "source_branch, target_branch, title are missing" }),
        );
    };
    let bare = shared.0.bare.lock().expect("bare").clone();
    for (field, branch) in [("target_branch", base), ("source_branch", head)] {
        if !has_branch(&bare, branch) {
            let mut fields = serde_json::Map::new();
            fields.insert(field.to_string(), json!(["does not exist"]));
            return answer(StatusCode::BAD_REQUEST, json!({ "message": fields }));
        }
    }
    if let Some(custom) = shared.0.answer.lock().expect("answer").clone() {
        return answer(StatusCode::CREATED, custom);
    }
    let number = shared.1.lock().expect("calls").len();
    answer(
        StatusCode::CREATED,
        json!({
            "id": 5_000 + number,
            "iid": number,
            "state": "opened",
            "title": title,
            "web_url": format!("https://gitlab.example/{}/-/merge_requests/{number}", shared.0.repository),
            "source_branch": head,
            "target_branch": base,
        }),
    )
}

/// The API moved: a redirect to where it is. A client that followed it
/// would open the pull request; the instance never does.
async fn moved(State(shared): State<Shared>, Path(rest): Path<String>) -> Response {
    shared
        .1
        .lock()
        .expect("calls")
        .push(json!({ "path": format!("/moved/{rest}"), "redirected": true }));
    (
        StatusCode::TEMPORARY_REDIRECT,
        [(axum::http::header::LOCATION, format!("/{rest}"))],
    )
        .into_response()
}
