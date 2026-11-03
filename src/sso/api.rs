//! The routes of single sign-on: `GET /_rustybin/sso/status` (the sign-in
//! screen), `GET /_rustybin/sso/start` and `GET /_rustybin/sso/callback`
//! (the browser's round trip through the provider), `POST
//! /_rustybin/sso/token` (the one-time code and the verifier for a sign-in
//! token) and `POST /_rustybin/sso/test` (discovery and the keys, fetched
//! now). All but the test are open: they are how one signs in. The
//! callback and the token are throttled like the password sign-in (per
//! client address, and every address together); a start is limited by the
//! sign-ins under way per address. Everything answers `404` outside the
//! password sign-in (`RUSTYBIN_CONTROL_AUTH=password`), where users live.

use axum::extract::{Query, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{Refusal, ReturnTo};
use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

pub const STATUS_PATH: &str = "/_rustybin/sso/status";
pub const START_PATH: &str = "/_rustybin/sso/start";
pub const CALLBACK_PATH: &str = "/_rustybin/sso/callback";
pub const TOKEN_PATH: &str = "/_rustybin/sso/token";
pub const TEST_PATH: &str = "/_rustybin/sso/test";
/// What the sign-in throttle counts single sign-on failures under (never
/// a user name: parentheses are not allowed in one).
const SSO_KEY: &str = "(sso)";
/// Where the console takes a one-time code (`#/sso/<code>`) or a refusal
/// (`#/sso-error/<reason>`).
pub const CONSOLE_CODE: &str = "/ui/#/sso/";
pub const CONSOLE_ERROR: &str = "/ui/#/sso-error/";

fn not_found(state: &AppState) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "not found",
            "control_auth": state.config.control_auth.as_str(),
            "hint": "single sign-on works with the password sign-in only (RUSTYBIN_CONTROL_AUTH=password, the default, outside shared mode)",
        })),
    )
        .into_response()
}

fn no_store(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    resp
}

fn json_error(status: StatusCode, reason: &str, message: &str) -> Response {
    no_store((status, Json(json!({ "error": message, "reason": reason }))).into_response())
}

fn redirect(location: &str) -> Response {
    let mut resp = StatusCode::FOUND.into_response();
    match HeaderValue::from_str(location) {
        Ok(v) => {
            resp.headers_mut().insert(header::LOCATION, v);
            no_store(resp)
        }
        Err(_) => page(
            StatusCode::BAD_REQUEST,
            "The sign-in could not send you back: the address is not valid.",
        ),
    }
}

/// A small HTML page (the callback's answers when there is nowhere to go
/// back to). The message is escaped.
fn page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Single sign-on</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:36rem;margin:4rem auto;padding:0 1rem;line-height:1.5}}</style>\
         </head><body><h1>Single sign-on</h1><p>{}</p><p><a href=\"/ui/\">Back to the sign-in</a></p></body></html>",
        crate::landing::html_escape(message)
    );
    let mut resp = (status, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
    );
    no_store(resp)
}

/// Send the browser back with a refusal: the console's error page, or the
/// CLI's loopback address with `error` and `error_description`.
fn back_with_error(to: Option<&ReturnTo>, r: &Refusal) -> Response {
    match to {
        Some(ReturnTo::Console) => redirect(&format!("{CONSOLE_ERROR}{}", r.reason)),
        Some(ReturnTo::Loopback(url)) => match reqwest::Url::parse(url) {
            Ok(mut u) => {
                u.query_pairs_mut()
                    .append_pair("error", r.reason)
                    .append_pair("error_description", &r.message);
                redirect(u.as_str())
            }
            Err(_) => page(StatusCode::BAD_REQUEST, &r.message),
        },
        None => page(StatusCode::BAD_REQUEST, &r.message),
    }
}

fn back_with_code(to: &ReturnTo, code: &str) -> Response {
    match to {
        ReturnTo::Console => redirect(&format!("{CONSOLE_CODE}{code}")),
        ReturnTo::Loopback(url) => match reqwest::Url::parse(url) {
            Ok(mut u) => {
                u.query_pairs_mut().append_pair("code", code);
                redirect(u.as_str())
            }
            Err(_) => page(StatusCode::BAD_REQUEST, "The CLI's address is not valid."),
        },
    }
}

fn redirect_uri(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
) -> String {
    format!(
        "{}{CALLBACK_PATH}",
        crate::session::request_origin(headers, extensions, uri, &state.config).base_url()
    )
}

fn audit(event: &str, user: &str, ip: Option<std::net::IpAddr>, outcome: &str) {
    tracing::info!(
        target: "rustybin::auth",
        event,
        user = %crate::logging::printable(user, 64),
        ip = %ip.map(|i| i.to_string()).unwrap_or_default(),
        outcome,
        "single sign-on"
    );
}

