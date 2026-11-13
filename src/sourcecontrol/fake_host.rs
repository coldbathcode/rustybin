//! Tests only: an in-process fake of each git host's API on loopback, as
//! far as `host.rs` uses one (the call that opens a pull request, how it
//! is refused, and the read that lists the open ones). No test calls a
//! real host.
//!
//! A fake knows one repository (a bare repository on disk) and looks there
//! for the two branches a pull request names, as a host would. It remembers
//! the pull requests it opened and refuses a second one for the same two
//! branches in the host's own way. A token is
//! [`WRITER`] (may open), [`READER`] (may not), [`ECHO`] (refused in words
//! that repeat the credential, as a careless host or proxy might) or
//! unknown. The paths and bodies are each host's own:
//!
//! - `github`: `POST /api/v3/repos/{owner}/{repo}/pulls` with `{title,
//!   head, base, body}`, answering `201 {number, html_url}`, `401 Bad
//!   credentials`, `403`, `404 Not Found`, `422 Validation Failed`.
//! - `gitlab`: `POST /api/v4/projects/{url-encoded path}/merge_requests`
//!   with `{source_branch, target_branch, title, description}`, answering
//!   `201 {iid, web_url}`, `401`, `403`, `404 Project Not Found`, `400`,
//!   `409` for one already open. `GET` of the same path with `state`,
//!   `source_branch` and `target_branch` lists them.
//! - `gitea` (Gitea and Forgejo): `POST /api/v1/repos/{owner}/{repo}/pulls`
//!   with `{title, head, base, body}` and `Authorization: token ...`,
//!   answering `201 {number, html_url}`, `401`, `403`, `404`, `409` for one
//!   already open. `GET` of the same path lists the open ones, newest
//!   first, and has no filter by branch.
//!
//! `github` also lists: `GET` of its path with `state`, `head`
//! (`owner:branch`) and `base`, and refuses one already open with `422`.

use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
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
    /// The pull requests the host opened, in its own shape.
    open: Mutex<Vec<Value>>,
    /// What the list answers instead of the open ones.
    listing: Mutex<Option<Value>>,
    /// Whether the list answers at all (`false`: `500`).
    lists: Mutex<bool>,
}

impl Fake {
    /// Start a fake of `kind` (`github`, `gitlab` or `gitea`) that knows
    /// `repository`, kept in the bare repository at `bare`.
    pub async fn start(kind: &'static str, repository: &str, bare: &FsPath) -> Fake {
        let inner = Arc::new(Inner {
            repository: repository.to_string(),
            bare: Mutex::new(bare.to_path_buf()),
            delay: Mutex::new(Duration::ZERO),
            answer: Mutex::new(None),
            open: Mutex::new(Vec::new()),
            listing: Mutex::new(None),
            lists: Mutex::new(true),
        });
        let calls = Arc::new(Mutex::new(Vec::new()));
        let state = (inner.clone(), calls.clone());
        let (prefix, api) = match kind {
            "github" => (
                "/api/v3",
                Router::new().route(REPO_PULLS, post(github).get(github_list)),
            ),
            "gitea" => (
                "/api/v1",
                Router::new().route(REPO_PULLS, post(gitea).get(gitea_list)),
            ),
            _ => (
                "/api/v4",
                Router::new().route(PROJECT_MERGE_REQUESTS, post(gitlab).get(gitlab_list)),
            ),
        };
        let app = Router::new()
            .nest(prefix, api)
            .route(MOVED, post(moved).get(moved))
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

    /// The list answers this body from now on.
    pub fn lists(&self, body: Value) {
        *self.inner.listing.lock().expect("listing") = Some(body);
    }

    /// The list fails from now on (`500`).
    pub fn list_fails(&self) {
        *self.inner.lists.lock().expect("lists") = false;
    }

    /// How many calls of `method` arrived.
    pub fn count_of(&self, method: &str) -> usize {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|c| c["method"] == json!(method))
            .count()
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
const REPO_PULLS: &str = "/repos/{owner}/{repo}/pulls";
const PROJECT_MERGE_REQUESTS: &str = "/projects/{id}/merge_requests";

type Params = Query<std::collections::BTreeMap<String, String>>;

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
    arrive_as(shared, "POST", path, headers, body).await
}

/// The same for a read: `body` is then its query.
async fn arrive_as(
    shared: &Shared,
    method: &str,
    path: String,
    headers: &HeaderMap,
    body: &Value,
) -> String {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    shared.1.lock().expect("calls").push(json!({
        "method": method,
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
        .or_else(|| authorization.strip_prefix("token "))
        .unwrap_or("")
        .to_string()
}

/// The open pull request for the two branches, when the host has one.
fn open_for(shared: &Shared, head: &str, base: &str) -> Option<Value> {
    shared
        .0
        .open
        .lock()
        .expect("open")
        .iter()
        .find(|p| p["_head"] == json!(head) && p["_base"] == json!(base))
        .cloned()
}

/// Remember an opened pull request; the answer is it without the fake's
/// own fields.
fn remember(shared: &Shared, head: &str, base: &str, mut pr: Value) -> Value {
    let answer = pr.clone();
    pr["_head"] = json!(head);
    pr["_base"] = json!(base);
    shared.0.open.lock().expect("open").push(pr);
    answer
}

fn next_number(shared: &Shared) -> usize {
    shared.0.open.lock().expect("open").len() + 1
}

/// What a list answers: the body a test set, else the open pull requests
/// `keep` lets through, newest first, without the fake's own fields.
fn listed(shared: &Shared, keep: impl Fn(&Value) -> bool) -> Response {
    if !*shared.0.lists.lock().expect("lists") {
        return answer(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "message": "the list is not available" }),
        );
    }
    if let Some(custom) = shared.0.listing.lock().expect("listing").clone() {
        return answer(StatusCode::OK, custom);
    }
    let open: Vec<Value> = shared
        .0
        .open
        .lock()
        .expect("open")
        .iter()
        .rev()
        .filter(|p| keep(p))
        .map(|p| {
            let mut p = p.clone();
            if let Some(fields) = p.as_object_mut() {
                fields.remove("_head");
                fields.remove("_base");
            }
            p
        })
        .collect();
    answer(StatusCode::OK, json!(open))
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
    if open_for(&shared, head, base).is_some() {
        return answer(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Validation Failed",
                "errors": [{
                    "resource": "PullRequest",
                    "code": "custom",
                    "message": format!("A pull request already exists for {owner}:{head}."),
                }],
                "documentation_url": docs,
                "status": "422",
            }),
        );
    }
    if let Some(custom) = shared.0.answer.lock().expect("answer").clone() {
        return answer(StatusCode::CREATED, custom);
    }
    let number = next_number(&shared);
    let pr = json!({
        "id": 1_000 + number,
        "number": number,
        "state": "open",
        "title": title,
        "html_url": format!("https://github.example/{}/pull/{number}", shared.0.repository),
        "head": { "ref": head },
        "base": { "ref": base },
    });
    answer(StatusCode::CREATED, remember(&shared, head, base, pr))
}

