//! The source control routes (admin, audited, serialised by one lock):
//!
//! - `GET /_rustybin/git?fetch=true`: status (remote, branch, head, ahead
//!   and behind, the drift between the working copy and the instance, the
//!   deploy key, `verification`, the timeline).
//! - With `git.verify` on, every route that takes the repository's side
//!   (pull, resolve with `repository`, branch, init's first checkout)
//!   answers `409` with `reason: unverified`, `commit` and `why` for a
//!   commit no allowed signer signed (`verify.rs`).
//! - `POST /_rustybin/git/init`: clone, or initialise for an empty remote.
//! - `POST /_rustybin/git/commit` `{message, checkpoints}`: export into the
//!   working copy and commit, with the named checkpoints.
//! - `POST /_rustybin/git/push`, `POST /_rustybin/git/pull` `{apply,
//!   dry_run, prune}`, `POST /_rustybin/git/resolve` `{with, message,
//!   branch, dry_run}`: the ways out of a diverged branch.
//! - `GET`, `POST /_rustybin/git/key` `{replace}`: the deploy key.
//! - `GET /_rustybin/git/history`: the last 50 commits.
//! - `GET /_rustybin/git/branches`: the remote's branches; `POST
//!   /_rustybin/git/branch` `{branch, create, apply, dry_run, prune,
//!   message}`: switch to one (with a preview) or start one.
//! - `GET`, `POST /_rustybin/git/signing-key` `{replace}`: the key commits
//!   are signed with (the public half and its fingerprint only).

use axum::body::Bytes;
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
pub const WEBHOOK_PATH: &str = "/_rustybin/git/webhook";
pub const RESOLVE_PATH: &str = "/_rustybin/git/resolve";
pub const KEY_PATH: &str = "/_rustybin/git/key";
pub const HISTORY_PATH: &str = "/_rustybin/git/history";
pub const BRANCHES_PATH: &str = "/_rustybin/git/branches";
pub const BRANCH_PATH: &str = "/_rustybin/git/branch";
pub const SIGNING_KEY_PATH: &str = "/_rustybin/git/signing-key";

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

async fn status(State(state): State<AppState>, Query(q): Query<StatusQuery>) -> Response {
    Json(state.git.status(&state, q.fetch.unwrap_or(false)).await).into_response()
}

async fn init(State(state): State<AppState>, headers: HeaderMap) -> Response {
    answer(state.git.init(&operator(&headers, &state)).await)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CommitBody {
    #[serde(default)]
    message: String,
    /// Local checkpoints to write next to the document (default none).
    #[serde(default)]
    checkpoints: Vec<String>,
}

async fn commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    let b: CommitBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    answer(
        state
            .git
            .commit_with(
                &state,
                &b.message,
                &b.checkpoints,
                &operator(&headers, &state),
            )
            .await,
    )
}

async fn push(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = state.git.push(&operator(&headers, &state)).await;
    answer(explained(&state, result).await)
}

/// A diverged branch's `409` with what differs.
async fn explained(state: &AppState, result: Result<Value, GitError>) -> Result<Value, GitError> {
    match result {
        Err(e) => Err(state.git.explain(state, e).await),
        ok => ok,
    }
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
    let b: PullBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let result = state
        .git
        .pull(
            &state,
            b.apply,
            b.dry_run,
            b.prune,
            &operator(&headers, &state),
        )
        .await;
    answer(explained(&state, result).await)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    #[serde(default)]
    with: String,
    #[serde(default)]
    message: String,
    /// The new branch of `pull_request` (a name is made when absent).
    #[serde(default)]
    branch: Option<String>,
    /// What the way out would do, nothing changed.
    #[serde(default)]
    dry_run: bool,
}

async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    let b: ResolveBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let req = super::Resolve {
        with: b.with,
        message: b.message,
        branch: b.branch,
        dry_run: b.dry_run,
    };
    answer(
        state
            .git
            .resolve(&state, &req, &operator(&headers, &state))
            .await,
    )
}

