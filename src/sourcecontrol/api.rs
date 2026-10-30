//! The source control routes (admin, audited, serialised by one lock):
//!
//! - `GET /_rustybin/git?fetch=true`: status (remote, branch, head, ahead
//!   and behind, the drift between the working copy and the instance, the
//!   deploy key, the timeline).
//! - `POST /_rustybin/git/init`: clone, or initialise for an empty remote.
//! - `POST /_rustybin/git/commit` `{message}`: export into the working copy
//!   and commit.
//! - `POST /_rustybin/git/push`, `POST /_rustybin/git/pull` `{apply,
//!   dry_run, prune}`, `POST /_rustybin/git/resolve` `{with, message}`.
//! - `GET`, `POST /_rustybin/git/key` `{replace}`: the deploy key.
//! - `GET /_rustybin/git/history`: the last 50 commits.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::GitError;
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const GIT_PATH: &str = "/_rustybin/git";
pub const INIT_PATH: &str = "/_rustybin/git/init";
pub const COMMIT_PATH: &str = "/_rustybin/git/commit";
pub const PUSH_PATH: &str = "/_rustybin/git/push";
pub const PULL_PATH: &str = "/_rustybin/git/pull";
pub const RESOLVE_PATH: &str = "/_rustybin/git/resolve";
pub const KEY_PATH: &str = "/_rustybin/git/key";
pub const HISTORY_PATH: &str = "/_rustybin/git/history";

fn answer(result: Result<Value, GitError>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => (e.status, Json(e.body())).into_response(),
    }
}

#[allow(clippy::result_large_err)]
fn body<T: for<'de> Deserialize<'de> + Default>(bytes: &[u8]) -> Result<T, Response> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("invalid request: {e}") })),
        )
            .into_response()
    })
}

fn operator(headers: &HeaderMap, state: &AppState) -> String {
    crate::settings::principal(headers, &state.config)
}

#[derive(Deserialize, Default)]
pub struct StatusQuery {
    fetch: Option<bool>,
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StatusQuery>,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    Json(state.git.status(&state, q.fetch.unwrap_or(false)).await).into_response()
}

async fn init(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    answer(state.git.init(&operator(&headers, &state)).await)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CommitBody {
    #[serde(default)]
    message: String,
}

async fn commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let b: CommitBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    answer(
        state
            .git
            .commit(&state, &b.message, &operator(&headers, &state))
            .await,
    )
}

async fn push(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    answer(state.git.push(&operator(&headers, &state)).await)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PullBody {
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    prune: bool,
}

async fn pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let b: PullBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    answer(
        state
            .git
            .pull(
                &state,
                b.apply,
                b.dry_run,
                b.prune,
                &operator(&headers, &state),
            )
            .await,
    )
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    #[serde(default)]
    with: String,
    #[serde(default)]
    message: String,
}

async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let b: ResolveBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    answer(
        state
            .git
            .resolve(&state, &b.with, &b.message, &operator(&headers, &state))
            .await,
    )
}

async fn get_key(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    Json(json!({ "deploy_key": state.git.public_key() })).into_response()
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct KeyBody {
    #[serde(default)]
    replace: bool,
}

async fn generate_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    let b: KeyBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    answer(
        state
            .git
            .generate_key(b.replace, &operator(&headers, &state))
            .await,
    )
}

async fn history(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::admin::require_admin(&headers, &state.config) {
        return resp;
    }
    answer(
        state
            .git
            .history()
            .await
            .map(|commits| json!({ "commits": commits })),
    )
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(GIT_PATH, get(status))
        .route(INIT_PATH, post(init))
        .route(COMMIT_PATH, post(commit))
        .route(PUSH_PATH, post(push))
        .route(PULL_PATH, post(pull))
        .route(RESOLVE_PATH, post(resolve))
        .route(KEY_PATH, get(get_key).post(generate_key))
        .route(HISTORY_PATH, get(history))
}

