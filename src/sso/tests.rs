//! Single sign-on: a fake provider on a loopback port (discovery, keys,
//! token endpoint; ID tokens signed here, RS256 with the shared test key
//! and ES256 with a P-256 key made once), the flow through the full app,
//! every ID token check, the role map, provisioning, domains, the password
//! sign-in switch, the egress policy, and a second instance whose provider
//! is Rustybin's own demo identity provider.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::control_auth::ControlAuth;
use crate::jwt_state::JwtState;
use crate::rbac::Source;
use crate::state::AppState;
use crate::test_support::{body_json, body_string, test_app_with};

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
/// The fake provider's paths (constants: the catalogue's route scan reads
/// literals).
const JWKS_PATH: &str = "/jwks";
const TOKEN_ENDPOINT: &str = "/token";
const CLIENT: &str = "rustybin-console";

// ── A fake provider ─────────────────────────────────────────────────

#[derive(Default)]
struct Fake {
    issuer: String,
    /// The discovery document's issuer (another one to test the check).
    claimed_issuer: Option<String>,
    jwks: Value,
    jwks_hits: usize,
    /// The ID token the token endpoint answers (built per test).
    id_token: Option<String>,
    token_error: bool,
    last_form: BTreeMap<String, String>,
    last_basic: Option<String>,
    /// The jwks_uri the discovery document names (another host to test
    /// the egress policy).
    jwks_uri: Option<String>,
}

type Shared = Arc<Mutex<Fake>>;

async fn discovery(State(f): State<Shared>) -> Json<Value> {
    let f = f.lock().unwrap();
    let iss = f.claimed_issuer.clone().unwrap_or_else(|| f.issuer.clone());
    Json(json!({
        "issuer": iss,
        "authorization_endpoint": format!("{}/authorize", f.issuer),
        "token_endpoint": format!("{}/token", f.issuer),
        "jwks_uri": f.jwks_uri.clone().unwrap_or_else(|| format!("{}/jwks", f.issuer)),
        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
    }))
}

async fn jwks(State(f): State<Shared>) -> Json<Value> {
    let mut f = f.lock().unwrap();
    f.jwks_hits += 1;
    Json(f.jwks.clone())
}

async fn token(
    State(f): State<Shared>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    let mut f = f.lock().unwrap();
    f.last_form = form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    f.last_basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if f.token_error {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    }
    Json(json!({
        "access_token": "provider-access-token",
        "refresh_token": "provider-refresh-token",
        "token_type": "Bearer",
        "id_token": f.id_token.clone().unwrap_or_default(),
    }))
    .into_response()
}

/// The fake provider on a loopback port.
async fn fake_provider() -> Shared {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shared: Shared = Arc::new(Mutex::new(Fake {
        issuer: format!("http://{addr}"),
        jwks: json!({ "keys": [rsa_jwk(), ec_jwk("ec-1")] }),
        ..Fake::default()
    }));
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route(JWKS_PATH, get(jwks))
        .route(TOKEN_ENDPOINT, post(token))
        .with_state(shared.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    shared
}

fn rsa_jwk() -> Value {
    JwtState::shared_for_tests().rs256_jwk.clone()
}

/// A P-256 key made once per test binary (never RSA per test).
fn ec_key() -> &'static (jsonwebtoken::EncodingKey, String, String) {
    static KEY: OnceLock<(jsonwebtoken::EncodingKey, String, String)> = OnceLock::new();
    KEY.get_or_init(|| {
        let kp = rcgen::KeyPair::generate().expect("p256");
        let raw = kp.public_key_raw();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let x = b64.encode(&raw[1..33]);
        let y = b64.encode(&raw[33..65]);
        let enc =
            jsonwebtoken::EncodingKey::from_ec_pem(kp.serialize_pem().as_bytes()).expect("ec pem");
        (enc, x, y)
    })
}

fn ec_jwk(kid: &str) -> Value {
    let (_, x, y) = ec_key();
    json!({ "kty": "EC", "crv": "P-256", "kid": kid, "use": "sig", "alg": "ES256", "x": x, "y": y })
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn base_claims(issuer: &str, nonce: &str) -> Value {
    json!({
        "iss": issuer,
        "sub": "subject-alice",
        "aud": CLIENT,
        "exp": now() + 300,
        "iat": now(),
        "nonce": nonce,
        "preferred_username": "alice",
        "name": "Alice Doe",
        "email": "alice@example.com",
        "email_verified": true,
        "groups": ["platform-admins", "developers"],
    })
}

fn sign_rs256(claims: &Value) -> String {
    let jwt = JwtState::shared_for_tests();
    let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    h.kid = jwt.rs256_jwk["kid"].as_str().map(str::to_string);
    jsonwebtoken::encode(&h, claims, &jwt.rs256_encoding_key).unwrap()
}

fn sign_es256(claims: &Value, kid: &str) -> String {
    let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    h.kid = Some(kid.to_string());
    jsonwebtoken::encode(&h, claims, &ec_key().0).unwrap()
}

/// An unsigned token (`alg: none`).
fn unsigned(claims: &Value) -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.",
        b64.encode(br#"{"alg":"none","typ":"JWT"}"#),
        b64.encode(claims.to_string())
    )
}

// ── The instance ────────────────────────────────────────────────────

const ADMIN_TOKEN: &str = "sso-test-admin-token";