// ── Handlers ────────────────────────────────────────────────────────

async fn status_handler(State(state): State<AppState>) -> Response {
    if !crate::login::enabled(&state.config) {
        return not_found(&state);
    }
    no_store(Json(state.sso.status()).into_response())
}

#[derive(Deserialize, Default)]
struct StartQuery {
    #[serde(default)]
    return_to: String,
    #[serde(default)]
    challenge: String,
}

async fn start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
    q: Result<Query<StartQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if !crate::login::enabled(&state.config) {
        return not_found(&state);
    }
    let Ok(Query(q)) = q else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "expected ?return_to=console&challenge=<S256 challenge>",
        );
    };
    let Some(to) = ReturnTo::parse(&q.return_to) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "return_to: console, or an http:// address on a loopback host (the CLI's listener)",
        );
    };
    let ip = crate::session::client_ip(&headers, &extensions, &state.config);
    let callback = redirect_uri(&state, &headers, &extensions, &uri);
    match state
        .sso
        .begin(to.clone(), &q.challenge, &callback, ip)
        .await
    {
        Ok(url) => redirect(&url),
        Err(r) if r.reason == "invalid_request" => {
            json_error(StatusCode::BAD_REQUEST, r.reason, &r.message)
        }
        Err(r) => {
            audit("sso_start", "", ip, r.reason);
            back_with_error(Some(&to), &r)
        }
    }
}

#[derive(Deserialize, Default)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: Option<String>,
}

async fn callback_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    q: Result<Query<CallbackQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if !crate::login::enabled(&state.config) {
        return not_found(&state);
    }
    let q = q.map(|Query(q)| q).unwrap_or_default();
    let ip = crate::session::client_ip(&headers, &extensions, &state.config);
    let tokens = &state.config.console_tokens;
    if let Err(wait) = tokens.check(ip, SSO_KEY) {
        audit("sso", "", ip, "rate_limited");
        return page(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("Too many failed sign-ins from this address: try again in {wait} s."),
        );
    }
    if let Some(error) = q.error.as_deref() {
        // The provider refused (the person denied, say): the sign-in under
        // way ends and the browser goes back with the provider's reason.
        tokens.abandoned(ip, SSO_KEY);
        let to = state.sso.cancel(&q.state);
        audit("sso", "", ip, "denied");
        let r = Refusal::new(
            "denied",
            format!(
                "the identity provider did not sign you in ({})",
                crate::logging::printable(error, 80)
            ),
        );
        return back_with_error(to.as_ref(), &r);
    }
    match state.sso.complete(&state.users, &q.state, &q.code).await {
        Ok(done) => {
            tokens.succeeded(ip, SSO_KEY);
            audit("sso", &done.user.name, ip, "ok");
            back_with_code(&done.return_to, &done.code)
        }
        Err((to, r)) => {
            if matches!(r.reason, "busy" | "not_configured" | "provider_error") {
                tokens.abandoned(ip, SSO_KEY);
            } else {
                tokens.failed(ip, SSO_KEY);
            }
            audit("sso", "", ip, r.reason);
            back_with_error(to.as_ref(), &r)
        }
    }
}

#[derive(Deserialize, Default)]
struct TokenBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    verifier: String,
}

async fn token_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    body: Result<Json<TokenBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if !crate::login::enabled(&state.config) {
        return not_found(&state);
    }
    let Ok(Json(body)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "expected { \"code\": \"rbs_...\", \"verifier\": \"...\" }",
        );
    };
    let ip = crate::session::client_ip(&headers, &extensions, &state.config);
    let tokens = &state.config.console_tokens;
    if let Err(wait) = tokens.check(ip, SSO_KEY) {
        let mut resp = json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            &format!("too many failed sign-ins: try again in {wait} s"),
        );
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return resp;
    }
    let user = state
        .sso
        .redeem(&body.code, &body.verifier)
        .and_then(|id| state.users.get(&id))
        .filter(|u| !u.disabled);
    let Some(user) = user else {
        tokens.failed(ip, SSO_KEY);
        audit("sso_token", "", ip, "invalid_code");
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_code",
            "this code is not valid: it was used, it expired (two minutes), or the verifier is not the one the sign-in started with",
        );
    };
    match crate::login::sso_session(&state, &user, ip) {
        Some(body) => {
            tokens.succeeded(ip, SSO_KEY);
            audit("sso_token", &user.name, ip, "ok");
            no_store(Json(body).into_response())
        }
        None => {
            tokens.abandoned(ip, SSO_KEY);
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "could not sign the token",
            )
        }
    }
}

