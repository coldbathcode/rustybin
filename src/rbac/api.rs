//! The routes of users, roles and your own account (step 09):
//! `/_rustybin/users*`, `/_rustybin/roles*`, `/_rustybin/me` and
//! `/_rustybin/me/sessions*`. Thin handlers over [`super::service`], which
//! the management tools call too. The password routes (`PUT
//! /_rustybin/me/password`, the 0.3 `PUT /_rustybin/password`) and the
//! invitation's `POST /_rustybin/join` are the sign-in's (`crate::login`).
//!
//! Users exist only with the password sign-in (`404` otherwise); roles
//! work in every mode (a control JWT's `rb_roles` may name a custom role).

use axum::extract::{Path as RoutePath, State};
use axum::http::{Extensions, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::service::{self, Invite};
use super::users::{Source, UserPatch};
use super::{CustomRole, Permission};
use crate::catalog::{category, Endpoint, Example};
use crate::mocks::service::SvcResult;
use crate::state::AppState;

pub const USERS_PATH: &str = "/_rustybin/users";
pub const ROLES_PATH: &str = "/_rustybin/roles";
pub const ME_PATH: &str = "/_rustybin/me";
pub const ME_SESSIONS_PATH: &str = "/_rustybin/me/sessions";

fn answer(r: SvcResult<Value>) -> Response {
    answer_with(r, StatusCode::OK)
}

fn answer_with(r: SvcResult<Value>, ok: StatusCode) -> Response {
    match r {
        Ok(v) => {
            let mut resp = (ok, Json(v)).into_response();
            resp.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            resp
        }
        Err(e) => e.into_response(),
    }
}

fn bad_body(e: axum::extract::rejection::JsonRejection, expected: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": format!("expected {expected}: {}", e.body_text()),
            "reason": "invalid_body",
        })),
    )
        .into_response()
}

fn base_url(state: &AppState, headers: &HeaderMap, extensions: &Extensions, uri: &Uri) -> String {
    crate::session::request_origin(headers, extensions, uri, &state.config).base_url()
}

/// A field that may be absent, `null` or a value.
fn nullable_string(v: Option<&Value>) -> Result<Option<Option<String>>, String> {
    match v {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.clone()))),
        Some(_) => Err("expected a string or null".into()),
    }
}

fn string_list(v: Option<&Value>, what: &str) -> Result<Option<Vec<String>>, String> {
    match v {
        None => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{what}: expected strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(Value::String(s)) => Ok(Some(
            s.split([',', ' '])
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect(),
        )),
        Some(_) => Err(format!("{what}: expected a list of names")),
    }
}

fn invalid(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into(), "reason": "invalid" })),
    )
        .into_response()
}

/// A user change from a JSON object (`PATCH /_rustybin/users/{id}`, the
/// `update_user` tool).
pub fn patch_of(body: &Value) -> Result<UserPatch, String> {
    let Some(obj) = body.as_object() else {
        return Err("expected a JSON object".into());
    };
    for k in obj.keys() {
        if !matches!(
            k.as_str(),
            "roles" | "pinned_roles" | "display_name" | "email" | "disabled"
        ) {
            return Err(format!(
                "{} cannot be changed here (roles, pinned_roles, display_name, email, disabled)",
                crate::logging::printable(k, 40)
            ));
        }
    }
    let display_name = match obj.get("display_name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err("display_name: expected a string".into()),
    };
    let disabled = match obj.get("disabled") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err("disabled: expected true or false".into()),
    };
    Ok(UserPatch {
        display_name,
        email: nullable_string(obj.get("email")).map_err(|e| format!("email: {e}"))?,
        roles: string_list(obj.get("roles"), "roles")?,
        disabled,
        pinned_roles: string_list(obj.get("pinned_roles"), "pinned_roles")?,
    })
}

/// A new user from a JSON object (`POST /_rustybin/users`, the
/// `invite_user` tool).
pub fn invite_of(body: &Value) -> Result<Invite, String> {
    let Some(obj) = body.as_object() else {
        return Err("expected a JSON object".into());
    };
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or("name: the sign-in name is required")?
        .to_string();
    let opt = |k: &str| -> Result<Option<String>, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{k}: expected a string")),
        }
    };
    let sso = match obj.get("sso") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("sso: expected true or false".into()),
    };
    Ok(Invite {
        name,
        display_name: opt("display_name")?,
        email: opt("email")?,
        roles: string_list(obj.get("roles"), "roles")?.unwrap_or_default(),
        sso,
    })
}