fn config() -> Config {
    let mut c = Config::for_tests();
    c.control_auth = ControlAuth::Password;
    c.admin_token = Some(ADMIN_TOKEN.into());
    c.egress_allow_loopback = true;
    c
}

fn set(state: &AppState, values: Value) {
    let values: BTreeMap<String, Value> = values
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    state
        .settings
        .update(values, BTreeMap::new(), "test")
        .expect("settings");
}

fn configure(state: &AppState, issuer: &str) {
    set(
        state,
        json!({
            "sso.enabled": true,
            "sso.issuer": issuer,
            "sso.client_id": CLIENT,
            "sso.role_map": ["platform-admins=admin", "developers=editor tester"],
        }),
    );
    let mut secrets = BTreeMap::new();
    secrets.insert(CLIENT_SECRET.to_string(), Some("s3cret".to_string()));
    state
        .settings
        .update(BTreeMap::new(), secrets, "test")
        .expect("secret");
}

async fn instance(fake: &Shared) -> (AppState, Router) {
    instance_with(fake, config()).await
}

async fn instance_with(fake: &Shared, c: Config) -> (AppState, Router) {
    let state = AppState::for_tests(c);
    let issuer = fake.lock().unwrap().issuer.clone();
    configure(&state, &issuer);
    let app = test_app_with(state.clone());
    (state, app)
}

fn req(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "rustybin.test");
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn location(resp: &axum::response::Response) -> String {
    resp.headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn query_of(url: &str) -> BTreeMap<String, String> {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    form_urlencoded::parse(q.as_bytes()).into_owned().collect()
}

/// Start a sign-in from the console: the provider's URL's parameters.
async fn start(app: &Router) -> BTreeMap<String, String> {
    let challenge = challenge_of(VERIFIER);
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?return_to=console&challenge={challenge}",
                api::START_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND, "{}", location(&resp));
    query_of(&location(&resp))
}

/// The callback with a code: where the browser goes.
async fn callback(app: &Router, state_param: &str) -> String {
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?code=provider-code&state={state_param}",
                api::CALLBACK_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    location(&resp)
}

/// A whole sign-in whose ID token `make` builds from the nonce: where the
/// callback sends the browser.
async fn sign_in(app: &Router, fake: &Shared, make: impl Fn(&str, &str) -> String) -> String {
    let q = start(app).await;
    let issuer = fake.lock().unwrap().issuer.clone();
    fake.lock().unwrap().id_token = Some(make(&issuer, &q["nonce"]));
    callback(app, &q["state"]).await
}

/// The callback's page when there is no sign-in to go back to.
async fn callback_page(app: &Router, state_param: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?code=provider-code&state={state_param}",
                api::CALLBACK_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_string(resp).await)
}