/// `GET /repos/{owner}/{repo}/pulls`: `state` (open by default), `head` as
/// `owner:branch` (a `head` without the owner filters nothing, as the host
/// documents) and `base`.
async fn github_list(
    State(shared): State<Shared>,
    Path((owner, repo)): Path<(String, String)>,
    Query(query): Params,
    headers: HeaderMap,
) -> Response {
    let token = arrive_as(
        &shared,
        "GET",
        format!("/repos/{owner}/{repo}/pulls"),
        &headers,
        &json!(query),
    )
    .await;
    if !matches!(token.as_str(), WRITER | READER) {
        return answer(
            StatusCode::UNAUTHORIZED,
            json!({ "message": "Bad credentials", "documentation_url": "https://docs.github.com/rest", "status": "401" }),
        );
    }
    if format!("{owner}/{repo}") != shared.0.repository {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "message": "Not Found", "status": "404" }),
        );
    }
    if query
        .get("state")
        .is_some_and(|s| s != "open" && s != "all")
    {
        return answer(StatusCode::OK, json!([]));
    }
    let head = query
        .get("head")
        .and_then(|h| h.strip_prefix(&format!("{owner}:")))
        .map(str::to_string);
    let base = query.get("base").cloned();
    listed(&shared, |p| {
        head.as_ref().is_none_or(|h| p["_head"] == json!(h))
            && base.as_ref().is_none_or(|b| p["_base"] == json!(b))
    })
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
    if let Some(existing) = open_for(&shared, head, base) {
        return answer(
            StatusCode::CONFLICT,
            json!({ "message": [format!(
                "Another open merge request already exists for this source branch: !{}",
                existing["iid"]
            )] }),
        );
    }
    if let Some(custom) = shared.0.answer.lock().expect("answer").clone() {
        return answer(StatusCode::CREATED, custom);
    }
    let number = next_number(&shared);
    let mr = json!({
        "id": 5_000 + number,
        "iid": number,
        "state": "opened",
        "title": title,
        "web_url": format!("https://gitlab.example/{}/-/merge_requests/{number}", shared.0.repository),
        "source_branch": head,
        "target_branch": base,
    });
    answer(StatusCode::CREATED, remember(&shared, head, base, mr))
}