async fn branches(State(state): State<AppState>) -> Response {
    answer(state.git.branches().await)
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchBody {
    #[serde(default)]
    branch: String,
    /// Start the branch from the instance (it must not exist on the remote).
    #[serde(default)]
    create: bool,
    /// Apply the branch's document after a switch (default true).
    #[serde(default = "default_true")]
    apply: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    prune: bool,
    #[serde(default)]
    message: String,
}

impl Default for BranchBody {
    fn default() -> Self {
        Self {
            branch: String::new(),
            create: false,
            apply: true,
            dry_run: false,
            prune: false,
            message: String::new(),
        }
    }
}

async fn branch(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    let b: BranchBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let req = super::BranchRequest {
        branch: b.branch,
        create: b.create,
        apply: b.apply,
        dry_run: b.dry_run,
        prune: b.prune,
        message: b.message,
    };
    answer(
        state
            .git
            .switch_branch(&state, &req, &operator(&headers, &state))
            .await,
    )
}

async fn get_signing_key(State(state): State<AppState>) -> Response {
    Json(json!({ "signing": state.git.signing() })).into_response()
}

async fn generate_signing_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Response {
    let b: KeyBody = match body(&bytes) {
        Ok(b) => b,
        Err(r) => return r,
    };
    answer(
        state
            .git
            .generate_signing_key(b.replace, &operator(&headers, &state))
            .await
            .map(|signing| json!({ "signing": signing })),
    )
}

async fn get_key(State(state): State<AppState>) -> Response {
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

async fn history(State(state): State<AppState>) -> Response {
    answer(
        state
            .git
            .history()
            .await
            .map(|commits| json!({ "commits": commits })),
    )
}

/// Did the caller hold the secret? `X-Hub-Signature-256: sha256=<hex>` over
/// the body (GitHub, Gitea, Forgejo, Bitbucket), or `X-Gitlab-Token`.
fn signed(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(given) = header("x-hub-signature-256") {
        let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let want: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        return crate::control_auth::constant_time_eq(given, &format!("sha256={want}"));
    }
    header("x-gitlab-token").is_some_and(|t| crate::control_auth::constant_time_eq(t, secret))
}

/// `POST /_rustybin/git/webhook`: a push on the configured branch pulls and
/// applies, as the automatic mode does, through the same checks. Open, since
/// a git host holds no control credential: the signature is the credential.
/// Off (`404`) until `git.webhook_secret` is set.
async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(secret) = state.git.webhook_secret() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no webhook here: set git.webhook_secret" })),
        )
            .into_response();
    };
    if !signed(&secret, &headers, &body) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "the signature does not match" })),
        )
            .into_response();
    }
    let pushed = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v["ref"].as_str().map(str::to_string));
    let branch = format!("refs/heads/{}", state.git.branch());
    if pushed.is_some_and(|r| r != branch) {
        return Json(json!({ "pulled": false, "reason": "another branch" })).into_response();
    }
    if headers
        .get("x-github-event")
        .is_some_and(|e| e.as_bytes() == b"ping")
    {
        return Json(json!({ "pulled": false, "reason": "ping" })).into_response();
    }
    super::SourceControl::queue_pull(&state);
    (StatusCode::ACCEPTED, Json(json!({ "queued": true }))).into_response()
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(GIT_PATH, get(status))
        .route(INIT_PATH, post(init))
        .route(COMMIT_PATH, post(commit))
        .route(PUSH_PATH, post(push))
        .route(PULL_PATH, post(pull))
        .route(WEBHOOK_PATH, post(webhook))
        .route(RESOLVE_PATH, post(resolve))
        .route(KEY_PATH, get(get_key).post(generate_key))
        .route(HISTORY_PATH, get(history))
        .route(BRANCHES_PATH, get(branches))
        .route(BRANCH_PATH, post(branch))
        .route(
            SIGNING_KEY_PATH,
            get(get_signing_key).post(generate_signing_key),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    // Without a state directory the operations answer 409 (not configured):
    // the examples say so.
    vec![
        Endpoint::new(WEBHOOK_PATH, &["POST"], category::CONTROL, "A signed push from a git host: pull and apply at once")
            .open()
            .description(
                "For a git host, which holds no control credential: the signature is the \
                 credential (X-Hub-Signature-256 over the body, or X-Gitlab-Token). 404 until \
                 git.webhook_secret is set. A push to another branch is ignored, and a burst \
                 costs at most two pulls.",
            )
            .example(Example::post("A push", WEBHOOK_PATH).json("{}").expect_status(404)),
        Endpoint::new(GIT_PATH, &["GET"], category::CONTROL, "Source control status (admin)")
.permission(crate::rbac::Permission::GitRead)
            .description(
                "The configured repository (git.remote, git.branch, git.path, git.auth), whether git \
                 is installed and the working copy exists, the last commit, ahead and behind \
                 (?fetch=true asks the remote), the drift between the working copy's document and \
                 the instance (what a commit would record), the deploy key's public half, \
                 verification (whether pulled commits are verified against allowed signers: \
                 the signers' names and fingerprints, the head trusted when it was turned on, \
                 the last verified head, the last refusal) and the timeline of operations.",
            )
            .example(Example::get("Source control status", GIT_PATH)),
        Endpoint::new(INIT_PATH, &["POST"], category::CONTROL, "Connect the repository (admin)")
.permission(crate::rbac::Permission::GitWrite)
            .description(
                "Clone the remote's branch into <state>/repo, or initialise an empty working copy \
                 when the remote has no such branch yet; an existing working copy is pointed at \
                 the remote.",
            )
            .example(Example::post("Connect", INIT_PATH).expect_status(409)),
        Endpoint::new(COMMIT_PATH, &["POST"], category::CONTROL, "Commit the instance's configuration (admin)")
.permission(crate::rbac::Permission::GitWrite)
            .description(
                "{message, checkpoints}: export the instance as a bundle into the working copy and \
                 commit it; checkpoints names local checkpoints to write to \
                 <git.path>/checkpoints/<name>.json in the same commit (default none, never by \
                 the automatic mode).",
            )
            .example(
                Example::post("Commit", COMMIT_PATH)
                    .json(r#"{"message": "Add the orders mock"}"#)
                    .expect_status(409),
            ),
        Endpoint::new(PUSH_PATH, &["POST"], category::CONTROL, "Push (admin)")
.permission(crate::rbac::Permission::GitWrite)
            .description("Push the branch; a remote with commits this instance lacks answers 409 with the conflict.")
            .example(Example::post("Push", PUSH_PATH).expect_status(409)),
        Endpoint::new(PULL_PATH, &["POST"], category::CONTROL, "Pull and apply (admin)")
.permission(crate::rbac::Permission::GitWrite)
            .description(
                "{apply, dry_run, prune}: fetch and fast-forward; with apply, apply the repository's \
                 document to the instance (dry_run answers the diff without touching anything, \
                 prune makes the document the whole truth). Never merges: a diverged branch \
                 answers 409 with what differs (divergence) and the ways out (options). With \
                 the git.verify setting on, every commit since the last verified head needs an \
                 allowed signer's SSH signature (a merge commit passes unsigned only as the \
                 clean merge of two verified parents): otherwise 409 with reason unverified, \
                 the commit and why, nothing applied and the working copy not moved. The \
                 same holds for resolve with repository, a switch of branch and a checkpoint \
                 from the repository.",
            )
            .example(
                Example::post("Pull, dry run", PULL_PATH)
                    .json(r#"{"apply": true, "dry_run": true}"#)
                    .expect_status(409),
            ),
        Endpoint::new(RESOLVE_PATH, &["POST"], category::CONTROL, "Resolve a diverged branch (admin)")
.permission(crate::rbac::Permission::GitWrite)
            .description(
                "{with: instance | repository | pull_request, message, branch, dry_run}: the ways \
                 out of a diverged branch, each a person's call (the automatic mode never \
                 resolves). instance drops the local commits, commits this instance's \
                 configuration on top of the remote head and pushes (never a forced push); \
                 repository takes the remote's branch and applies it with prune; pull_request \
                 pushes this instance's side to a new branch (branch, or a made name) for a \
                 pull request at the git host and applies nothing. dry_run answers what the \
                 way out would change, validated like the apply.",
            )
            .example(
                Example::post("Resolve with the instance's copy", RESOLVE_PATH)
                    .json(r#"{"with": "instance"}"#)
                    .expect_status(409),
            ),
        Endpoint::new(KEY_PATH, &["GET", "POST"], category::CONTROL, "The deploy key (admin)")
.permission_for(&["GET"], crate::rbac::Permission::GitRead).permission_for(&["POST"], crate::rbac::Permission::GitWrite)
            .description(
                "GET shows the public half of the ed25519 deploy key; POST {replace} generates it \
                 (ssh-keygen) into <state>/repo.key.",
            )
            .example(Example::get("Show the deploy key", KEY_PATH))
            .example(Example::post("Generate the deploy key", KEY_PATH).expect_status(409)),
        Endpoint::new(HISTORY_PATH, &["GET"], category::CONTROL, "Commit history (admin)")
.permission(crate::rbac::Permission::GitRead)
            .description("The last 50 commits of the working copy with their file counts.")
            .example(Example::get("History", HISTORY_PATH).expect_status(409)),
        Endpoint::new(BRANCHES_PATH, &["GET"], category::CONTROL, "The remote's branches (admin)")
            .permission(crate::rbac::Permission::GitRead)
            .description(
                "The branches of the remote (git ls-remote through the outbound policy, 200 at \
                 most) with their commits, and which one this instance is on (the git.branch \
                 setting).",
            )
            .example(Example::get("Branches", BRANCHES_PATH).expect_status(409)),
        Endpoint::new(BRANCH_PATH, &["POST"], category::CONTROL, "Switch branch, or start one (admin)")
            .permission(crate::rbac::Permission::GitWrite)
            .description(
                "{branch, create, apply, dry_run, prune, message}: move this instance to another \
                 branch of the remote (a branch per environment). dry_run answers what applying \
                 that branch's document would change, validated like the apply, and changes \
                 nothing; without it the git.branch setting is stored, the working copy takes \
                 the branch and its document is applied (apply: false moves the branch only). \
                 create: true starts a branch the remote does not have from this instance's \
                 current configuration: committed and pushed. A document never names a branch.",
            )
            .example(
                Example::post("Preview a switch", BRANCH_PATH)
                    .json(r#"{"branch": "staging", "dry_run": true}"#)
                    .expect_status(409),
            ),
        Endpoint::new(SIGNING_KEY_PATH, &["GET", "POST"], category::CONTROL, "The commit signing key (admin)")
            .permission_for(&["GET"], crate::rbac::Permission::GitRead)
            .permission_for(&["POST"], crate::rbac::Permission::GitWrite)
            .description(
                "GET answers whether commits are signed (git.sign), the signing key's public half \
                 and its SHA256 fingerprint to register at the git host as a signing key, and \
                 why signing cannot work when it is on and cannot; never the private half. POST \
                 {replace} generates an ed25519 key (ssh-keygen) and keeps it as the secret \
                 git.signing_key. Never the deploy key.",
            )
            .example(Example::get("Show the signing key", SIGNING_KEY_PATH))
            .example(Example::post("Generate the signing key", SIGNING_KEY_PATH).expect_status(409)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Control Plane"]);
    let not_ready = json!({ "description": "Not configured, not initialised, busy, a conflict to resolve, or (with git.verify on) a commit of the repository's side that no allowed signer signed: {error, reason: unverified, commit, why}, nothing applied" });
    let admin = json!({ "description": "Admin rights required" });
    let ok = |desc: &str| json!({ "200": { "description": desc }, "401": admin, "409": not_ready, "502": { "description": "git failed (the remote, the network)" } });
    json!({
        GIT_PATH: { "get": {
            "tags": tags, "summary": "Source control status", "operationId": "gitStatus",
            "parameters": [{ "name": "fetch", "in": "query", "schema": { "type": "boolean" }, "description": "Ask the remote for ahead and behind" }],
            "responses": { "200": { "description": "{git, remote, branch, path, auth, initialised, head, ahead, behind, drift, divergence (what differs and the ways out, when diverged), deploy_key, signing, verification: {enabled, signers: [{name, key_type, fingerprint}], own_key, since, trusted, verified_head, last_refusal, problem}, timeline}" }, "401": admin }
        } },
        INIT_PATH: { "post": { "tags": tags, "summary": "Connect the repository", "operationId": "gitInit", "responses": ok("{initialised, fresh, remote_branch, head}") } },
        COMMIT_PATH: { "post": { "tags": tags, "summary": "Commit the instance's configuration", "operationId": "gitCommit",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["message"], "properties": {
                "message": { "type": "string", "maxLength": super::MAX_MESSAGE },
                "checkpoints": { "type": "array", "items": { "type": "string" }, "maxItems": crate::checkpoints::MAX_CHECKPOINTS, "default": [],
                    "description": "Local checkpoints written to <git.path>/checkpoints/<name>.json in this commit (mock data: an explicit commit only)" } } } } } },
            "responses": ok("{committed, files, checkpoints, head}") } },
        PUSH_PATH: { "post": { "tags": tags, "summary": "Push", "operationId": "gitPush", "responses": ok("{pushed, head}") } },
        WEBHOOK_PATH: { "post": { "tags": tags, "summary": "A signed push from a git host", "operationId": "gitWebhook", "responses": { "202": { "description": "A pull is queued" }, "200": { "description": "Another branch, or a ping" }, "401": { "description": "The signature does not match" }, "404": { "description": "git.webhook_secret is not set" } } } },
        PULL_PATH: { "post": { "tags": tags, "summary": "Pull and apply", "operationId": "gitPull",
            "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": {
                "apply": { "type": "boolean", "default": false }, "dry_run": { "type": "boolean", "default": false }, "prune": { "type": "boolean", "default": false } } } } } },
            "responses": ok("{fast_forwarded, ahead, behind, head, applied, checkpoints (the repository's, by name)}") } },
        RESOLVE_PATH: { "post": { "tags": tags, "summary": "Resolve a diverged branch", "operationId": "gitResolve",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["with"], "properties": {
                "with": { "type": "string", "enum": ["instance", "repository", "pull_request"] },
                "message": { "type": "string", "description": "The commit message when the instance's side is committed" },
                "branch": { "type": "string", "maxLength": 128, "description": "pull_request: the new branch (a name is made when absent)" },
                "dry_run": { "type": "boolean", "default": false, "description": "What the way out would change; nothing is changed" } } } } } },
            "responses": ok("{resolved, commit, push}, {resolved, applied, head} or {resolved, branch, base, pushed}; with dry_run {dry_run, would_resolve, ...}") } },
        KEY_PATH: {
            "get": { "tags": tags, "summary": "The deploy key's public half", "operationId": "gitKey", "responses": { "200": { "description": "{deploy_key: {public, path} | null}" }, "401": admin } },
            "post": { "tags": tags, "summary": "Generate the deploy key", "operationId": "gitGenerateKey",
                "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "replace": { "type": "boolean", "default": false } } } } } },
                "responses": ok("{public, path, generated}") }
        },
        HISTORY_PATH: { "get": { "tags": tags, "summary": "Commit history", "operationId": "gitHistory", "responses": ok("{commits: [{sha, short, author, at, subject, stat}]}") } },
        BRANCHES_PATH: { "get": { "tags": tags, "summary": "The remote's branches", "operationId": "gitBranches", "responses": ok("{branch, on_remote, branches: [{name, sha, short, current}], truncated, unlisted}") } },
        BRANCH_PATH: { "post": { "tags": tags, "summary": "Switch branch, or start one", "operationId": "gitBranch",
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["branch"], "properties": {
                "branch": { "type": "string", "maxLength": 128 },
                "create": { "type": "boolean", "default": false, "description": "Start the branch from this instance (the remote must not have it)" },
                "apply": { "type": "boolean", "default": true, "description": "Apply the branch's document after the switch" },
                "dry_run": { "type": "boolean", "default": false, "description": "The preview: what applying the branch's document would change" },
                "prune": { "type": "boolean", "default": false },
                "message": { "type": "string", "description": "The first commit's message of a new branch" } } } } } },
            "responses": ok("{switched, branch, from, head, applied}, {created, branch, from, commit, push} or, with dry_run, {dry_run, branch, from, applied}") } },
        SIGNING_KEY_PATH: {
            "get": { "tags": tags, "summary": "The commit signing key's public half", "operationId": "gitSigningKey", "responses": { "200": { "description": "{signing: {enabled, key_set, public, fingerprint, key_type, problem}}" }, "401": admin } },
            "post": { "tags": tags, "summary": "Generate the commit signing key", "operationId": "gitGenerateSigningKey",
                "requestBody": { "content": { "application/json": { "schema": { "type": "object", "properties": { "replace": { "type": "boolean", "default": false } } } } } },
                "responses": ok("{signing: {enabled, key_set, public, fingerprint, key_type, problem, generated}}") }
        }
    })
}