async fn redeem(app: &Router, code: &str, verifier: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(req(
            "POST",
            api::TOKEN_PATH,
            Some(json!({ "code": code, "verifier": verifier })),
        ))
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

fn code_of(back: &str) -> &str {
    back.strip_prefix(api::CONSOLE_CODE)
        .unwrap_or_else(|| panic!("not a code: {back}"))
}

fn refusal_of(back: &str) -> &str {
    back.strip_prefix(api::CONSOLE_ERROR)
        .unwrap_or_else(|| panic!("not a refusal: {back}"))
}

// ── The flow ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_sign_in_through_the_provider_maps_groups_to_roles() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;

    let status = body_json(
        app.clone()
            .oneshot(req("GET", api::STATUS_PATH, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status["ready"], true, "{status}");
    assert_eq!(status["name"], "127.0.0.1");
    assert_eq!(status["password_sign_in"], true);
    assert!(!status.to_string().contains("s3cret"));

    let q = start(&app).await;
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], CLIENT);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(
        q["redirect_uri"],
        "http://rustybin.test/_rustybin/sso/callback"
    );
    assert!(q["scope"].starts_with("openid"));
    // The provider's challenge is not the client's: the instance keeps its
    // own verifier for the code exchange.
    assert_ne!(q["code_challenge"], challenge_of(VERIFIER));

    let issuer = fake.lock().unwrap().issuer.clone();
    fake.lock().unwrap().id_token = Some(sign_rs256(&base_claims(&issuer, &q["nonce"])));
    let back = callback(&app, &q["state"]).await;
    let code = code_of(&back).to_string();
    {
        let f = fake.lock().unwrap();
        assert_eq!(f.last_form["grant_type"], "authorization_code");
        assert_eq!(f.last_form["code"], "provider-code");
        assert_eq!(f.last_form["redirect_uri"], q["redirect_uri"]);
        assert_eq!(
            challenge_of(&f.last_form["code_verifier"]),
            q["code_challenge"]
        );
        // A confidential client: HTTP Basic.
        assert!(f
            .last_basic
            .as_deref()
            .is_some_and(|b| b.starts_with("Basic ")));
        assert!(!f.last_form.contains_key("client_secret"));
    }
    // The state works once: with nothing under way there is nowhere to
    // send the browser back, so a page says so.
    let (status, page) = callback_page(&app, &q["state"]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("not under way"), "{page}");

    // A wrong verifier spends the code.
    let (s, body) = redeem(&app, &code, "x".repeat(43).as_str()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{body}");
    let (s, _) = redeem(&app, &code, VERIFIER).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // A fresh sign-in, redeemed with the right verifier.
    let back = sign_in(&app, &fake, |iss, n| sign_rs256(&base_claims(iss, n))).await;
    let (s, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["obtained"], "sso");
    assert_eq!(body["user"], "alice");
    assert_eq!(body["roles"], json!(["admin", "editor", "tester"]));
    let token = body["token"].as_str().unwrap().to_string();
    assert!(!body.to_string().contains("provider-access-token"));

    let user = state.users.find_by_name("alice").expect("provisioned");
    assert_eq!(user.source, Source::Sso);
    assert_eq!(user.display_name, "Alice Doe");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    assert_eq!(user.sso.as_ref().unwrap().subject, "subject-alice");
    assert!(state.users.password_hash(&user.id).is_none());

    let mut r = req("GET", "/_rustybin/session", None);
    r.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let session = body_json(app.clone().oneshot(r).await.unwrap()).await;
    assert_eq!(session["obtained"], "sso", "{session}");
    assert_eq!(session["user"], "alice");
    assert!(session["permissions"]
        .as_array()
        .unwrap()
        .contains(&json!("users.manage")));

    // A group removed at the provider demotes at the next sign-in; the
    // user is the same (linked by subject, whatever the name says now).
    let back = sign_in(&app, &fake, |iss, n| {
        let mut c = base_claims(iss, n);
        c["groups"] = json!(["developers"]);
        c["preferred_username"] = json!("alice.renamed");
        sign_rs256(&c)
    })
    .await;
    let (s, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["user"], "alice");
    assert_eq!(body["roles"], json!(["editor", "tester"]));
    assert_eq!(state.users.list().len(), 2, "user one and alice");

    // The provider's tokens are never kept: nothing in the users file view
    // or the state carries them.
    let users = serde_json::to_string(&state.users.list()).unwrap();
    assert!(!users.contains("provider-access-token"));
    assert!(!users.contains("provider-refresh-token"));
}

#[tokio::test]
async fn every_id_token_check_refuses() {
    let fake = fake_provider().await;
    type Make = Box<dyn Fn(&str, &str) -> String>;
    let cases: Vec<(&str, Make)> = vec![
        (
            "another issuer",
            Box::new(|_iss: &str, n: &str| {
                sign_rs256(&base_claims("https://elsewhere.example", n))
            }),
        ),
        (
            "another audience",
            Box::new(|iss: &str, n: &str| {
                let mut c = base_claims(iss, n);
                c["aud"] = json!("someone-else");
                sign_rs256(&c)
            }),
        ),
        (
            "several audiences and no azp",
            Box::new(|iss: &str, n: &str| {
                let mut c = base_claims(iss, n);
                c["aud"] = json!([CLIENT, "someone-else"]);
                sign_rs256(&c)
            }),
        ),
        (
            "an azp for another client",
            Box::new(|iss: &str, n: &str| {
                let mut c = base_claims(iss, n);
                c["azp"] = json!("someone-else");
                sign_rs256(&c)
            }),
        ),
        (
            "expired",
            Box::new(|iss: &str, n: &str| {
                let mut c = base_claims(iss, n);
                c["exp"] = json!(now() - 600);
                c["iat"] = json!(now() - 900);
                sign_rs256(&c)
            }),
        ),
        (
            "another nonce",
            Box::new(|iss: &str, _n: &str| sign_rs256(&base_claims(iss, "replayed"))),
        ),
        (
            "no nonce",
            Box::new(|iss: &str, n: &str| {
                let mut c = base_claims(iss, n);
                c.as_object_mut().unwrap().remove("nonce");
                sign_rs256(&c)
            }),
        ),
        (
            "alg none",
            Box::new(|iss: &str, n: &str| unsigned(&base_claims(iss, n))),
        ),
        (
            "a shared secret (HS256)",
            Box::new(|iss: &str, n: &str| {
                jsonwebtoken::encode(
                    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
                    &base_claims(iss, n),
                    &jsonwebtoken::EncodingKey::from_secret(b"s3cret"),
                )
                .unwrap()
            }),
        ),
        (
            "a bad signature",
            Box::new(|iss: &str, n: &str| {
                let t = sign_rs256(&base_claims(iss, n));
                let (rest, _) = t.rsplit_once('.').unwrap();
                format!("{rest}.AAAA")
            }),
        ),
        (
            "garbage",
            Box::new(|_iss: &str, _n: &str| "not.a.jwt".to_string()),
        ),
    ];
    for (what, make) in cases {
        // A fresh instance per case: the throttle counts every refusal.
        let (state, app) = instance(&fake).await;
        let back = sign_in(&app, &fake, |i, n| make(i, n)).await;
        assert_eq!(refusal_of(&back), "invalid_id_token", "{what}: {back}");
        assert!(state.users.find_by_name("alice").is_none(), "{what}");
    }
    // The same token, signed right, signs in (the cases above differ by
    // their flaw only).
    let (_, app) = instance(&fake).await;
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    code_of(&back);
}

#[tokio::test]
async fn an_unknown_key_id_refetches_the_keys_once_and_es256_verifies() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    // ES256 with a key the cache knows.
    let back = sign_in(&app, &fake, |i, n| sign_es256(&base_claims(i, n), "ec-1")).await;
    code_of(&back);
    let fetched = state.sso.jwks_fetches();
    assert_eq!(fetched, 1);
    // A key the cached set lacks: the keys are fetched once more.
    fake.lock().unwrap().jwks = json!({ "keys": [rsa_jwk(), ec_jwk("ec-2")] });
    let back = sign_in(&app, &fake, |i, n| sign_es256(&base_claims(i, n), "ec-2")).await;
    code_of(&back);
    assert_eq!(state.sso.jwks_fetches(), fetched + 1);
    // Still unknown after the refetch: refused, and fetched only once.
    let back = sign_in(&app, &fake, |i, n| sign_es256(&base_claims(i, n), "ec-9")).await;
    assert_eq!(refusal_of(&back), "invalid_id_token");
    assert_eq!(state.sso.jwks_fetches(), fetched + 2);
    // A known key does not fetch.
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    code_of(&back);
    assert_eq!(state.sso.jwks_fetches(), fetched + 2);
    assert_eq!(fake.lock().unwrap().jwks_hits as u64, fetched + 2);
}

