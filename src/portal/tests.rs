//! The dev portal: item ids and metadata, counters, and the full app
//! (publish and unpublish each kind, the public catalogue, the SPA, auth
//! modes, visitors, the config file, persistence).

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use chrono::NaiveDate;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::insights::{Event, Insights, DAYS, MAX_TRACKED};
use super::*;
use crate::config::Config;
use crate::state::AppState;
use crate::test_support::{body_string, test_app_with, test_state_with};

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "rustybin.test");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = body_string(resp).await;
    (
        status,
        headers,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn app() -> (Router, AppState) {
    let state = test_state_with(Config::for_tests());
    (test_app_with(state.clone()), state)
}

fn item<'a>(catalog: &'a Value, id: &str) -> Option<&'a Value> {
    catalog["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["id"] == id))
}

// ── Units ───────────────────────────────────────────────────────────

#[test]
fn item_ids_round_trip() {
    for raw in [
        "mock:petstore",
        "mcp:main",
        "mcp:weather",
        "llm",
        "a2a:echo",
        "a2a:travel-planner",
        "tools",
        "registry",
    ] {
        let id = ItemId::parse(raw).expect(raw);
        assert_eq!(id.as_string(), raw);
    }
    assert_eq!(ItemId::parse("mock:petstore").map(|i| i.kind()), Ok("api"));
    for bad in [
        "mock:",
        "mock:../x",
        "mcp:nope",
        "a2a:nope",
        "other",
        "",
        "mcp:manage",
    ] {
        assert!(ItemId::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn metadata_is_cleaned_and_bounded() {
    let meta = clean_meta(&Meta {
        title: Some("  Orders\u{7} API  ".into()),
        summary: Some("Line one\r\n\nLine\u{0} two".into()),
        tags: Some(vec!["a".into(), " a ".into(), "".into(), "b".into()]),
        contact: Some("\u{1b}[31mteam@example.com".into()),
        order: Some(5),
        credentials: Some(false),
    })
    .expect("valid");
    assert_eq!(meta.title.as_deref(), Some("Orders API"));
    assert_eq!(meta.summary.as_deref(), Some("Line one\n\nLine two"));
    assert_eq!(meta.tags, Some(vec!["a".to_string(), "b".to_string()]));
    assert_eq!(meta.contact.as_deref(), Some("[31mteam@example.com"));
    assert!(clean_meta(&Meta {
        title: Some("x".repeat(MAX_TITLE + 1)),
        ..Meta::default()
    })
    .is_err());
    assert!(clean_meta(&Meta {
        tags: Some((0..=MAX_TAGS).map(|i| format!("t{i}")).collect()),
        ..Meta::default()
    })
    .is_err());
    assert!(clean_meta(&Meta {
        order: Some(5000),
        ..Meta::default()
    })
    .is_err());
    // Empty strings clear to None (the item's own title is used).
    let meta = clean_meta(&Meta {
        title: Some("   ".into()),
        ..Meta::default()
    })
    .expect("valid");
    assert!(meta.title.is_none());
}

#[test]
fn counters_are_bounded() {
    let mut i = Insights::default();
    let day0 = NaiveDate::from_ymd_opt(2026, 1, 1).expect("date");
    for d in 0..(DAYS as i64 + 20) {
        let day = day0 + chrono::Duration::days(d);
        i.record("llm", Event::View, day);
        i.record("llm", Event::View, day);
    }
    let today = day0 + chrono::Duration::days(DAYS as i64 + 19);
    let s = &i.items["llm"];
    assert_eq!(s.views.len(), DAYS);
    assert_eq!(s.views_since(today, 7), 14);
    assert_eq!(s.views_since(today, DAYS as i64), 2 * DAYS as u64);
    for n in 0..40 {
        i.record("llm", Event::Download(&format!("f{n}")), today);
    }
    assert!(i.items["llm"].downloads.len() <= 16);
    for n in 0..(MAX_TRACKED + 10) {
        i.record(&format!("mock:m{n}"), Event::Try, today);
    }
    assert_eq!(i.items.len(), MAX_TRACKED);
    let v = i.item_json("mock:nothing", today);
    assert_eq!(v["views_7d"], 0);
}

#[test]
fn file_items_are_validated() {
    let section: FileSection = serde_json::from_value(json!({ "items": [
        { "id": "mock:petstore", "title": "Pets" },
        { "id": "llm" },
        { "id": "llm", "title": "again" },
        { "id": "nope" },
        { "id": "tools", "order": 99999 }
    ] }))
    .expect("section");
    let (ok, problems) = file_items(Some(section));
    assert_eq!(
        ok.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["mock:petstore", "llm"]
    );
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(serde_json::from_value::<FileSection>(
        json!({ "items": [{ "id": "llm", "extra": 1 }] })
    )
    .is_err());
}

#[test]
fn settings_are_registered() {
    let keys: Vec<&str> = crate::settings::specs().iter().map(|s| s.key).collect();
    for k in [
        "portal.enabled",
        "portal.auth",
        "portal.base_url",
        "portal.title",
        "portal.intro",
        "portal.accent",
        "portal.contact",
        "portal.visitors",
    ] {
        assert!(keys.contains(&k), "{k}");
    }
    let (c, w) = Config::from_lookup(|k| match k {
        "RUSTYBIN_PORTAL" => Some("ON".into()),
        "RUSTYBIN_PORTAL_AUTH" => Some("sometimes".into()),
        "RUSTYBIN_PORTAL_BASE_URL" => Some("https://gw.example.com/rb/".into()),
        "RUSTYBIN_PORTAL_VISITORS" => Some("true".into()),
        _ => None,
    });
    assert_eq!(c.portal.as_deref(), Some("on"));
    assert!(c.portal_auth.is_none());
    assert_eq!(
        c.portal_base_url.as_deref(),
        Some("https://gw.example.com/rb")
    );
    assert!(c.portal_visitors);
    assert!(
        w.iter().any(|w| w.contains("RUSTYBIN_PORTAL_AUTH")),
        "{w:?}"
    );
}

// ── Full app ────────────────────────────────────────────────────────

#[tokio::test]
async fn off_until_something_is_published() {
    let (app, _) = app();
    for uri in [
        "/portal",
        "/portal/",
        "/portal/portal.js",
        "/portal/api/catalog",
    ] {
        let (status, _, _) = call(&app, "GET", uri, &[], None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let (status, _, view) = call(&app, "GET", "/_rustybin/portal", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["enabled"], false);
    assert_eq!(view["mode"], "auto");
    let ids: Vec<&str> = view["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    for id in [
        "mock:petstore",
        "mcp:main",
        "llm",
        "a2a:echo",
        "tools",
        "registry",
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    assert!(!ids.contains(&"mcp:manage"));

    let (status, _, out) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/llm",
        &[],
        Some(json!({ "tags": ["ai"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["published"]["tags"], json!(["ai"]));
    for uri in [
        "/portal/",
        "/portal/portal.js",
        "/portal/portal.css",
        "/portal/api/catalog",
        "/portal/deep/link",
    ] {
        let (status, _, _) = call(&app, "GET", uri, &[], None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    let (status, headers, _) = call(&app, "GET", "/portal?x=1", &[], None).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(headers["location"], "portal/?x=1");
    let (status, _, _) = call(&app, "GET", "/portal/api/nope", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Explicit off wins.
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &[],
        Some(json!({ "values": { "portal.enabled": "off" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn publish_each_kind_and_only_published_items_show() {
    let (app, _) = app();
    for id in [
        "mock:petstore",
        "mcp:main",
        "mcp:apikey",
        "mcp:sse",
        "llm",
        "a2a:travel-planner",
        "tools",
        "registry",
    ] {
        let (status, _, out) = call(
            &app,
            "PUT",
            &format!("/_rustybin/portal/items/{id}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{id}: {out}");
        assert_eq!(out["id"], id);
    }
    // Updating keeps the publish time and answers 200.
    let (status, _, out) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        &[],
        Some(json!({ "title": "Pets", "summary": "The pet store.", "order": -1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["published"]["title"], "Pets");

    let (status, headers, catalog) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = catalog["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 8, "{ids:?}");
    assert_eq!(ids[0], "mock:petstore", "APIs come first");
    assert!(!ids.contains(&"a2a:echo"));
    let api = item(&catalog, "mock:petstore").expect("api");
    assert_eq!(api["title"], "Pets");
    assert_eq!(api["kind"], "api");
    assert_eq!(api["new"], true);
    let d = &api["detail"];
    assert_eq!(d["base_url"], "http://rustybin.test/mock/petstore");
    assert!(d["operations"].as_array().is_some_and(|o| !o.is_empty()));
    assert_eq!(
        d["downloads"].as_array().map(Vec::len),
        Some(crate::mocks::collections::FORMATS.len())
    );
    assert!(d["mcp"]["claude"]
        .as_str()
        .is_some_and(|c| c.contains("/mock/petstore/_rustybin/mcp")));
    assert!(d["registry"]["search_url"].as_str().is_some());
    assert!(catalog["whats_new"]
        .as_array()
        .is_some_and(|w| w.len() == 6));
    assert_eq!(catalog["portal"]["title"], "API portal");
    assert_eq!(catalog["portal"]["auth"], "open");
    let mcp = &item(&catalog, "mcp:apikey").expect("mcp")["detail"];
    assert_eq!(mcp["auth"]["kind"], "api_key");
    assert!(mcp["tools"].as_array().is_some_and(|t| !t.is_empty()));
    assert!(mcp["claude"]
        .as_str()
        .is_some_and(|c| c.contains("X-API-Key")));
    assert_eq!(
        item(&catalog, "mcp:sse").expect("sse")["detail"]["transport"],
        "sse"
    );
    assert!(
        item(&catalog, "tools").expect("tools")["detail"]["categories"]
            .as_array()
            .is_some_and(|c| c.len() > 5)
    );
    assert_eq!(
        item(&catalog, "registry").expect("registry")["detail"]["scenario"],
        "normal"
    );

    // The catalogue is cached with an ETag.
    let etag = headers["etag"].to_str().expect("etag").to_string();
    let (status, _, _) = call(
        &app,
        "GET",
        "/portal/api/catalog",
        &[("if-none-match", &etag)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);

    // Unpublish: gone from the catalogue, the ETag changes.
    let (status, _, _) = call(
        &app,
        "DELETE",
        "/_rustybin/portal/items/registry",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "DELETE",
        "/_rustybin/portal/items/registry",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, catalog) = call(
        &app,
        "GET",
        "/portal/api/catalog",
        &[("if-none-match", &etag)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(item(&catalog, "registry").is_none());
    assert!(item(&catalog, "mock:petstore").expect("api")["detail"]["registry"].is_null());

    // Bad ids and bodies.
    let (status, _, _) = call(&app, "PUT", "/_rustybin/portal/items/nope", &[], None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:missing",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/llm",
        &[],
        Some(json!({ "colour": "red" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Every sample the portal offers (operations, providers, agents, the
/// registry) works against the instance.
#[tokio::test]
async fn every_sample_answers() {
    let (app, _) = app();
    for id in [
        "mock:petstore",
        "llm",
        "a2a:echo",
        "a2a:weather",
        "registry",
    ] {
        let (status, _, _) = call(
            &app,
            "PUT",
            &format!("/_rustybin/portal/items/{id}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (_, _, catalog) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    let base = catalog["portal"]["base_url"]
        .as_str()
        .expect("base")
        .to_string();
    let mut samples: Vec<(String, Value)> = Vec::new();
    let api = &item(&catalog, "mock:petstore").expect("api")["detail"];
    let mock_base = api["base_url"].as_str().expect("mock base").to_string();
    for op in api["operations"].as_array().expect("ops") {
        samples.push((mock_base.clone(), op["sample"].clone()));
    }
    for p in item(&catalog, "llm").expect("llm")["detail"]["providers"]
        .as_array()
        .expect("providers")
    {
        samples.push((base.clone(), p["sample"].clone()));
    }
    for a in ["a2a:echo", "a2a:weather"] {
        samples.push((
            base.clone(),
            item(&catalog, a).expect("agent")["detail"]["sample"].clone(),
        ));
    }
    for (prefix, s) in samples {
        let path = format!(
            "{}{}",
            prefix.trim_start_matches("http://rustybin.test"),
            s["path"].as_str().expect("path")
        );
        let mut b = Request::builder()
            .method(s["method"].as_str().expect("method"))
            .uri(&path)
            .header("host", "rustybin.test");
        for hd in s["headers"].as_array().into_iter().flatten() {
            b = b.header(
                hd["name"].as_str().expect("name"),
                hd["value"].as_str().expect("value"),
            );
        }
        let body = s["body"]
            .as_str()
            .map(|b| Body::from(b.to_string()))
            .unwrap_or_else(Body::empty);
        let resp = app
            .clone()
            .oneshot(b.body(body).expect("request"))
            .await
            .expect("response");
        let status = resp.status();
        let text = body_string(resp).await;
        assert!(status.is_success(), "{path}: {status} {text}");
    }
    let registry = &item(&catalog, "registry").expect("registry")["detail"];
    let path = registry["servers_url"]
        .as_str()
        .expect("url")
        .trim_start_matches("http://rustybin.test")
        .to_string();
    let (status, _, list) = call(&app, "GET", &path, &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["servers"].as_array().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn downloads_events_and_insights_count() {
    let (app, _) = app();
    let (status, _, _) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, headers, text) = call(
        &app,
        "GET",
        "/portal/api/items/mock:petstore/collections/hurl",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-disposition"]
        .to_str()
        .expect("cd")
        .contains("petstore.hurl"));
    assert!(
        text.as_str().is_some_and(
            |t| t.contains("base_url=http://rustybin.test/mock/petstore")
                && t.contains("GET {{base_url}}/v1/pets")
        ),
        "{text}"
    );
    let (status, _, _) = call(
        &app,
        "GET",
        "/portal/api/items/mock:petstore/collections/nope",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(
        &app,
        "GET",
        "/portal/api/items/llm/collections/hurl",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for ev in ["view", "view", "try"] {
        let (status, _, _) = call(
            &app,
            "POST",
            "/portal/api/events",
            &[],
            Some(json!({ "item": "mock:petstore", "event": ev })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (status, _, _) = call(
        &app,
        "POST",
        "/portal/api/events",
        &[],
        Some(json!({ "item": "llm", "event": "view" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not published");
    let (status, _, _) = call(
        &app,
        "POST",
        "/portal/api/events",
        &[],
        Some(json!({ "item": "mock:petstore", "event": "click" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, out) = call(&app, "GET", "/_rustybin/portal/insights", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let pets = out["items"]
        .as_array()
        .and_then(|i| i.iter().find(|x| x["id"] == "mock:petstore"))
        .expect("pets");
    assert_eq!(pets["views_7d"], 2);
    assert_eq!(pets["tries"], 1);
    assert_eq!(pets["downloads"]["hurl"], 1);
    let (_, _, view) = call(&app, "GET", "/_rustybin/portal", &[], None).await;
    let pets = view["items"]
        .as_array()
        .and_then(|i| i.iter().find(|x| x["id"] == "mock:petstore"))
        .expect("pets");
    assert_eq!(pets["insights"]["downloads_total"], 1);
}

#[tokio::test]
async fn operator_strings_are_escaped() {
    let (app, _) = app();
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &[],
        Some(json!({ "values": { "portal.title": "Acme <script>\"x\"</script>", "portal.accent": "teal" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/mock:petstore",
        &[],
        Some(json!({ "title": "</title><script>alert(1)</script>" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    for uri in ["/portal/", "/portal/a/b"] {
        let (_, headers, html) = call(&app, "GET", uri, &[], None).await;
        let html = html.as_str().expect("html").to_string();
        assert!(
            html.contains("<title>Acme &lt;script&gt;&quot;x&quot;&lt;/script&gt;</title>"),
            "{html}"
        );
        assert!(html.contains("<meta name=\"rustybin-portal-accent\" content=\"teal\">"));
        assert!(!html.contains("<script>\"x\""));
        assert!(headers["content-security-policy"]
            .to_str()
            .expect("csp")
            .contains("script-src 'self'"));
    }
    let (_, _, html) = call(&app, "GET", "/portal/a/b", &[], None).await;
    assert!(html.as_str().expect("html").contains("<base href=\"../\">"));
    let (status, _, html) = call(&app, "GET", "/portal/reference/mock:petstore", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    let html = html.as_str().expect("html").to_string();
    assert!(
        html.contains("&lt;/title&gt;&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
    assert!(html.contains("data-url=\"../../mock/petstore/_rustybin/openapi.json\""));
    let (status, _, _) = call(&app, "GET", "/portal/reference/llm", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn password_and_key_modes() {
    let mut c = Config::for_tests();
    c.admin_token = Some("admin-secret".into());
    c.portal_auth = Some("password".into());
    c.portal = Some("on".into());
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let admin = [("authorization", "Bearer admin-secret")];
    // The page loads; its data needs the credential.
    let (status, _, html) = call(&app, "GET", "/portal/", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html
        .as_str()
        .expect("html")
        .contains("content=\"password\""));
    let (status, headers, out) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(out["auth"], "password");
    assert!(headers.contains_key("www-authenticate"));
    let (status, _, _) = call(
        &app,
        "GET",
        "/portal/api/catalog",
        &[("authorization", "Bearer wrong")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, headers, out) = call(&app, "GET", "/portal/api/catalog", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["items"], json!([]));
    assert!(headers["cache-control"]
        .to_str()
        .expect("cc")
        .contains("private"));
    let jwt = crate::test_support::control_jwt(Some("console"));
    let mut jc = crate::test_support::jwt_config();
    jc.portal_auth = Some("password".into());
    jc.portal = Some("on".into());
    let japp = test_app_with(test_state_with(jc));
    let bearer = format!("Bearer {jwt}");
    let (status, _, _) = call(
        &japp,
        "GET",
        "/portal/api/catalog",
        &[("authorization", &bearer)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Key mode: an API key of the registry.
    use crate::keys::tests::{doc, entry, shared_config, KEY_A};
    let mut kc = shared_config();
    kc.portal_auth = Some("key".into());
    kc.portal = Some("on".into());
    let kstate = test_state_with(kc);
    kstate
        .keys
        .load_bytes(doc(vec![entry("ka1", KEY_A, "starter", "active", json!({}))]).as_bytes())
        .expect("keys");
    let kapp = test_app_with(kstate);
    let (status, _, _) = call(&kapp, "GET", "/portal/", &[], None).await;
    assert_eq!(status, StatusCode::OK, "the page needs no key");
    let (status, _, out) = call(&kapp, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(out["auth"], "key");
    let (status, _, out) = call(
        &kapp,
        "GET",
        "/portal/api/catalog",
        &[("x-rustybin-key", KEY_A)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
}

#[tokio::test]
async fn rights_and_visitors() {
    use crate::config::SelfServiceConfig;
    let mut c = Config::for_tests();
    c.public_mode = true;
    c.mocks_sample = false;
    c.admin_token = Some("admin-secret".into());
    c.self_service = SelfServiceConfig {
        enabled: true,
        home_url: Some("https://home.example/".into()),
        leads_salt: Some("test-salt".into()),
        ..SelfServiceConfig::default()
    };
    let state = test_state_with(c);
    let app = test_app_with(state.clone());
    let admin = [("authorization", "Bearer admin-secret")];
    // Instance items need admin in public mode.
    let (status, _, _) = call(&app, "PUT", "/_rustybin/portal/items/llm", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(&app, "PUT", "/_rustybin/portal/items/llm", &admin, None).await;
    assert_eq!(status, StatusCode::CREATED);

    // A visitor's mock.
    let spec = r#"{"openapi":"3.0.3","info":{"title":"Shop","version":"1"},"paths":{"/items":{"get":{"operationId":"items","responses":{"200":{"description":"ok"}}}}}}"#;
    let (status, _, mock) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        &[("x-rustybin-session", "a")],
        Some(json!({ "spec": spec })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{mock}");
    let name = mock["name"].as_str().expect("name").to_string();
    let token = mock["manage_token"].as_str().expect("token").to_string();
    let mine = [("x-rustybin-mock-token", token.as_str())];
    let uri = format!("/_rustybin/portal/items/mock:{name}");
    // Others see nothing; the owner may not publish without the switch.
    let (status, _, _) = call(&app, "PUT", &uri, &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, out) = call(&app, "PUT", &uri, &mine, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    assert!(out["error"]
        .as_str()
        .is_some_and(|e| e.contains("portal.visitors")));
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/_rustybin/settings",
        &admin,
        Some(json!({ "values": { "portal.visitors": true } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, out) = call(
        &app,
        "PUT",
        &uri,
        &mine,
        Some(json!({ "title": "My shop" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["published"]["visitor"], true);
    assert!(out["published"]["by"]
        .as_str()
        .is_some_and(|b| b.starts_with("v:")));
    let (_, _, catalog) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    let shop = item(&catalog, &format!("mock:{name}")).expect("shop");
    assert_eq!(shop["visitor"], true);
    // The visitor's control view lists their own mock, not other visitors'.
    let (_, _, view) = call(&app, "GET", "/_rustybin/portal", &mine, None).await;
    assert!(view["items"]
        .as_array()
        .is_some_and(|i| i.iter().any(|x| x["id"] == format!("mock:{name}"))));
    let (_, _, view) = call(&app, "GET", "/_rustybin/portal", &[], None).await;
    assert!(!view["items"]
        .as_array()
        .is_some_and(|i| i.iter().any(|x| x["id"] == format!("mock:{name}"))));
    // The visit ends: the item leaves with the mock.
    let (status, _, _) = call(
        &app,
        "DELETE",
        &format!("/_rustybin/mocks/{name}"),
        &mine,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, catalog) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert!(item(&catalog, &format!("mock:{name}")).is_none());
    assert!(state.portal.get(&format!("mock:{name}")).is_none(), "swept");
}

#[tokio::test]
async fn hidden_until_deployed_and_config_file_items() {
    let (app, state) = app();
    let spec = r#"{"openapi":"3.0.3","info":{"title":"Draft","version":"1"},"paths":{"/x":{"get":{"responses":{"200":{"description":"ok"}}}}}}"#;
    let (status, _, out) = call(
        &app,
        "POST",
        "/_rustybin/mocks",
        &[],
        Some(json!({ "name": "draft", "spec": spec, "deploy": false })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    let (status, _, out) = call(&app, "PUT", "/_rustybin/portal/items/mock:draft", &[], None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(out["hidden"], "the mock is not deployed");
    let (status, _, _) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "auto: nothing shown yet");

    state.mocks.set_file_portal(vec![FileItem {
        id: "tools".into(),
        title: Some("Tools from the file".into()),
        summary: None,
        tags: None,
        contact: None,
        order: None,
        credentials: None,
    }]);
    let (status, _, catalog) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        item(&catalog, "tools").expect("tools")["title"],
        "Tools from the file"
    );
    let (status, _, _) = call(&app, "PUT", "/_rustybin/portal/items/tools", &[], None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = call(&app, "DELETE", "/_rustybin/portal/items/tools", &[], None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, _, view) = call(&app, "GET", "/_rustybin/portal", &[], None).await;
    let tools = view["items"]
        .as_array()
        .and_then(|i| i.iter().find(|x| x["id"] == "tools"))
        .expect("tools");
    assert_eq!(tools["published"]["source"], "file");
}

#[tokio::test]
async fn config_file_section_loads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("rustybin.yaml");
    std::fs::write(
        &file,
        "version: 1\nportal:\n  items:\n    - id: mock:petstore\n      title: Pets\n    - id: bogus\n",
    )
    .expect("write");
    let mut c = Config::for_tests();
    c.config_file = Some(file.to_string_lossy().into_owned());
    let state = test_state_with(c);
    crate::mocks::load_startup(&state);
    let (items, _) = state.mocks.file_portal();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "mock:petstore");
    assert!(state
        .mocks
        .load_errors()
        .iter()
        .any(|e| e.contains("bogus")));
    let app = test_app_with(state);
    let (status, _, catalog) = call(&app, "GET", "/portal/api/catalog", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        item(&catalog, "mock:petstore").expect("pets")["title"],
        "Pets"
    );
}

#[tokio::test]
async fn published_items_and_counts_persist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = Config::for_tests();
    c.state_dir = Some(dir.path().to_path_buf());
    let state = test_state_with(c.clone());
    let app = test_app_with(state.clone());
    let (status, _, _) = call(
        &app,
        "PUT",
        "/_rustybin/portal/items/a2a:echo",
        &[],
        Some(json!({ "summary": "Echo" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = call(
        &app,
        "POST",
        "/portal/api/events",
        &[],
        Some(json!({ "item": "a2a:echo", "event": "view" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    state.portal.save().expect("save");
    let path = dir.path().join(STATE_FILE);
    assert!(path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let again = test_state_with(c);
    assert_eq!(
        again
            .portal
            .get("a2a:echo")
            .and_then(|p| p.summary)
            .as_deref(),
        Some("Echo")
    );
    assert_eq!(again.portal.insights().items["a2a:echo"].views.len(), 1);
    // A broken file starts empty.
    std::fs::write(&path, "{not json").expect("write");
    let mut c2 = Config::for_tests();
    c2.state_dir = Some(dir.path().to_path_buf());
    assert!(test_state_with(c2).portal.published().is_empty());
}

#[test]
fn embedded_portal_files() {
    let paths = api::asset_paths();
    for p in ["index.html", "portal.js", "portal.css"] {
        assert!(paths.contains(&p), "{p}");
    }
    for (path, bytes) in api::PORTAL_ASSETS {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{path} contains an em or en dash"
        );
    }
}
