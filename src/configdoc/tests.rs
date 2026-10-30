//! The configuration document: the round trip of every section, the
//! version 1 upgrade, the diff, secret references, bundles, the routes
//! (dry run, prune), startup from a version 2 file.

use std::collections::BTreeMap;

use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::mocks::store::Definition;
use crate::test_support::{
    body_json, body_string, get_request, json_request, test_app_with, test_state_with,
};

const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"1"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok"}}}}}}"#;
const SPEC_TWO: &str = r#"{"openapi":"3.0.3","info":{"title":"Orders","version":"2"},"paths":{"/orders":{"get":{"operationId":"listOrders","responses":{"200":{"description":"ok"}}},"post":{"operationId":"createOrder","responses":{"201":{"description":"ok"}}}}}}"#;
const SERVER: &str = r#"{"$schema":"https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json","name":"io.example/orders","description":"Orders over MCP","version":"1.0.0","remotes":[{"type":"streamable-http","url":"https://mcp.example.com/orders"}]}"#;

fn state_with_everything() -> AppState {
    let state = test_state_with(Config::for_tests());
    let report = state.mocks.apply(
        vec![Definition {
            name: "orders".into(),
            source: SPEC.into(),
            settings: Default::default(),
            enabled: true,
        }],
        Origin::Api,
        None,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let mut values = BTreeMap::new();
    values.insert("console.title".to_string(), json!("Payments team mocks"));
    values.insert("console.user".to_string(), json!("payments"));
    values.insert("agents.default_ceiling".to_string(), json!("manage"));
    let mut secrets = BTreeMap::new();
    secrets.insert(
        "leads.webhook_token".to_string(),
        Some("a-long-stored-token".to_string()),
    );
    state
        .settings
        .update(values, secrets, "test")
        .expect("settings");
    state
        .mocks
        .set_file_registry(vec![serde_json::from_str(SERVER).expect("server")]);
    state
        .portal
        .publish(
            "mock:orders",
            crate::portal::Meta {
                title: Some("Orders API".into()),
                tags: Some(vec!["payments".into()]),
                ..Default::default()
            },
            "test".into(),
            Some(crate::mocks::visits::Owner::Instance),
            chrono::Utc::now(),
        )
        .expect("publish");
    let (policy, problems) = agents::file_policy(Some(agents::FileSection {
        default_ceiling: Some("traffic".into()),
        rules: vec![agents::FileRule {
            matcher: agents::Match {
                name: Some("ci-*".into()),
                credential: None,
            },
            ceiling: Some("read-only".into()),
            ..Default::default()
        }],
    }));
    assert!(problems.is_empty(), "{problems:?}");
    state.mocks.set_file_agents(policy);
    state.health.set(false);
    state
}

fn sections_except_secrets(file: &MocksFile) -> Value {
    let mut v = serde_json::to_value(file).expect("value");
    v.as_object_mut().expect("object").remove("secrets");
    v
}

#[test]
fn export_then_apply_on_a_second_instance_gives_the_same_export() {
    let a = state_with_everything();
    let export_a = export(&a, SecretsMode::Refs, false);
    let file = &export_a.file;
    assert_eq!(file.version, 2);
    assert_eq!(
        file.instance,
        Some(InstanceSection {
            title: Some("Payments team mocks".into()),
            console: Some(ConsoleSection {
                user: Some("payments".into())
            }),
        })
    );
    assert_eq!(
        file.settings
            .as_ref()
            .and_then(|s| s.get("agents.default_ceiling")),
        Some(&json!("manage"))
    );
    assert!(file
        .settings
        .as_ref()
        .is_some_and(|s| !s.contains_key("console.title")));
    assert_eq!(
        file.secrets
            .as_ref()
            .and_then(|s| s.get("leads.webhook_token")),
        Some(&Some(SECRET_STORED.to_string()))
    );
    assert_eq!(file.mocks().len(), 1, "the sample is never exported");
    assert_eq!(file.mocks()[0].name, "orders");
    assert!(file.mocks()[0].spec_inline.is_some());
    assert_eq!(file.registry.as_ref().map(|r| r.servers.len()), Some(1));
    assert_eq!(file.portal.as_ref().map(|p| p.items.len()), Some(1));
    assert_eq!(
        file.portal.as_ref().and_then(|p| p.items[0].title.clone()),
        Some("Orders API".into())
    );
    assert!(file.harness.is_none(), "harness mode is off in tests");
    assert_eq!(file.agents.as_ref().map(|a| a.rules.len()), Some(1));
    assert_eq!(
        file.health.as_ref().map(|h| h.initial.as_str()),
        Some("unhealthy")
    );
    let yaml = export_a.yaml();
    assert!(yaml.starts_with("# Rustybin configuration"), "{yaml}");
    assert!(
        !yaml.contains("a-long-stored-token"),
        "a secret value leaked"
    );

    // Apply the YAML to a fresh instance.
    let b = test_state_with(Config::for_tests());
    let parsed = crate::mocks::config_file::parse(&yaml).expect("parse the export");
    let report = apply(
        &b,
        parsed,
        &ApplyOptions {
            prune: true,
            dry_run: false,
            principal: "test".into(),
            base: None,
        },
    );
    assert_eq!(report["problems"], json!([]), "{report}");
    assert_eq!(report["results"]["mocks"]["created"], json!(["orders"]));
    assert_eq!(
        report["results"]["secrets"]["kept"],
        json!(["leads.webhook_token"])
    );
    assert!(!b.health.is_healthy());
    assert_eq!(
        b.settings.string("console.title").as_deref(),
        Some("Payments team mocks")
    );
    assert_eq!(b.mocks.file_agents().rules.len(), 1);
    let export_b = export(&b, SecretsMode::Refs, false);
    assert_eq!(
        sections_except_secrets(&export_b.file),
        sections_except_secrets(&export_a.file)
    );
    // B holds no secret: the reference is not exported there.
    assert_eq!(export_b.file.secrets, Some(BTreeMap::new()));
    // Secrets omitted: no section, demo credentials redacted.
    let omitted = export(&a, SecretsMode::Omit, false);
    assert!(omitted.file.secrets.is_none());
}

#[test]
fn a_version_1_file_is_a_version_2_document_with_mocks_only() {
    let state = test_state_with(Config::for_tests());
    let text = format!("version: 1\nmocks:\n  - name: orders\n    spec_inline: {SPEC}\n");
    let file = crate::mocks::config_file::parse(&text).expect("v1");
    assert_eq!(file.version, 1);
    assert_eq!(file.sections(), vec!["mocks"]);
    let report = apply(
        &state,
        file,
        &ApplyOptions {
            prune: true,
            dry_run: false,
            principal: "t".into(),
            base: None,
        },
    );
    assert_eq!(report["results"]["mocks"]["created"], json!(["orders"]));
    assert!(
        report["results"].get("settings").is_none(),
        "absent sections are left alone"
    );
    assert!(state.health.is_healthy(), "health untouched");
    let exported = export(&state, SecretsMode::Refs, false).file;
    assert_eq!(exported.version, 2);
}

#[test]
fn the_diff_names_what_changes() {
    let before_text = format!(
        "version: 2\ninstance: {{title: Old}}\nsettings: {{console.backlink: 'https://a.example'}}\nmocks:\n  - name: orders\n    spec_inline: {SPEC}\n  - name: gone\n    spec_inline: {SPEC}\nportal: {{items: [{{id: 'mock:orders'}}]}}\nhealth: {{initial: healthy}}\n"
    );
    let after_text = format!(
        "version: 2\ninstance: {{title: New}}\nsettings: {{console.backlink: 'https://b.example', agents.default_ceiling: manage}}\nmocks:\n  - name: orders\n    enabled: false\n    spec_inline: {SPEC_TWO}\n    responses: {{mode: generated}}\n  - name: fresh\n    spec_inline: {SPEC}\nportal: {{items: []}}\nhealth: {{initial: unhealthy}}\nagents: {{default_ceiling: traffic}}\n"
    );
    let before = crate::mocks::config_file::parse(&before_text).expect("before");
    let after = crate::mocks::config_file::parse(&after_text).expect("after");
    let lines: Vec<String> = diff(&before, &after, None)
        .iter()
        .map(Change::line)
        .collect();
    assert_eq!(
        lines,
        vec![
            "instance.title: changed from \"Old\" to \"New\"",
            "settings.agents.default_ceiling: added (manage)",
            "settings.console.backlink: changed to https://b.example",
            "mocks.orders: spec changed (1 operation(s) added), disabled, responses changed",
            "mocks.fresh: added",
            "mocks.gone: removed",
            "portal.mock:orders: removed",
            "agents.default_ceiling: set to \"traffic\"",
            "health.initial: changed from \"healthy\" to \"unhealthy\"",
        ]
    );
    // A section the new document leaves out is not compared.
    let partial = crate::mocks::config_file::parse("version: 2\nhealth: {initial: healthy}\n")
        .expect("partial");
    assert!(diff(&before, &partial, None).is_empty());
}

#[test]
fn secret_references_resolve_from_the_environment_or_are_kept() {
    let state = test_state_with(Config::for_tests());
    std::env::set_var("RUSTYBIN_TEST_CONFIGDOC_TOKEN", "from-the-environment");
    let text = "version: 2\nsecrets:\n  leads.webhook_token: ${RUSTYBIN_TEST_CONFIGDOC_TOKEN}\n  console.password_hash: secret:stored\n  git.token: ${RUSTYBIN_TEST_CONFIGDOC_MISSING}\n  console.title: nope\n";
    let file = crate::mocks::config_file::parse(text).expect("doc");
    let opts = ApplyOptions {
        prune: false,
        dry_run: false,
        principal: "t".into(),
        base: None,
    };
    let report = apply(&state, file, &opts);
    let problems = report["problems"].as_array().expect("problems").clone();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0]
            .as_str()
            .unwrap_or("")
            .contains("console.title: not a secret"),
        "{problems:?}"
    );
    assert!(
        problems[1]
            .as_str()
            .unwrap_or("")
            .contains("RUSTYBIN_TEST_CONFIGDOC_MISSING not set"),
        "{problems:?}"
    );
    assert_eq!(
        report["results"]["secrets"]["kept"],
        json!(["console.password_hash"])
    );
    assert_eq!(
        report["results"]["secrets"]["changed"],
        json!(["leads.webhook_token"])
    );
    assert_eq!(
        state
            .settings
            .secret("leads.webhook_token")
            .as_deref()
            .map(String::as_str),
        Some("from-the-environment")
    );
    // null unsets a stored secret; prune unsets the ones not named.
    let file =
        crate::mocks::config_file::parse("version: 2\nsecrets: {leads.webhook_token: null}\n")
            .expect("doc");
    let report = apply(&state, file, &opts);
    assert_eq!(
        report["results"]["secrets"]["unset"],
        json!(["leads.webhook_token"])
    );
    assert!(state.settings.secret("leads.webhook_token").is_none());
}