#[tokio::test]
async fn the_role_map_and_the_default_roles() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    let with_groups = |groups: Value, sub: &'static str, name: &'static str| {
        move |iss: &str, n: &str| {
            let mut c = base_claims(iss, n);
            c["groups"] = groups.clone();
            c["sub"] = json!(sub);
            c["preferred_username"] = json!(name);
            c["email"] = json!(format!("{name}@example.com"));
            sign_rs256(&c)
        }
    };
    // In no mapped group, no default roles: refused, and nobody is created.
    let back = sign_in(&app, &fake, with_groups(json!(["sales"]), "s-bob", "bob")).await;
    assert_eq!(refusal_of(&back), "no_role");
    assert!(state.users.find_by_name("bob").is_none());
    // The default roles cover them; an unknown role in the map is ignored.
    set(
        &state,
        json!({ "sso.default_roles": ["viewer"], "sso.role_map": ["platform-admins=admin", "developers=editor ghost"] }),
    );
    let back = sign_in(&app, &fake, with_groups(json!(["sales"]), "s-bob", "bob")).await;
    let (_, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(body["roles"], json!(["viewer"]), "{body}");
    let back = sign_in(
        &app,
        &fake,
        with_groups(json!("developers"), "s-carl", "carl"),
    )
    .await;
    let (_, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(
        body["roles"],
        json!(["editor"]),
        "a string claim is one group: {body}"
    );
    // A nested groups claim.
    set(&state, json!({ "sso.groups_claim": "realm_access.roles" }));
    let back = sign_in(&app, &fake, |iss, n| {
        let mut c = base_claims(iss, n);
        c["realm_access"] = json!({ "roles": ["platform-admins"] });
        c.as_object_mut().unwrap().remove("groups");
        sign_rs256(&c)
    })
    .await;
    let (_, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(body["roles"], json!(["admin"]), "{body}");

    // An admin pins a role on an SSO user: kept on top of the groups'.
    let carl = state.users.find_by_name("carl").unwrap();
    let mut r = req(
        "PATCH",
        &format!("/_rustybin/users/{}", carl.id),
        Some(json!({ "pinned_roles": ["tester"] })),
    );
    r.headers_mut()
        .insert("x-rustybin-admin-token", ADMIN_TOKEN.parse().unwrap());
    let resp = app.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["roles"], json!(["editor", "tester"]), "{v}");
    // Their roles are the provider's: a PATCH of roles is refused.
    let mut r = req(
        "PATCH",
        &format!("/_rustybin/users/{}", carl.id),
        Some(json!({ "roles": ["admin"] })),
    );
    r.headers_mut()
        .insert("x-rustybin-admin-token", ADMIN_TOKEN.parse().unwrap());
    let resp = app.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(resp).await["reason"], "sso_user");
    set(&state, json!({ "sso.groups_claim": "groups" }));
    let back = sign_in(&app, &fake, with_groups(json!([]), "s-carl", "carl")).await;
    let (_, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(body["roles"], json!(["viewer", "tester"]), "{body}");
}

#[tokio::test]
async fn provisioning_off_admits_only_users_added_ahead() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    set(&state, json!({ "sso.provision": false }));
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    assert_eq!(refusal_of(&back), "not_provisioned");
    assert!(state.users.find_by_name("alice").is_none());
    // An admin adds alice as a single sign-on user: no link.
    let mut r = req(
        "POST",
        "/_rustybin/users",
        Some(json!({ "name": "alice", "roles": ["viewer"], "sso": true })),
    );
    r.headers_mut()
        .insert("x-rustybin-admin-token", ADMIN_TOKEN.parse().unwrap());
    let resp = app.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = body_json(resp).await;
    assert!(v["invitation"].is_null(), "{v}");
    assert_eq!(v["user"]["source"], "sso");
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    let (_, body) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(body["user"], "alice");
    assert_eq!(
        body["roles"],
        json!(["admin", "editor", "tester", "viewer"])
    );
    // Another identity of the provider cannot take the linked name.
    let back = sign_in(&app, &fake, |i, n| {
        let mut c = base_claims(i, n);
        c["sub"] = json!("someone-else");
        sign_rs256(&c)
    })
    .await;
    assert_eq!(refusal_of(&back), "other_identity");
    // Nor a password user's name.
    let back = sign_in(&app, &fake, |i, n| {
        let mut c = base_claims(i, n);
        c["sub"] = json!("rb-takeover");
        c["preferred_username"] = json!("rustybin");
        sign_rs256(&c)
    })
    .await;
    assert_eq!(refusal_of(&back), "name_taken");
}