/// A custom role from a JSON object; `name` from the path wins (and must
/// agree with the body's).
pub fn role_of(body: &Value, path_name: Option<&str>) -> Result<CustomRole, String> {
    let Some(obj) = body.as_object() else {
        return Err("expected a JSON object".into());
    };
    let body_name = obj.get("name").and_then(Value::as_str);
    let name = match (path_name, body_name) {
        (Some(p), Some(b)) if p != b => {
            return Err(format!("name: the body says {b}, the path {p}"));
        }
        (Some(p), _) => p.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => return Err("name: the role's name is required".into()),
    };
    let description = match obj.get("description") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err("description: expected a string".into()),
    };
    Ok(CustomRole {
        name,
        description,
        permissions: string_list(obj.get("permissions"), "permissions")?.unwrap_or_default(),
    })
}

// ── Users ───────────────────────────────────────────────────────────

async fn list_users(State(state): State<AppState>) -> Response {
    answer(service::list_users(&state))
}

async fn invite_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return bad_body(e, "{ \"name\": \"...\", \"roles\": [\"...\"] }"),
    };
    let invite = match invite_of(&body) {
        Ok(i) => i,
        Err(e) => return invalid(e),
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let base = base_url(&state, &headers, &extensions, &uri);
    answer_with(
        service::invite_user(&state, &caller, invite, &base, Source::Password),
        StatusCode::CREATED,
    )
}

async fn get_user(State(state): State<AppState>, RoutePath(id): RoutePath<String>) -> Response {
    answer(service::get_user(&state, &id))
}

async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(id): RoutePath<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return bad_body(
                e,
                "{ \"roles\"?, \"display_name\"?, \"email\"?, \"disabled\"? }",
            )
        }
    };
    let patch = match patch_of(&body) {
        Ok(p) => p,
        Err(e) => return invalid(e),
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::update_user(&state, &caller, &id, patch))
}

async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(id): RoutePath<String>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::delete_user(&state, &caller, &id))
}

async fn reinvite_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    RoutePath(id): RoutePath<String>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let base = base_url(&state, &headers, &extensions, &uri);
    answer_with(
        service::reinvite_user(&state, &caller, &id, &base),
        StatusCode::CREATED,
    )
}

async fn user_sessions(
    State(state): State<AppState>,
    RoutePath(id): RoutePath<String>,
) -> Response {
    answer(service::user_sessions(&state, &id))
}

async fn end_user_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(id): RoutePath<String>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::end_user_sessions(&state, &caller, &id))
}

// ── Roles ───────────────────────────────────────────────────────────

async fn list_roles(State(state): State<AppState>) -> Response {
    answer(Ok(service::list_roles(&state)))
}

async fn create_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return bad_body(e, "{ \"name\": \"...\", \"permissions\": [\"...\"] }"),
    };
    let role = match role_of(&body, None) {
        Ok(r) => r,
        Err(e) => return invalid(e),
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer_with(
        service::save_role(&state, &caller, role, true),
        StatusCode::CREATED,
    )
}

async fn get_role(State(state): State<AppState>, RoutePath(name): RoutePath<String>) -> Response {
    answer(service::get_role(&state, &name))
}

async fn put_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(name): RoutePath<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return bad_body(e, "{ \"description\"?, \"permissions\": [\"...\"] }"),
    };
    let role = match role_of(&body, Some(&name)) {
        Ok(r) => r,
        Err(e) => return invalid(e),
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::save_role(&state, &caller, role, false))
}

async fn delete_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(name): RoutePath<String>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::delete_role(&state, &caller, &name))
}

// ── Your own account ────────────────────────────────────────────────

async fn me(State(state): State<AppState>, headers: HeaderMap, extensions: Extensions) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::me(&state, &caller))
}