#[test]
fn a_bundle_writes_the_specs_as_files_and_loads_back() {
    let state = state_with_everything();
    let export = super::export(&state, SecretsMode::Refs, true);
    assert_eq!(export.specs.len(), 1);
    assert_eq!(export.specs[0].path, "specs/orders.json");
    assert_eq!(
        export.file.mocks()[0].spec.as_deref(),
        Some("specs/orders.json")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("specs").join("stale.yaml"), "x").ok();
    write_dir(&export, dir.path()).expect("write");
    assert!(dir.path().join("rustybin.yaml").exists());
    assert!(dir.path().join("specs/orders.json").exists());
    let loaded =
        crate::mocks::config_file::load_file(&dir.path().join("rustybin.yaml"), |_| None, 1 << 20)
            .expect("load");
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.defs.len(), 1);
    assert_eq!(loaded.defs[0].source.trim(), SPEC);
    assert!(loaded.extra.instance.is_some());
    assert!(loaded.extra.health.is_some());
    assert!(loaded.sections.contains(&"settings"));
    // A second write removes spec files of mocks that are gone.
    std::fs::write(dir.path().join("specs/old.yaml"), "x").expect("stale");
    write_dir(&export, dir.path()).expect("write again");
    assert!(!dir.path().join("specs/old.yaml").exists());
    // The tar: gzip magic, then the entries.
    let gz = tar_gz(&export);
    assert_eq!(&gz[..2], &[0x1f, 0x8b]);
    let mut tar = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gz[..]), &mut tar)
        .expect("gunzip");
    let mut names = Vec::new();
    let mut at = 0;
    while at + 512 <= tar.len() && tar[at] != 0 {
        let name = String::from_utf8_lossy(&tar[at..at + 100])
            .trim_end_matches('\0')
            .to_string();
        let size =
            usize::from_str_radix(String::from_utf8_lossy(&tar[at + 124..at + 135]).trim(), 8)
                .expect("size");
        names.push(name);
        at += 512 + size.div_ceil(512) * 512;
    }
    assert_eq!(
        names,
        vec![
            "rustybin-config/rustybin.yaml",
            "rustybin-config/specs/orders.json"
        ]
    );
}