#[tokio::test]
async fn allowed_domains_and_disabled_users() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    set(&state, json!({ "sso.allowed_domains": ["corp.example"] }));
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    assert_eq!(refusal_of(&back), "domain_not_allowed");
    let back = sign_in(&app, &fake, |i, n| {
        let mut c = base_claims(i, n);
        c["email"] = json!("alice@CORP.example");
        c["email_verified"] = json!(false);
        sign_rs256(&c)
    })
    .await;
    assert_eq!(refusal_of(&back), "email_unverified");
    let back = sign_in(&app, &fake, |i, n| {
        let mut c = base_claims(i, n);
        c["email"] = json!("alice@CORP.example");
        sign_rs256(&c)
    })
    .await;
    let (s, _) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(s, StatusCode::OK);
    // Disabled on the instance: refused whatever the provider says.
    let alice = state.users.find_by_name("alice").unwrap();
    state.users.set_disabled(&alice.id, true).unwrap();
    let back = sign_in(&app, &fake, |i, n| {
        let mut c = base_claims(i, n);
        c["email"] = json!("alice@corp.example");
        sign_rs256(&c)
    })
    .await;
    assert_eq!(refusal_of(&back), "disabled");
}

#[tokio::test]
async fn the_provider_cannot_demote_the_last_admin() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    redeem(&app, code_of(&back), VERIFIER).await;
    let first = state.users.first_admin().unwrap();
    state.users.set_disabled(&first.id, true).unwrap();
    let back = sign_in(&app, &fake, |i, n| {
        let mut c = base_claims(i, n);
        c["groups"] = json!(["developers"]);
        sign_rs256(&c)
    })
    .await;
    assert_eq!(refusal_of(&back), "last_admin");
    assert!(state
        .users
        .find_by_name("alice")
        .unwrap()
        .roles
        .contains(&"admin".to_string()));
}