/// `GET /projects/{id}/merge_requests`: `state`, `source_branch` and
/// `target_branch`.
async fn gitlab_list(
    State(shared): State<Shared>,
    Path(id): Path<String>,
    Query(query): Params,
    headers: HeaderMap,
) -> Response {
    let token = arrive_as(
        &shared,
        "GET",
        format!("/projects/{id}/merge_requests"),
        &headers,
        &json!(query),
    )
    .await;
    if !matches!(token.as_str(), WRITER | READER) {
        return answer(
            StatusCode::UNAUTHORIZED,
            json!({ "message": "401 Unauthorized" }),
        );
    }
    if id != shared.0.repository {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "message": "404 Project Not Found" }),
        );
    }
    if query
        .get("state")
        .is_some_and(|s| s != "opened" && s != "all")
    {
        return answer(StatusCode::OK, json!([]));
    }
    let head = query.get("source_branch").cloned();
    let base = query.get("target_branch").cloned();
    listed(&shared, |p| {
        head.as_ref().is_none_or(|h| p["_head"] == json!(h))
            && base.as_ref().is_none_or(|b| p["_base"] == json!(b))
    })
}

/// Where this host's errors point (`url` of every refusal).
const GITEA_DOCS: &str = "https://gitea.example/api/swagger";

async fn gitea(
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
    let said = |status: StatusCode, message: String| {
        answer(status, json!({ "message": message, "url": GITEA_DOCS }))
    };
    match token.as_str() {
        WRITER => {}
        READER => {
            return said(
                StatusCode::FORBIDDEN,
                "token does not have at least one of required scope(s): [write:repository]".into(),
            )
        }
        ECHO => {
            return said(
                StatusCode::FORBIDDEN,
                format!("token {ECHO} does not have at least one of required scope(s)"),
            )
        }
        _ => {
            return said(
                StatusCode::UNAUTHORIZED,
                "invalid username, password or token".into(),
            )
        }
    }
    if format!("{owner}/{repo}") != shared.0.repository {
        return said(
            StatusCode::NOT_FOUND,
            "The target couldn't be found.".into(),
        );
    }
    let (Some(head), Some(base), Some(title)) = (
        body["head"].as_str(),
        body["base"].as_str(),
        body["title"].as_str(),
    ) else {
        return said(StatusCode::UNPROCESSABLE_ENTITY, "[Title]: Required".into());
    };
    // A branch this host does not know is "not found" here, not a
    // validation failure.
    let bare = shared.0.bare.lock().expect("bare").clone();
    if !has_branch(&bare, base) || !has_branch(&bare, head) {
        return said(
            StatusCode::NOT_FOUND,
            "The target couldn't be found.".into(),
        );
    }
    if let Some(existing) = open_for(&shared, head, base) {
        return said(
            StatusCode::CONFLICT,
            format!(
                "pull request already exists for these targets [id: {}, issue_id: {}, head_repo_id: 1, base_repo_id: 1, head_branch: {head}, base_branch: {base}]",
                existing["id"], existing["id"]
            ),
        );
    }
    if let Some(custom) = shared.0.answer.lock().expect("answer").clone() {
        return answer(StatusCode::CREATED, custom);
    }
    let number = next_number(&shared);
    let pr = json!({
        "id": 9_000 + number,
        "number": number,
        "state": "open",
        "title": title,
        "html_url": format!("https://gitea.example/{}/pulls/{number}", shared.0.repository),
        "head": { "ref": head, "label": head },
        "base": { "ref": base, "label": base },
    });
    answer(StatusCode::CREATED, remember(&shared, head, base, pr))
}

/// `GET /repos/{owner}/{repo}/pulls`: `state` and `limit`. This API's list
/// has no filter by branch: every open pull request is answered.
async fn gitea_list(
    State(shared): State<Shared>,
    Path((owner, repo)): Path<(String, String)>,
    Query(query): Params,
    headers: HeaderMap,
) -> Response {
    let token = arrive_as(
        &shared,
        "GET",
        format!("/repos/{owner}/{repo}/pulls"),
        &headers,
        &json!(query),
    )
    .await;
    if !matches!(token.as_str(), WRITER | READER) {
        return answer(
            StatusCode::UNAUTHORIZED,
            json!({ "message": "invalid username, password or token", "url": GITEA_DOCS }),
        );
    }
    if format!("{owner}/{repo}") != shared.0.repository {
        return answer(
            StatusCode::NOT_FOUND,
            json!({ "message": "The target couldn't be found.", "url": GITEA_DOCS }),
        );
    }
    if query
        .get("state")
        .is_some_and(|s| s != "open" && s != "all")
    {
        return answer(StatusCode::OK, json!([]));
    }
    listed(&shared, |_| true)
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