pub fn catalog() -> Vec<Endpoint> {
    // Without a state directory the operations answer 409 (not configured):
    // the examples say so.
    vec![
        Endpoint::new(GIT_PATH, &["GET"], category::CONTROL, "Source control status (admin)")
            .description(
                "The configured repository (git.remote, git.branch, git.path, git.auth), whether git \
                 is installed and the working copy exists, the last commit, ahead and behind \
                 (?fetch=true asks the remote), the drift between the working copy's document and \
                 the instance (what a commit would record), the deploy key's public half and the \
                 timeline of operations.",
            )
            .example(Example::get("Source control status", GIT_PATH)),
        Endpoint::new(INIT_PATH, &["POST"], category::CONTROL, "Connect the repository (admin)")
            .description(
                "Clone the remote's branch into <state>/repo, or initialise an empty working copy \
                 when the remote has no such branch yet; an existing working copy is pointed at \
                 the remote.",
            )
            .example(Example::post("Connect", INIT_PATH).expect_status(409)),
        Endpoint::new(COMMIT_PATH, &["POST"], category::CONTROL, "Commit the instance's configuration (admin)")
            .description("{message}: export the instance as a bundle into the working copy and commit it.")
            .example(
                Example::post("Commit", COMMIT_PATH)
                    .json(r#"{"message": "Add the orders mock"}"#)
                    .expect_status(409),
            ),
        Endpoint::new(PUSH_PATH, &["POST"], category::CONTROL, "Push (admin)")
            .description("Push the branch; a remote with commits this instance lacks answers 409 with the conflict.")
            .example(Example::post("Push", PUSH_PATH).expect_status(409)),
        Endpoint::new(PULL_PATH, &["POST"], category::CONTROL, "Pull and apply (admin)")
            .description(
                "{apply, dry_run, prune}: fetch and fast-forward; with apply, apply the repository's \
                 document to the instance (dry_run answers the diff without touching anything, \
                 prune makes the document the whole truth). Never merges: a diverged branch \
                 answers 409 with the two ways out.",
            )
            .example(
                Example::post("Pull, dry run", PULL_PATH)
                    .json(r#"{"apply": true, "dry_run": true}"#)
                    .expect_status(409),
            ),
        Endpoint::new(RESOLVE_PATH, &["POST"], category::CONTROL, "Resolve a diverged branch (admin)")
            .description(
                "{with: instance | repository, message}: instance drops the local commits and \
                 commits and pushes this instance's configuration on top of the remote; \
                 repository takes the remote's branch and applies it with prune.",
            )
            .example(
                Example::post("Resolve with the instance's copy", RESOLVE_PATH)
                    .json(r#"{"with": "instance"}"#)
                    .expect_status(409),
            ),
        Endpoint::new(KEY_PATH, &["GET", "POST"], category::CONTROL, "The deploy key (admin)")
            .description(
                "GET shows the public half of the ed25519 deploy key; POST {replace} generates it \
                 (ssh-keygen) into <state>/repo.key.",
            )
            .example(Example::get("Show the deploy key", KEY_PATH))
            .example(Example::post("Generate the deploy key", KEY_PATH).expect_status(409)),
        Endpoint::new(HISTORY_PATH, &["GET"], category::CONTROL, "Commit history (admin)")
            .description("The last 50 commits of the working copy with their file counts.")
            .example(Example::get("History", HISTORY_PATH).expect_status(409)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    let not_ready =
        json!({ "description": "Not configured, not initialised, busy, or a conflict to resolve" });
    let admin = json!({ "description": "Admin rights required" });
    let ok = |desc: &str| json!({ "200": { "description": desc }, "401": admin, "409": not_ready, "502": { "description": "git failed (the remote, the network)" } });
    json!({
        GIT_PATH: { "get": {
            "tags": tags, "summary": "Source control status", "operationId": "gitStatus",
            "parameters": [{ "name": "fetch", "in": "query", "schema": { "type": "boolean" }, "description": "Ask the remote for ahead and behind" }],
            "responses": { "200": { "description": "{git, remote, branch, path, auth, initialised, head, ahead, behind, drift, deploy_key, timeline}" }, "401": admin }
        } },
        INIT_PATH: { "post": { "tags": tags, "summary": "Connect the repository", "operationId": "gitInit", "responses": ok("{initialised, fresh, remote_branch, head}") } },
        COMMIT_PATH: { "post": { "tags": tags, "summary": "Commit the instance's configuration", "operationId": "gitCommit",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["message"], "properties": { "message": { "type": "string", "maxLength": super::MAX_MESSAGE } } } } } },
            "responses": ok("{committed, files, head}") } },
        PUSH_PATH: { "post": { "tags": tags, "summary": "Push", "operationId": "gitPush", "responses": ok("{pushed, head}") } },
        PULL_PATH: { "post": { "tags": tags, "summary": "Pull and apply", "operationId": "gitPull",
            "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": {
                "apply": { "type": "boolean", "default": false }, "dry_run": { "type": "boolean", "default": false }, "prune": { "type": "boolean", "default": false } } } } } },
            "responses": ok("{fast_forwarded, ahead, behind, head, applied}") } },
        RESOLVE_PATH: { "post": { "tags": tags, "summary": "Resolve a diverged branch", "operationId": "gitResolve",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["with"], "properties": {
                "with": { "type": "string", "enum": ["instance", "repository"] }, "message": { "type": "string" } } } } } },
            "responses": ok("{resolved, commit, push} or {resolved, applied, head}") } },
        KEY_PATH: {
            "get": { "tags": tags, "summary": "The deploy key's public half", "operationId": "gitKey", "responses": { "200": { "description": "{deploy_key: {public, path} | null}" }, "401": admin } },
            "post": { "tags": tags, "summary": "Generate the deploy key", "operationId": "gitGenerateKey",
                "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "replace": { "type": "boolean", "default": false } } } } } },
                "responses": ok("{public, path, generated}") }
        },
        HISTORY_PATH: { "get": { "tags": tags, "summary": "Commit history", "operationId": "gitHistory", "responses": ok("{commits: [{sha, short, author, at, subject, stat}]}") } }
    })
}