#[tokio::test]
async fn sso_users_never_get_a_password_or_an_invitation() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    let (_, body) = redeem(&app, code_of(&back), VERIFIER).await;
    let token = body["token"].as_str().unwrap().to_string();
    let alice = state.users.find_by_name("alice").unwrap();
    let hash = crate::login::hash_password("a-longer-secret").unwrap();
    assert!(matches!(
        state.users.set_password(&alice.id, hash),
        Err(crate::rbac::UsersError::Refused(
            crate::rbac::users::SSO_USER,
            _
        ))
    ));
    assert!(matches!(
        state.users.invite(&alice.id, "test"),
        Err(crate::rbac::UsersError::Refused(
            crate::rbac::users::SSO_USER,
            _
        ))
    ));
    let mut r = req(
        "POST",
        &format!("/_rustybin/users/{}/invite", alice.id),
        None,
    );
    r.headers_mut()
        .insert("x-rustybin-admin-token", ADMIN_TOKEN.parse().unwrap());
    let resp = app.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(resp).await["reason"], "sso_user");
    let mut r = req(
        "PUT",
        "/_rustybin/me/password",
        Some(json!({ "current_password": "x", "new_password": "a-longer-secret" })),
    );
    r.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let resp = app.clone().oneshot(r).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    // No password signs them in.
    let resp = app
        .clone()
        .oneshot(req(
            "POST",
            "/_rustybin/login",
            Some(json!({ "username": "alice", "password": "" })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // A password user gets no pinned roles.
    let first = state.users.first_admin().unwrap();
    let patch = crate::rbac::users::UserPatch {
        pinned_roles: Some(vec!["viewer".into()]),
        ..Default::default()
    };
    assert!(matches!(
        state.users.update(&first.id, patch),
        Err(crate::rbac::UsersError::Refused(
            crate::rbac::users::NOT_SSO,
            _
        ))
    ));
}

#[tokio::test]
async fn password_sign_in_off_leaves_sso_and_the_admin_token() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    let login = || {
        req(
            "POST",
            "/_rustybin/login",
            Some(json!({ "username": "rustybin", "password": "rustybin" })),
        )
    };
    assert_eq!(
        app.clone().oneshot(login()).await.unwrap().status(),
        StatusCode::OK
    );
    set(&state, json!({ "sso.password_sign_in": false }));
    let resp = app.clone().oneshot(login()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(resp).await["reason"], "password_sign_in_off");
    let resp = app
        .clone()
        .oneshot(req(
            "POST",
            "/_rustybin/join",
            Some(json!({ "token": "rbi_x", "password": "a-longer-secret" })),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let status = body_json(
        app.clone()
            .oneshot(req("GET", api::STATUS_PATH, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status["password_sign_in"], false);
    // The admin token is break glass.
    let mut r = req("GET", "/_rustybin/users", None);
    r.headers_mut()
        .insert("x-rustybin-admin-token", ADMIN_TOKEN.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(r).await.unwrap().status(),
        StatusCode::OK
    );
    // Single sign-on still signs in.
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    let (s, _) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(s, StatusCode::OK);
    // With single sign-on not set up, the switch does not lock anyone out.
    set(&state, json!({ "sso.enabled": false }));
    assert_eq!(
        app.clone().oneshot(login()).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn discovery_and_keys_go_through_the_egress_policy() {
    let fake = fake_provider().await;
    // Loopback not allowed: the issuer's host is permitted, its address
    // is not.
    let mut c = config();
    c.egress_allow_loopback = false;
    let (state, app) = instance_with(&fake, c).await;
    let challenge = challenge_of(VERIFIER);
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?return_to=console&challenge={challenge}",
                api::START_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(refusal_of(&location(&resp)), "provider_error");
    let decisions = state.egress.decisions();
    let d = decisions
        .iter()
        .find(|d| d.purpose == crate::egress::Purpose::Sso)
        .expect("a decision for purpose sso");
    assert!(!d.allowed);
    assert!(d.reason.contains("loopback"), "{}", d.reason);
    // The rule names the issuer's host.
    let rule = state.egress.rule(crate::egress::Purpose::Sso);
    assert!(!rule.allow.is_empty());
    // Another start right away answers the failure without fetching again
    // (an open route cannot make the instance hammer the provider).
    let count = |s: &AppState| {
        s.egress
            .decisions()
            .iter()
            .filter(|d| d.purpose == crate::egress::Purpose::Sso)
            .count()
    };
    let before = count(&state);
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?return_to=console&challenge={challenge}",
                api::START_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(refusal_of(&location(&resp)), "provider_error");
    assert_eq!(count(&state), before);

    // A discovery document whose keys live on another host: refused (only
    // the issuer's host is permitted).
    let (state, app) = instance(&fake).await;
    fake.lock().unwrap().jwks_uri = Some("http://keys.elsewhere.example/jwks".into());
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    assert_eq!(refusal_of(&back), "provider_error");
    assert!(state
        .egress
        .decisions()
        .iter()
        .any(|d| d.purpose == crate::egress::Purpose::Sso
            && !d.allowed
            && d.origin.contains("keys.elsewhere.example")));
    fake.lock().unwrap().jwks_uri = None;

    // A discovery document for another issuer is refused.
    let (_, app) = instance(&fake).await;
    fake.lock().unwrap().claimed_issuer = Some("https://other.example".into());
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?return_to=console&challenge={challenge}",
                api::START_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(refusal_of(&location(&resp)), "provider_error");
    fake.lock().unwrap().claimed_issuer = None;

    // The token endpoint's refusal.
    let (_, app) = instance(&fake).await;
    fake.lock().unwrap().token_error = true;
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    assert_eq!(refusal_of(&back), "provider_error");
    fake.lock().unwrap().token_error = false;
}

#[tokio::test]
async fn the_test_route_reads_discovery_and_the_keys() {
    let fake = fake_provider().await;
    let (state, app) = instance(&fake).await;
    set(
        &state,
        json!({ "sso.role_map": ["platform-admins=admin", "x=ghost"] }),
    );
    let mut r = req("POST", api::TEST_PATH, None);
    r.headers_mut()
        .insert("x-rustybin-admin-token", ADMIN_TOKEN.parse().unwrap());
    let v = body_json(app.clone().oneshot(r).await.unwrap()).await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["keys"].as_array().unwrap().len(), 2);
    assert_eq!(v["unknown_roles"], json!(["ghost"]));
    assert_eq!(
        v["redirect_uri"],
        "http://rustybin.test/_rustybin/sso/callback"
    );
    assert_eq!(v["client_secret"]["set"], true);
    assert!(!v.to_string().contains("s3cret"));
    // Without a credential the test is refused (it is not a sign-in route).
    let resp = app
        .clone()
        .oneshot(req("POST", api::TEST_PATH, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sign_ins_under_way_are_bounded_and_expire() {
    let fake = fake_provider().await;
    let clock = Arc::new(crate::limits::ManualClock::new(chrono::Utc::now()));
    let state = AppState::for_tests_with_clock(config(), clock.clone());
    let issuer = fake.lock().unwrap().issuer.clone();
    configure(&state, &issuer);
    let app = test_app_with(state.clone());
    let challenge = challenge_of(VERIFIER);
    let begin = || {
        req(
            "GET",
            &format!(
                "{}?return_to=console&challenge={challenge}",
                api::START_PATH
            ),
            None,
        )
    };
    let mut first = None;
    for _ in 0..MAX_PENDING_PER_ADDRESS {
        let resp = app.clone().oneshot(begin()).await.unwrap();
        let q = query_of(&location(&resp));
        first.get_or_insert(q["state"].clone());
    }
    let resp = app.clone().oneshot(begin()).await.unwrap();
    assert_eq!(refusal_of(&location(&resp)), "rate_limited");
    assert_eq!(state.sso.pending_count(), MAX_PENDING_PER_ADDRESS);
    // Ten minutes later they are gone, and the state no longer works.
    clock.advance(PENDING_TTL);
    assert_eq!(state.sso.pending_count(), 0);
    let (status, page) = callback_page(&app, first.as_deref().unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(page.contains("not under way"), "{page}");
    // A code expires in two minutes.
    let back = sign_in(&app, &fake, |i, n| sign_rs256(&base_claims(i, n))).await;
    clock.advance(HANDOFF_TTL);
    let (s, _) = redeem(&app, code_of(&back), VERIFIER).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_routes_check_what_they_are_given_and_escape_what_they_show() {
    let fake = fake_provider().await;
    let (_, app) = instance(&fake).await;
    // A return address that is not the console nor a loopback one.
    for bad in [
        "https://attacker.example/cb",
        "http://attacker.example/cb",
        "javascript:alert(1)",
    ] {
        let resp = app
            .clone()
            .oneshot(req(
                "GET",
                &format!(
                    "{}?return_to={}&challenge={}",
                    api::START_PATH,
                    form_urlencoded::byte_serialize(bad.as_bytes()).collect::<String>(),
                    challenge_of(VERIFIER)
                ),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    // A missing challenge.
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!("{}?return_to=console", api::START_PATH),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // The CLI's loopback address gets the code in its query.
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?return_to=http%3A%2F%2F127.0.0.1%3A5999%2Fcallback&challenge={}",
                api::START_PATH,
                challenge_of(VERIFIER)
            ),
            None,
        ))
        .await
        .unwrap();
    let q = query_of(&location(&resp));
    let issuer = fake.lock().unwrap().issuer.clone();
    fake.lock().unwrap().id_token = Some(sign_rs256(&base_claims(&issuer, &q["nonce"])));
    let back = callback(&app, &q["state"]).await;
    assert!(
        back.starts_with("http://127.0.0.1:5999/callback?code=rbs_"),
        "{back}"
    );
    // The provider's refusal, with no sign-in under way: an escaped page.
    let resp = app
        .clone()
        .oneshot(req(
            "GET",
            &format!(
                "{}?state=nope&error=%3Cscript%3Ealert(1)%3C%2Fscript%3E",
                api::CALLBACK_PATH
            ),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let html = body_string(resp).await;
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;"), "{html}");
}

#[test]
fn return_to_is_the_console_or_a_loopback_address() {
    assert_eq!(ReturnTo::parse("console"), Some(ReturnTo::Console));
    assert_eq!(ReturnTo::parse(""), Some(ReturnTo::Console));
    for ok in [
        "http://127.0.0.1:5000/callback",
        "http://localhost:1/x",
        "http://[::1]:8080/cb",
    ] {
        assert!(
            matches!(ReturnTo::parse(ok), Some(ReturnTo::Loopback(_))),
            "{ok}"
        );
    }
    for bad in [
        "https://127.0.0.1/cb",
        "http://10.0.0.1/cb",
        "http://user:pw@127.0.0.1/cb",
        "http://127.0.0.1/cb#frag",
        "ftp://127.0.0.1/",
    ] {
        assert_eq!(ReturnTo::parse(bad), None, "{bad}");
    }
}

#[test]
fn the_settings_check_their_values() {
    let state = AppState::for_tests(config());
    let bad = |k: &str, v: Value| {
        let mut m = BTreeMap::new();
        m.insert(k.to_string(), v);
        state.settings.update(m, BTreeMap::new(), "test").is_err()
    };
    assert!(
        bad(ISSUER, json!("http://idp.example.com")),
        "http only on loopback"
    );
    assert!(bad(ISSUER, json!("https://idp.example.com/?x=1")));
    assert!(!bad(ISSUER, json!("https://idp.example.com/realms/team")));
    assert!(!bad(ISSUER, json!("http://127.0.0.1:9000")));
    assert!(bad(ROLE_MAP, json!(["no-equals"])));
    assert!(bad(ROLE_MAP, json!(["group="])));
    assert!(bad(ROLE_MAP, json!(["group=Admin"])));
    assert!(bad(DEFAULT_ROLES, json!(["Not A Role"])));
    assert!(bad(ALLOWED_DOMAINS, json!(["not a domain"])));
    assert!(bad(CLIENT_ID, json!("has space")));
    // The role map takes {group, roles} objects too, kept as strings.
    let mut m = BTreeMap::new();
    m.insert(
        ROLE_MAP.to_string(),
        json!([{ "group": "cn=admins,ou=groups", "roles": ["admin", "tester"] }, "devs=editor"]),
    );
    state.settings.update(m, BTreeMap::new(), "test").unwrap();
    assert_eq!(
        state.settings.get(ROLE_MAP).0,
        json!(["cn=admins,ou=groups=admin tester", "devs=editor"])
    );
    assert_eq!(
        parse_role_map_entry("cn=admins,ou=groups=admin tester").unwrap(),
        (
            "cn=admins,ou=groups".to_string(),
            vec!["admin".to_string(), "tester".to_string()]
        )
    );
    // Scopes as one string.
    let mut m = BTreeMap::new();
    m.insert(SCOPES.to_string(), json!("openid profile groups"));
    state.settings.update(m, BTreeMap::new(), "test").unwrap();
    assert_eq!(
        state.settings.list(SCOPES),
        vec!["openid", "profile", "groups"]
    );
}

#[test]
fn the_environment_seeds_the_settings() {
    let mut c = config();
    c.sso_env.set("RUSTYBIN_SSO_ENABLED", "true");
    c.sso_env
        .set("RUSTYBIN_SSO_ISSUER", "https://idp.example.com");
    c.sso_env.set("RUSTYBIN_SSO_CLIENT_ID", "rustybin");
    c.sso_env.set("RUSTYBIN_SSO_CLIENT_SECRET", "from-env");
    c.sso_env
        .set("RUSTYBIN_SSO_ROLE_MAP", "admins=admin,devs=editor tester");
    c.sso_env.set("RUSTYBIN_SSO_PROVISION", "off");
    // Invalid: warned about and ignored.
    c.sso_env
        .set("RUSTYBIN_SSO_ALLOWED_DOMAINS", "not a domain");
    assert!(!format!("{:?}", c.sso_env).contains("from-env"));
    let state = AppState::for_tests(c);
    let s = setup(&state.settings).expect("set up from the environment");
    assert_eq!(s.issuer, "https://idp.example.com");
    assert_eq!(
        s.client_secret.as_deref().map(|x| x.as_str()),
        Some("from-env")
    );
    assert_eq!(
        s.role_map,
        vec![
            ("admins".to_string(), vec!["admin".to_string()]),
            (
                "devs".to_string(),
                vec!["editor".to_string(), "tester".to_string()]
            )
        ]
    );
    assert!(!s.provision);
    assert!(s.allowed_domains.is_empty());
    assert_eq!(state.settings.get(ISSUER).1, crate::settings::Source::Env);
}

#[test]
fn a_document_from_git_never_changes_single_sign_on() {
    let state = AppState::for_tests(config());
    configure(&state, "https://idp.example.com");
    let doc = crate::mocks::config_file::parse(
        "version: 2\nsettings:\n  sso.issuer: https://attacker.example\n  sso.role_map: [everyone=admin]\n  sso.default_roles: [admin]\n  console.title: From git\n",
    )
    .expect("parse");
    let opts = crate::configdoc::ApplyOptions {
        prune: true,
        dry_run: false,
        principal: "git:test".into(),
        base: None,
        untrusted: true,
        caller: None,
        base_url: None,
    };
    let r = crate::configdoc::apply(&state, doc, &opts);
    let problems = r["problems"].to_string();
    for k in ["sso.issuer", "sso.role_map", "sso.default_roles"] {
        assert!(problems.contains(&format!("settings.{k}")), "{k}: {r}");
    }
    assert_eq!(
        state.settings.string(ISSUER).as_deref(),
        Some("https://idp.example.com")
    );
    assert_eq!(
        state.settings.list(ROLE_MAP),
        vec!["platform-admins=admin", "developers=editor tester"]
    );
    assert_eq!(state.settings.get("console.title").0, json!("From git"));
    // The prune of a pulled document leaves them alone too.
    assert!(state.settings.bool(ENABLED));
}

// ── Rustybin's own identity provider as the issuer ──────────────────

/// A second instance (the demo identity provider at /oauth/*) on a real
/// loopback port.
async fn idp_instance(dir: &std::path::Path) -> crate::server::RunningServer {
    let mut c = Config::for_tests();
    c.tls_cert = dir.join("server.crt").display().to_string();
    c.tls_key = dir.join("server.key").display().to_string();
    crate::server::start_with_state(AppState::for_tests(c))
        .await
        .expect("the provider instance starts")
}

/// Walk the demo provider's sign-in page as a browser would: the
/// authorization URL's parameters, the user's name and password, Authorize.
/// Answers where the provider sends the browser (the callback URL).
pub(crate) async fn approve_at_demo_provider(authorize_url: &str, user: &str) -> String {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let page = client.get(authorize_url).send().await.unwrap();
    assert_eq!(page.status(), 200, "the provider's sign-in page");
    let mut form: Vec<(String, String)> = query_of(authorize_url).into_iter().collect();
    form.push(("username".into(), user.into()));
    form.push(("password".into(), user.into()));
    form.push(("action".into(), "approve".into()));
    let base = authorize_url.split('?').next().unwrap();
    let resp = client.post(base).form(&form).send().await.unwrap();
    assert_eq!(
        resp.status(),
        303,
        "{}",
        resp.text().await.unwrap_or_default()
    );
    resp.headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn the_demo_provider_signs_in_a_second_instance() {
    let dir = tempfile::tempdir().unwrap();
    let idp = idp_instance(dir.path()).await;
    let issuer = format!("http://{}", idp.http_addr);
    let state = AppState::for_tests(config());
    set(
        &state,
        json!({
            "sso.enabled": true,
            "sso.issuer": issuer,
            "sso.client_id": crate::oidc::DEMO_CLIENT_ID,
            "sso.scopes": ["openid", "profile", "email", "groups"],
            "sso.role_map": ["admins=admin", "users=viewer"],
        }),
    );
    let mut secrets = BTreeMap::new();
    secrets.insert(
        CLIENT_SECRET.to_string(),
        Some(crate::oidc::DEMO_CLIENT_SECRET.to_string()),
    );
    state
        .settings
        .update(BTreeMap::new(), secrets, "test")
        .unwrap();
    let app = test_app_with(state.clone());
    for (who, roles) in [
        ("alice", json!(["admin", "viewer"])),
        ("bob", json!(["viewer"])),
    ] {
        let challenge = challenge_of(VERIFIER);
        let resp = app
            .clone()
            .oneshot(req(
                "GET",
                &format!(
                    "{}?return_to=console&challenge={challenge}",
                    api::START_PATH
                ),
                None,
            ))
            .await
            .unwrap();
        let authorize = location(&resp);
        assert!(
            authorize.starts_with(&format!("{issuer}/oauth/authorize?")),
            "{authorize}"
        );
        let back_to_us = approve_at_demo_provider(&authorize, who).await;
        let path = back_to_us
            .strip_prefix("http://rustybin.test")
            .unwrap_or_else(|| panic!("{back_to_us}"));
        let resp = app.clone().oneshot(req("GET", path, None)).await.unwrap();
        let back = location(&resp);
        let (s, body) = redeem(&app, code_of(&back), VERIFIER).await;
        assert_eq!(s, StatusCode::OK, "{who}: {body}");
        assert_eq!(body["user"], who);
        assert_eq!(body["roles"], roles, "{who}");
    }
    let alice = state.users.find_by_name("alice").unwrap();
    assert!(alice.email.is_some_and(|e| e.starts_with("alice@")));
    assert_eq!(alice.sso.unwrap().issuer, issuer);
    idp.shutdown();
}