#[derive(Deserialize)]
struct MeBody {
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

async fn update_me(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<MeBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return bad_body(e, "{ \"display_name\"?, \"email\"? }"),
    };
    for k in body.fields.keys() {
        if !matches!(k.as_str(), "display_name" | "email") {
            return invalid(format!(
                "{} cannot be changed here (display_name, email; the password: PUT /_rustybin/me/password)",
                crate::logging::printable(k, 40)
            ));
        }
    }
    let display_name = match body.fields.get("display_name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return invalid("display_name: expected a string"),
    };
    let email = match nullable_string(body.fields.get("email")) {
        Ok(e) => e,
        Err(e) => return invalid(format!("email: {e}")),
    };
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::update_me(&state, &caller, display_name, email))
}

async fn my_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    let current = crate::login::current_session(&headers, &state.config);
    answer(service::my_sessions(&state, &caller, current))
}

async fn end_my_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::end_my_sessions(&state, &caller))
}

async fn end_my_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    RoutePath(id): RoutePath<String>,
) -> Response {
    let caller = crate::control_auth::caller_of(&state, &headers, &extensions);
    answer(service::end_my_session(&state, &caller, &id))
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(USERS_PATH, get(list_users).post(invite_user))
        .route(
            "/_rustybin/users/{id}",
            get(get_user).patch(update_user).delete(delete_user),
        )
        .route("/_rustybin/users/{id}/invite", post(reinvite_user))
        .route(
            "/_rustybin/users/{id}/sessions",
            get(user_sessions).delete(end_user_sessions),
        )
        .route(ROLES_PATH, get(list_roles).post(create_role))
        .route(
            "/_rustybin/roles/{name}",
            get(get_role).put(put_role).delete(delete_role),
        )
        .route(ME_PATH, get(me).patch(update_me))
        .route(ME_SESSIONS_PATH, get(my_sessions).delete(end_my_sessions))
        .route("/_rustybin/me/sessions/{id}", delete(end_my_session))
}