#[tokio::test]
async fn the_routes_export_dry_run_apply_and_prune() {
    let state = state_with_everything();
    let app = test_app_with(state.clone());
    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/config/schema"))
        .await
        .expect("schema");
    assert_eq!(resp.status(), StatusCode::OK);
    let schema = body_json(resp).await;
    assert_eq!(schema["properties"]["version"]["enum"], json!([1, 2]));
    assert!(schema["properties"]["secrets"].is_object());

    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/config/export"))
        .await
        .expect("export");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap_or("")
        .starts_with("application/yaml"));
    let yaml = body_string(resp).await;
    assert!(yaml.contains("name: orders"), "{yaml}");

    let resp = app
        .clone()
        .oneshot(get_request(
            "/_rustybin/config/export?format=bundle&secrets=omit",
        ))
        .await
        .expect("bundle");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/gzip");

    let resp = app
        .clone()
        .oneshot(get_request("/_rustybin/config/export?format=nope"))
        .await
        .expect("bad");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // A dry run reports and changes nothing.
    let doc = format!("version: 2\nmocks:\n  - name: fresh\n    spec_inline: {SPEC}\nsettings: {{}}\nhealth: {{initial: healthy}}\n");
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/config/apply",
            &json!({ "document": doc, "prune": true, "dry_run": true }),
        ))
        .await
        .expect("dry run");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["dry_run"], json!(true));
    assert_eq!(body["results"]["mocks"]["would_create"], json!(["fresh"]));
    assert_eq!(body["results"]["mocks"]["would_delete"], json!(["orders"]));
    assert_eq!(
        body["results"]["settings"]["unset"],
        json!(["agents.default_ceiling", "console.title", "console.user"])
    );
    assert!(
        !yaml.contains("git.remote"),
        "the git group is never exported"
    );
    assert!(
        body["summary"]
            .as_array()
            .expect("summary")
            .iter()
            .any(|l| l == "mocks.orders: removed"),
        "{body}"
    );
    assert!(state.mocks.get("orders").is_some() && state.mocks.get("fresh").is_none());
    assert!(!state.health.is_healthy());

    // The same, applied with prune.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/config/apply",
            &json!({ "document": doc, "prune": true }),
        ))
        .await
        .expect("apply");
    let body = body_json(resp).await;
    assert_eq!(body["ok"], json!(true), "{body}");
    assert_eq!(body["results"]["mocks"]["deleted"], json!(["orders"]));
    assert!(state.mocks.get("fresh").is_some() && state.mocks.get("orders").is_none());
    assert!(state.health.is_healthy());
    assert!(state.settings.string("console.title").is_none(), "pruned");
    assert_eq!(
        state.settings.get("console.user").1,
        crate::settings::Source::Default
    );

    // A JSON object is a document too; a bad one is 400.
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/config/apply",
            &json!({ "document": { "version": 2, "health": { "initial": "unhealthy" } } }),
        ))
        .await
        .expect("object");
    assert_eq!(body_json(resp).await["ok"], json!(true));
    assert!(!state.health.is_healthy());
    let resp = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/_rustybin/config/apply",
            &json!({ "document": "version: 3\n" }),
        ))
        .await
        .expect("bad version");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .oneshot(json_request(
            "POST",
            "/_rustybin/config/apply",
            &json!({ "document": "version: 2\nnope: 1\n" }),
        ))
        .await
        .expect("unknown section");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn a_version_2_config_file_loads_every_section_at_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("orders.json"), SPEC).expect("spec");
    let text = "version: 2\ninstance: {title: From the file}\nsettings: {agents.default_ceiling: read-only}\nsecrets: {leads.webhook_token: '${RUSTYBIN_TEST_CONFIGDOC_FILE_TOKEN:-file-token-value}'}\nmocks:\n  - name: orders\n    spec: orders.json\nhealth: {initial: unhealthy}\nagents: {rules: [{match: {name: 'x'}, ceiling: read-only}]}\n";
    let path = dir.path().join("rustybin.yaml");
    std::fs::write(&path, text).expect("file");
    let mut config = Config::for_tests();
    config.config_file = Some(path.display().to_string());
    let state = test_state_with(config);
    crate::mocks::load_startup(&state);
    let orders = state.mocks.get("orders").expect("the file mock");
    assert_eq!(orders.origin, Origin::File);
    assert_eq!(
        state.settings.string("console.title").as_deref(),
        Some("From the file")
    );
    assert_eq!(
        state.settings.string("agents.default_ceiling").as_deref(),
        Some("read-only")
    );
    assert_eq!(
        state
            .settings
            .secret("leads.webhook_token")
            .as_deref()
            .map(String::as_str),
        Some("file-token-value")
    );
    assert!(!state.health.is_healthy());
    assert_eq!(state.mocks.file_agents().rules.len(), 1);
    // The export carries it all back, with the secret as a reference.
    let file = export(&state, SecretsMode::Refs, false).file;
    assert_eq!(
        file.instance.as_ref().and_then(|i| i.title.clone()),
        Some("From the file".into())
    );
    assert_eq!(
        file.secrets
            .as_ref()
            .and_then(|s| s.get("leads.webhook_token").cloned()),
        Some(Some(SECRET_STORED.into()))
    );
}