async fn test_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    if !crate::login::enabled(&state.config) {
        return not_found(&state);
    }
    let callback = redirect_uri(&state, &headers, &extensions, &uri);
    no_store(Json(state.sso.test(&state.users, &callback).await).into_response())
}

// ── Router, catalogue, OpenAPI ──────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(STATUS_PATH, get(status_handler))
        .route(START_PATH, get(start_handler))
        .route(CALLBACK_PATH, get(callback_handler))
        .route(TOKEN_PATH, post(token_handler))
        .route(TEST_PATH, post(test_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            STATUS_PATH,
            &["GET"],
            category::CONTROL,
            "Single sign-on for the sign-in screen: on, ready, the button's name, whether the password sign-in works",
        )
        .open()
        .description(
            "Never behind control-plane auth and never a secret: enabled, ready (the issuer and \
             the client id are set), name (\"Sign in with <name>\"), password_sign_in. 404 \
             outside the password sign-in.",
        )
        .example(Example::get("Single sign-on status", STATUS_PATH).expect_status(404)),
        Endpoint::new(
            START_PATH,
            &["GET"],
            category::CONTROL,
            "Start a single sign-on: the browser goes to the identity provider (authorization code, PKCE, state, nonce)",
        )
        .open()
        .description(
            "return_to is console (the console's #/sso/<code>) or an http:// address on a \
             loopback host (the CLI's listener); challenge is the S256 challenge of a verifier \
             the client keeps for POST /_rustybin/sso/token. Answers 302 to the provider's \
             authorization endpoint. At most 10 sign-ins under way per client address, 256 in \
             all, each for ten minutes.",
        )
        .example(
            Example::get(
                "Start a sign-in",
                "/_rustybin/sso/start?return_to=console&challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            )
            .expect_status(404),
        ),
        Endpoint::new(
            CALLBACK_PATH,
            &["GET"],
            category::CONTROL,
            "The identity provider's redirect: the code is exchanged, the ID token verified, the user signed in",
        )
        .open()
        .description(
            "Register <instance>/_rustybin/sso/callback as the client's redirect URI. Verifies \
             the ID token (issuer, audience, expiry, nonce; RS256 or ES256), maps the groups \
             to roles, links or creates the user and sends the browser back with a one-time \
             code (two minutes). Throttled like the sign-in: 5 failures per client address \
             and minute, 100 from every address together.",
        )
        .example(
            Example::get("The provider's redirect", "/_rustybin/sso/callback?code=x&state=y")
                .expect_status(404),
        ),
        Endpoint::new(
            TOKEN_PATH,
            &["POST"],
            category::CONTROL,
            "Trade the one-time code and the PKCE verifier for a sign-in token (single sign-on)",
        )
        .open()
        .description(
            "The code works once, for two minutes, and only with the verifier whose challenge \
             started the sign-in. Answers the sign-in token of POST /_rustybin/login (12 hours, \
             the user's roles resolved at every request). Throttled like the sign-in.",
        )
        .example(
            Example::post("Trade the code", TOKEN_PATH)
                .json(r#"{"code":"rbs_x","verifier":"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"}"#)
                .expect_status(404),
        ),
        Endpoint::new(
            TEST_PATH,
            &["POST"],
            category::CONTROL,
            "Test single sign-on: fetch the provider's discovery document and keys now, check the role map",
        )
        .permission(crate::rbac::Permission::SettingsWrite)
        .description(
            "Fetches discovery and the JWKS through the outbound policy (purpose sso), fresh, \
             and answers what they say, the redirect URI to register, and the roles the role \
             map names that do not exist. Never the client secret (its {set, hint} only).",
        )
        .example(Example::post("Test single sign-on", TEST_PATH).expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let not_found = json!({ "description": "Not in the password sign-in (RUSTYBIN_CONTROL_AUTH=password, outside shared mode)" });
    let status_schema = json!({
        "type": "object",
        "properties": {
            "enabled": { "type": "boolean", "description": "sso.enabled" },
            "ready": { "type": "boolean", "description": "Enabled, with the issuer and the client id set" },
            "name": { "type": ["string", "null"], "description": "The button's name: Sign in with <name>" },
            "password_sign_in": { "type": "boolean", "description": "The password sign-in works (off only while single sign-on is ready and sso.password_sign_in is off)" },
            "reason": { "type": ["string", "null"], "description": "Why it is not ready" },
            "start": { "type": "string" }
        }
    });
    json!({
        STATUS_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Single sign-on status",
                "description": "What the sign-in screen needs: whether single sign-on is on and ready, the button's name and whether the password sign-in works. Never a secret; nothing is fetched. Open.",
                "operationId": "getSsoStatus",
                "security": [],
                "responses": {
                    "200": { "description": "The status", "content": { "application/json": { "schema": status_schema } } },
                    "404": not_found
                }
            }
        },
        START_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Start a single sign-on",
                "description": "Sends the browser to the identity provider's authorization endpoint (authorization code flow, PKCE S256, state, nonce). return_to: console, or an http:// address on a loopback host (the CLI). challenge: the S256 challenge of a verifier the client keeps. Open; at most 10 sign-ins under way per client address. A refusal goes back to return_to (the console's #/sso-error/<reason>, or the loopback address with error and error_description).",
                "operationId": "startSso",
                "security": [],
                "parameters": [
                    { "name": "return_to", "in": "query", "required": false, "schema": { "type": "string", "default": "console" } },
                    { "name": "challenge", "in": "query", "required": true, "schema": { "type": "string", "minLength": 43, "maxLength": 43 } }
                ],
                "responses": {
                    "302": { "description": "To the provider (or back to return_to with the refusal)" },
                    "400": { "description": "return_to or challenge is not valid" },
                    "404": not_found
                }
            }
        },
        CALLBACK_PATH: {
            "get": {
                "tags": ["Control Plane"],
                "summary": "Single sign-on callback",
                "description": "The provider's redirect (register it as the client's redirect URI). The code is exchanged at the token endpoint, the ID token verified (issuer, audience, expiry, nonce; RS256 or ES256), the groups mapped to roles, the user linked or created, and the browser sent back to return_to with a one-time code (the console's #/sso/<code>, or ?code= on the loopback address). The provider's tokens are never kept. Open; throttled per client address like the sign-in. Refusal reasons: expired, denied, provider_error, invalid_id_token, domain_not_allowed, email_unverified, not_provisioned, name_taken, other_identity, disabled, no_role, last_admin, rate_limited, busy.",
                "operationId": "ssoCallback",
                "security": [],
                "parameters": [
                    { "name": "code", "in": "query", "required": false, "schema": { "type": "string" } },
                    { "name": "state", "in": "query", "required": true, "schema": { "type": "string" } },
                    { "name": "error", "in": "query", "required": false, "schema": { "type": "string" } }
                ],
                "responses": {
                    "302": { "description": "Back to return_to, with the code or the refusal" },
                    "400": { "description": "No sign-in under way for this state: an HTML page" },
                    "404": not_found,
                    "429": { "description": "Too many failures from this address: an HTML page" }
                }
            }
        },
        TOKEN_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Single sign-on: the code for a sign-in token",
                "description": "The one-time code the callback sent back and the PKCE verifier whose challenge started the sign-in. One use, two minutes. Answers the sign-in token (as POST /_rustybin/login). Open; throttled per client address.",
                "operationId": "ssoToken",
                "security": [],
                "requestBody": { "required": true, "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["code", "verifier"],
                    "properties": {
                        "code": { "type": "string", "description": "rbs_ plus 43 base64url characters" },
                        "verifier": { "type": "string", "minLength": 43, "maxLength": 128 }
                    }
                } } } },
                "responses": {
                    "200": { "description": "Signed in", "content": { "application/json": { "schema": { "type": "object", "properties": {
                        "token": { "type": "string" },
                        "token_type": { "type": "string" },
                        "expires_in": { "type": "integer" },
                        "expires_at": { "type": "string", "format": "date-time" },
                        "user": { "type": "string" },
                        "user_id": { "type": "string" },
                        "obtained": { "type": "string", "enum": ["sso"] }
                    } } } } },
                    "401": { "description": "Not a valid code for this verifier (reason invalid_code)" },
                    "404": not_found,
                    "429": { "description": "Too many failures from this address (Retry-After)" }
                }
            }
        },
        TEST_PATH: {
            "post": {
                "tags": ["Control Plane"],
                "summary": "Test single sign-on",
                "description": "Fetches the provider's discovery document and keys now (purpose sso of the outbound policy) and answers them, the redirect URI to register at the provider, the role map and the roles it names that do not exist. Never the client secret.",
                "operationId": "testSso",
                "responses": {
                    "200": { "description": "The test (ok: false with error when the provider could not be read)", "content": { "application/json": { "schema": { "type": "object", "properties": {
                        "ok": { "type": "boolean" },
                        "error": { "type": "string" },
                        "redirect_uri": { "type": "string" },
                        "discovery": { "type": "object" },
                        "keys": { "type": "array", "items": { "type": "object" } },
                        "unknown_roles": { "type": "array", "items": { "type": "string" } }
                    } } } } },
                    "404": not_found
                }
            }
        }
    })
}