pub fn catalog() -> Vec<Endpoint> {
    use Permission as P;
    vec![
        Endpoint::new(
            USERS_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Users: list them, or POST to invite one (an invitation link, shown once)",
        )
        .permission_for(&["GET"], P::UsersRead)
        .permission_for(&["POST"], P::UsersManage)
        .description(
            "Password sign-in only (404 otherwise). POST creates the user invited (disabled \
             until they accept) and answers a one-time link /ui/#/join/<token>, valid 72 hours, \
             kept only as a SHA-256; at most 100 invitations are open. A caller grants only \
             roles whose permissions it holds.",
        )
        .example(Example::get("Users", USERS_PATH).expect_status(404))
        .example(
            Example::post("Invite a user", USERS_PATH)
                .json(r#"{"name":"alice","display_name":"Alice","roles":["editor"]}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/users/{id}",
            &["GET", "PATCH", "DELETE"],
            category::CONTROL,
            "One user (by id or sign-in name): read, change roles, display name, email or disabled, delete",
        )
        .permission_for(&["GET"], P::UsersRead)
        .permission_for(&["PATCH", "DELETE"], P::UsersManage)
        .description(
            "The guard rails: the last enabled admin is never disabled, demoted or deleted \
             (409 last_admin); nobody changes their own roles (403 own_roles); the first admin \
             is never deleted (409 first_admin, disable it instead). Disabling ends the user's \
             sessions.",
        )
        .example(Example::get("A user", "/_rustybin/users/alice").expect_status(404))
        .example(
            Example::new("Change a user's roles", "PATCH", "/_rustybin/users/alice")
                .json(r#"{"roles":["tester"]}"#)
                .expect_status(404),
        )
        .example(Example::delete("Delete a user", "/_rustybin/users/alice").expect_status(404)),
        Endpoint::new(
            "/_rustybin/users/{id}/invite",
            &["POST"],
            category::CONTROL,
            "A new invitation link for a user: a password reset once they joined",
        )
        .permission(P::UsersManage)
        .description(
            "Replaces the user's open invitation. A reset leaves the old password working until \
             the link is accepted; accepting ends the user's sessions.",
        )
        .example(
            Example::post("Reset a password", "/_rustybin/users/alice/invite").expect_status(404),
        ),
        Endpoint::new(
            "/_rustybin/users/{id}/sessions",
            &["GET", "DELETE"],
            category::CONTROL,
            "A user's sign-in sessions: list them, or DELETE to sign the user out everywhere",
        )
        .permission_for(&["GET"], P::UsersRead)
        .permission_for(&["DELETE"], P::UsersManage)
        .example(
            Example::get("A user's sessions", "/_rustybin/users/alice/sessions").expect_status(404),
        ),
        Endpoint::new(
            ROLES_PATH,
            &["GET", "POST"],
            category::CONTROL,
            "Roles: the built-in and custom roles with the permission table, or POST a custom role",
        )
        .permission_for(&["GET"], P::UsersRead)
        .permission_for(&["POST"], P::UsersManage)
        .description(
            "Every mode (a control JWT's rb_roles may name a custom role). A role that grants \
             users.manage or settings.security is privileged. At most 50 custom roles; a \
             caller saves only roles within its own permissions.",
        )
        .example(Example::get("Roles", ROLES_PATH))
        .example(
            Example::post("A custom role", ROLES_PATH)
                .json(r#"{"name":"catalog-reader","description":"Reads mocks and traffic","permissions":["mocks.read","traffic.read"]}"#),
        ),
        Endpoint::new(
            "/_rustybin/roles/{name}",
            &["GET", "PUT", "DELETE"],
            category::CONTROL,
            "One role: read it and who holds it, PUT to create or change a custom role, DELETE one nobody holds",
        )
        .permission_for(&["GET"], P::UsersRead)
        .permission_for(&["PUT", "DELETE"], P::UsersManage)
        .description(
            "Built-in roles never change (400 built_in); a role somebody holds is not deleted \
             (409 role_in_use).",
        )
        .example(Example::get("A role", "/_rustybin/roles/editor"))
        .example(
            Example::new("Change a custom role", "PUT", "/_rustybin/roles/catalog-reader")
                .json(r#"{"description":"Reads mocks","permissions":["mocks.read"]}"#),
        )
        .example(Example::delete("Delete a custom role", "/_rustybin/roles/catalog-reader")),
        Endpoint::new(
            ME_PATH,
            &["GET", "PATCH"],
            category::CONTROL,
            "Your own account: who you are, your roles and permissions; PATCH your display name and email",
        )
        .authenticated(&[])
        .description(
            "Any signed-in user (400 not_a_user for the admin token or a control JWT). The \
             password changes with PUT /_rustybin/me/password.",
        )
        .example(Example::get("My account", ME_PATH).expect_status(404))
        .example(
            Example::new("My display name", "PATCH", ME_PATH)
                .json(r#"{"display_name":"Alice Doe"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            ME_SESSIONS_PATH,
            &["GET", "DELETE"],
            category::CONTROL,
            "Your own sign-in sessions: list them, or DELETE to sign yourself out everywhere",
        )
        .authenticated(&[])
        .example(Example::get("My sessions", ME_SESSIONS_PATH).expect_status(404)),
        Endpoint::new(
            "/_rustybin/me/sessions/{id}",
            &["DELETE"],
            category::CONTROL,
            "End one of your own sign-in sessions",
        )
        .authenticated(&[])
        .example(
            Example::delete(
                "End my session",
                "/_rustybin/me/sessions/0123456789abcdef0123456789abcdef",
            )
            .expect_status(404),
        ),
    ]
}

pub fn openapi_paths() -> Value {
    let user = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "usr_ plus 16 hex" },
            "name": { "type": "string", "description": "The sign-in name" },
            "display_name": { "type": "string" },
            "email": { "type": ["string", "null"] },
            "roles": { "type": "array", "items": { "type": "string" } },
            "disabled": { "type": "boolean" },
            "status": { "type": "string", "enum": ["active", "invited", "disabled"] },
            "source": { "type": "string", "enum": ["password", "sso", "document"] },
            "created_at": { "type": "string", "format": "date-time" },
            "last_sign_in": { "type": ["string", "null"], "format": "date-time" },
            "invitation": { "type": ["object", "null"], "description": "The open invitation (never its link)", "properties": {
                "created_at": { "type": "string", "format": "date-time" },
                "expires_at": { "type": "string", "format": "date-time" },
                "by": { "type": "string" },
                "reset": { "type": "boolean" }
            } },
            "sessions": { "type": "integer", "description": "Sign-in sessions listed for the user" },
            "first_admin": { "type": "boolean", "description": "User one: the console user of the sign-in settings" }
        }
    });
    let invitation = json!({
        "type": "object",
        "properties": {
            "user": user.clone(),
            "invitation": { "type": "object", "properties": {
                "url": { "type": "string", "description": "<instance>/ui/#/join/<token>: send it to the person; shown once" },
                "token": { "type": "string", "description": "rbi_ plus 43 base64url characters (32 random bytes)" },
                "expires_at": { "type": "string", "format": "date-time" },
                "reset": { "type": "boolean" }
            } }
        }
    });
    let role = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "description": { "type": "string" },
            "built_in": { "type": "boolean" },
            "privileged": { "type": "boolean", "description": "Grants users.manage or settings.security" },
            "permissions": { "type": "array", "items": { "type": "string" } },
            "users": { "type": "integer", "description": "Users holding it" }
        }
    });
    let role_body = json!({
        "type": "object",
        "required": ["permissions"],
        "properties": {
            "name": { "type": "string", "description": "1 to 40 lower case letters, digits, _ and -, starting with a letter; not a built-in name" },
            "description": { "type": "string", "maxLength": 200 },
            "permissions": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Permission names (GET /_rustybin/roles lists them)" }
        }
    });
    let id_param = json!([{ "name": "id", "in": "path", "required": true, "description": "A user id or sign-in name", "schema": { "type": "string" } }]);
    let name_param =
        json!([{ "name": "name", "in": "path", "required": true, "schema": { "type": "string" } }]);
    let not_found = json!({ "description": "No such user, or not in password mode" });
    let guard = json!({ "description": "A guard rail (reason last_admin, first_admin, invited, role_in_use, full)" });
    let ok = |schema: Value| json!({ "description": "OK", "content": { "application/json": { "schema": schema } } });
    let sessions = json!({ "type": "object", "properties": {
        "count": { "type": "integer" },
        "sessions": { "type": "array", "items": { "type": "object" } }
    } });
    json!({
        USERS_PATH: {
            "get": {
                "tags": ["Control Plane"], "summary": "Users", "operationId": "listUsers",
                "description": "Every user with roles, status, the last sign-in, the open invitation (never its link) and the sessions listed. Password sign-in only.",
                "responses": { "200": ok(json!({ "type": "object", "properties": { "users": { "type": "array", "items": user.clone() }, "count": { "type": "integer" }, "capacity": { "type": "integer" }, "open_invitations": { "type": "integer" } } })), "404": not_found.clone() }
            },
            "post": {
                "tags": ["Control Plane"], "summary": "Invite a user", "operationId": "inviteUser",
                "description": "Creates the user, invited (disabled until they accept), and answers the one-time link (72 hours, shown once, kept only as a SHA-256). The person opens it and sets their own password: nobody else ever sees or types it.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["name", "roles"], "properties": {
                    "name": { "type": "string", "description": "The sign-in name: letters, digits and . _ @ -, at most 64" },
                    "display_name": { "type": "string" },
                    "email": { "type": "string" },
                    "roles": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                } } } } },
                "responses": { "201": { "description": "Invited", "content": { "application/json": { "schema": invitation.clone() } } }, "400": { "description": "Invalid" }, "404": not_found.clone(), "409": guard.clone() }
            }
        },
        "/_rustybin/users/{id}": {
            "get": { "tags": ["Control Plane"], "summary": "A user", "operationId": "getUser", "parameters": id_param.clone(), "responses": { "200": ok(user.clone()), "404": not_found.clone() } },
            "patch": {
                "tags": ["Control Plane"], "summary": "Change a user", "operationId": "updateUser", "parameters": id_param.clone(),
                "description": "Roles, display name, email (null clears it), disabled. Disabling ends the user's sessions. Nobody changes their own roles; the last enabled admin stays.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "properties": {
                    "roles": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "display_name": { "type": "string" },
                    "email": { "type": ["string", "null"] },
                    "disabled": { "type": "boolean" }
                } } } } },
                "responses": { "200": ok(user.clone()), "400": { "description": "Invalid" }, "404": not_found.clone(), "409": guard.clone() }
            },
            "delete": { "tags": ["Control Plane"], "summary": "Delete a user", "operationId": "deleteUser", "parameters": id_param.clone(), "responses": { "200": ok(json!({ "type": "object", "properties": { "deleted": { "type": "string" }, "sessions_ended": { "type": "integer" } } })), "404": not_found.clone(), "409": guard.clone() } }
        },
        "/_rustybin/users/{id}/invite": {
            "post": { "tags": ["Control Plane"], "summary": "A new invitation link (a password reset)", "operationId": "reinviteUser", "parameters": id_param.clone(), "responses": { "201": { "description": "A new link", "content": { "application/json": { "schema": invitation } } }, "404": not_found.clone(), "409": guard.clone() } }
        },
        "/_rustybin/users/{id}/sessions": {
            "get": { "tags": ["Control Plane"], "summary": "A user's sessions", "operationId": "listUserSessions", "parameters": id_param.clone(), "responses": { "200": ok(sessions.clone()), "404": not_found.clone() } },
            "delete": { "tags": ["Control Plane"], "summary": "Sign a user out everywhere", "operationId": "endUserSessions", "parameters": id_param, "responses": { "200": ok(json!({ "type": "object", "properties": { "ended": { "type": "integer" } } })), "404": not_found.clone() } }
        },
        ROLES_PATH: {
            "get": { "tags": ["Control Plane"], "summary": "Roles", "operationId": "listRoles", "description": "The built-in roles, then the custom ones, each with its permissions and how many users hold it, and the permission table (name, group, summary).", "responses": { "200": ok(json!({ "type": "object", "properties": { "roles": { "type": "array", "items": role.clone() }, "permissions": { "type": "array", "items": { "type": "object" } } } })) } },
            "post": { "tags": ["Control Plane"], "summary": "Create a custom role", "operationId": "createRole", "requestBody": { "required": true, "content": { "application/json": { "schema": role_body.clone() } } }, "responses": { "201": ok(role.clone()), "400": { "description": "Invalid, or a built-in name" }, "409": { "description": "A role of that name exists, or the cap" } } }
        },
        "/_rustybin/roles/{name}": {
            "get": { "tags": ["Control Plane"], "summary": "A role", "operationId": "getRole", "parameters": name_param.clone(), "responses": { "200": ok(role.clone()), "404": { "description": "No such role" } } },
            "put": { "tags": ["Control Plane"], "summary": "Create or change a custom role", "operationId": "putRole", "parameters": name_param.clone(), "requestBody": { "required": true, "content": { "application/json": { "schema": role_body } } }, "responses": { "200": ok(role), "400": { "description": "Invalid, or a built-in role" } } },
            "delete": { "tags": ["Control Plane"], "summary": "Delete a custom role", "operationId": "deleteRole", "parameters": name_param, "responses": { "200": ok(json!({ "type": "object", "properties": { "deleted": { "type": "string" } } })), "404": { "description": "No such custom role" }, "409": { "description": "Somebody holds it (role_in_use)" } } }
        },
        ME_PATH: {
            "get": { "tags": ["Control Plane"], "summary": "My account", "operationId": "getMe", "description": "The caller's own user, roles, permissions and sessions count. 400 not_a_user for a credential that is not a user's.", "responses": { "200": ok(user.clone()), "400": { "description": "Not a user (reason not_a_user)" }, "404": { "description": "Not in password mode" } } },
            "patch": { "tags": ["Control Plane"], "summary": "Change my display name or email", "operationId": "updateMe", "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "properties": { "display_name": { "type": "string" }, "email": { "type": ["string", "null"] } } } } } }, "responses": { "200": ok(user), "400": { "description": "Invalid, or not a user" }, "404": { "description": "Not in password mode" } } }
        },
        ME_SESSIONS_PATH: {
            "get": { "tags": ["Control Plane"], "summary": "My sessions", "operationId": "listMySessions", "responses": { "200": ok(sessions), "400": { "description": "Not a user" }, "404": { "description": "Not in password mode" } } },
            "delete": { "tags": ["Control Plane"], "summary": "Sign myself out everywhere", "operationId": "endMySessions", "responses": { "200": ok(json!({ "type": "object", "properties": { "ended": { "type": "integer" } } })), "400": { "description": "Not a user" }, "404": { "description": "Not in password mode" } } }
        },
        "/_rustybin/me/sessions/{id}": {
            "delete": { "tags": ["Control Plane"], "summary": "End one of my sessions", "operationId": "endMySession", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": ok(json!({ "type": "object", "properties": { "ended": { "type": "boolean" } } })), "400": { "description": "Not a user" }, "404": { "description": "Not in password mode" } } }
        }
    })
}