#[test]
fn the_applied_registry_and_agents_survive_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::for_tests();
    config.state_dir = Some(dir.path().to_path_buf());
    let state = test_state_with(config);
    let text = format!(
        "version: 2\nregistry: {{servers: [{SERVER}]}}\nagents: {{default_ceiling: traffic}}\n"
    );
    let file = crate::mocks::config_file::parse(&text).expect("doc");
    let report = apply(
        &state,
        file,
        &ApplyOptions {
            prune: false,
            dry_run: false,
            principal: "t".into(),
            base: None,
        },
    );
    assert_eq!(report["problems"], json!([]), "{report}");
    assert!(dir.path().join(APPLIED_FILE).exists());
    let mut config = Config::for_tests();
    config.state_dir = Some(dir.path().to_path_buf());
    let again = test_state_with(config);
    crate::mocks::load_startup(&again);
    assert_eq!(again.mocks.file_registry().0.len(), 1);
    assert_eq!(
        again.mocks.file_agents().default_ceiling,
        Some(agents::Ceiling::Traffic)
    );
}

#[tokio::test]
async fn the_mocks_apply_route_still_takes_a_version_2_document() {
    let state = test_state_with(Config::for_tests());
    let app = test_app_with(state.clone());
    let doc = format!("version: 2\nhealth: {{initial: unhealthy}}\nmocks:\n  - name: orders\n    spec_inline: {SPEC}\n");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_rustybin/mocks/apply")
                .header("content-type", "application/yaml")
                .body(axum::body::Body::from(doc))
                .expect("request"),
        )
        .await
        .expect("apply");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["created"], json!(["orders"]));
    assert!(
        state.health.is_healthy(),
        "the mocks route applies the mocks only"
    );
}
