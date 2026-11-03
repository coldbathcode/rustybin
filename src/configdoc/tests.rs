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
                ..Default::default()
            },
            ceiling: Some("read-only".into()),
            ..Default::default()
        }],
    }));
    assert!(problems.is_empty(), "{problems:?}");
    state.mocks.set_file_agents(policy);
    state
        .saved_tests
        .schedules
        .put(
            crate::replay::schedule::Schedule {
                name: "nightly".into(),
                description: Some("Every order test".into()),
                names: Vec::new(),
                prefix: Some("order".into()),
                every: None,
                at: Some("02:30".into()),
                enabled: true,
                webhook: Some("https://hooks.example.com/rustybin".into()),
                notify_recovery: true,
            },
            false,
        )
        .expect("schedule");
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
            untrusted: false,
            caller: None,
            base_url: None,
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
            untrusted: false,
            caller: None,
            base_url: None,
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
        untrusted: false,
        caller: None,
        base_url: None,
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
            untrusted: false,
            caller: None,
            base_url: None,
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

fn untrusted(prune: bool, base: Option<&Path>) -> ApplyOptions {
    ApplyOptions {
        prune,
        dry_run: false,
        principal: "git:test".into(),
        base: base.map(Path::to_path_buf),
        untrusted: true,
        caller: None,
        base_url: None,
    }
}

#[cfg(unix)]
#[test]
fn a_linked_specs_directory_or_document_is_refused() {
    let root = tempfile::tempdir().expect("tmp");
    let repo = root.path().join("repo");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&repo).expect("repo");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("settings.json"), "{}").expect("file");
    std::os::unix::fs::symlink(&outside, repo.join("specs")).expect("link");
    let state = state_with_everything();
    let export = super::export(&state, SecretsMode::Refs, true);
    let e = write_dir_within(&export, &repo, &repo).expect_err("refused");
    assert!(e.contains("symbolic link"), "{e}");
    assert!(
        outside.join("settings.json").exists(),
        "nothing outside was touched"
    );
    // A linked document.
    std::fs::remove_file(repo.join("specs")).expect("unlink");
    std::os::unix::fs::symlink(outside.join("settings.json"), repo.join(DOCUMENT_NAME))
        .expect("link");
    let e = write_dir_within(&export, &repo, &repo).expect_err("refused");
    assert!(e.contains("symbolic link"), "{e}");
    assert_eq!(
        std::fs::read_to_string(outside.join("settings.json")).expect("read"),
        "{}"
    );
}

#[test]
fn a_pulled_document_does_not_read_the_environment_or_the_host() {
    std::env::set_var("RUSTYBIN_TEST_PULLED_SECRET", "env-value-123456");
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(
        dir.path().join(DOCUMENT_NAME),
        "version: 2\nmocks:\n  - name: leak\n    spec_inline: |\n      openapi: 3.0.3\n      info: {title: \"${RUSTYBIN_TEST_PULLED_SECRET}\", version: \"1\"}\n      paths: {}\n",
    )
    .expect("doc");
    let (file, _) =
        crate::sourcecontrol::read_document(&dir.path().join(DOCUMENT_NAME)).expect("read");
    let text = serde_json::to_string(&file.mocks).expect("json");
    assert!(text.contains("${RUSTYBIN_TEST_PULLED_SECRET}"), "{text}");
    assert!(!text.contains("env-value-123456"));
    // Secrets: only the variable of the secret itself; never a value.
    let state = test_state_with(Config::for_tests());
    let mut secrets = BTreeMap::new();
    secrets.insert(
        "leads.webhook_token".to_string(),
        Some("${RUSTYBIN_TEST_PULLED_SECRET}".to_string()),
    );
    secrets.insert(
        "assist.openai_key".to_string(),
        Some("sk-a-literal-value".to_string()),
    );
    let doc = MocksFile {
        version: 2,
        secrets: Some(secrets),
        ..Default::default()
    };
    let report = apply(&state, doc, &untrusted(false, None));
    let problems = report["problems"].to_string();
    assert!(
        problems.contains("may only read RUSTYBIN_LEADS_WEBHOOK_TOKEN"),
        "{problems}"
    );
    assert!(problems.contains("never a value"), "{problems}");
    assert_eq!(
        state.settings.secret_state("leads.webhook_token")["set"],
        false
    );
    assert_eq!(
        state.settings.secret_state("assist.openai_key")["set"],
        false
    );
    // A spec file outside the working copy is refused.
    let mut mocks = Vec::new();
    mocks.push(
        serde_json::from_value::<MockDoc>(json!({ "name": "host", "spec": "../../etc/passwd" }))
            .expect("doc"),
    );
    mocks.push(
        serde_json::from_value::<MockDoc>(json!({ "name": "abs", "spec": "/etc/hosts" }))
            .expect("doc"),
    );
    let doc = MocksFile {
        version: 2,
        mocks: Some(mocks),
        ..Default::default()
    };
    let report = apply(&state, doc, &untrusted(false, Some(dir.path())));
    let problems = report["problems"].to_string();
    assert!(
        problems.contains("mocks.host: spec ../../etc/passwd"),
        "{problems}"
    );
    assert!(
        problems.contains("mocks.abs: spec /etc/hosts"),
        "{problems}"
    );
    assert!(state.mocks.get("host").is_none() && state.mocks.get("abs").is_none());
    std::env::remove_var("RUSTYBIN_TEST_PULLED_SECRET");
}

#[test]
fn a_secret_diff_shows_references_never_values() {
    let mut before = BTreeMap::new();
    before.insert(
        "assist.openai_key".to_string(),
        Some("${RUSTYBIN_ASSIST_OPENAI_KEY}".to_string()),
    );
    let after = before.clone();
    let b = MocksFile {
        version: 2,
        secrets: Some(before.clone()),
        ..Default::default()
    };
    let a = MocksFile {
        version: 2,
        secrets: Some(after),
        ..Default::default()
    };
    assert!(
        diff(&b, &a, None).is_empty(),
        "the same reference is no change"
    );
    let mut literal = before;
    literal.insert(
        "assist.openai_key".to_string(),
        Some("sk-secret-value-123".to_string()),
    );
    let a = MocksFile {
        version: 2,
        secrets: Some(literal),
        ..Default::default()
    };
    let changes = serde_json::to_string(&diff(&b, &a, None)).expect("json");
    assert!(!changes.contains("sk-secret-value-123"), "{changes}");
    assert!(changes.contains("a value"), "{changes}");
}

#[test]
fn pruning_secrets_never_removes_the_password_hash() {
    let state = test_state_with(Config::for_tests());
    let hash = crate::login::hash_password("a-new-password-1").expect("hash");
    let mut s = BTreeMap::new();
    s.insert(crate::login::HASH_KEY.to_string(), Some(hash));
    state
        .settings
        .update(BTreeMap::new(), s, "test")
        .expect("set");
    let doc = MocksFile {
        version: 2,
        secrets: Some(BTreeMap::new()),
        ..Default::default()
    };
    let report = apply(&state, doc, &untrusted(true, None));
    assert_eq!(report["results"]["secrets"]["unset"], json!([]), "{report}");
    assert_eq!(
        state.settings.secret_state(crate::login::HASH_KEY)["set"],
        true
    );
}

#[test]
fn a_public_instance_refuses_to_clear_the_password_hash() {
    let mut c = Config::for_tests();
    c.public_mode = true;
    let state = test_state_with(c);
    let hash = crate::login::hash_password("a-new-password-1").expect("hash");
    let mut s = BTreeMap::new();
    s.insert(crate::login::HASH_KEY.to_string(), Some(hash));
    state
        .settings
        .update(BTreeMap::new(), s, "test")
        .expect("set");
    assert!(state
        .settings
        .unset(crate::login::HASH_KEY, "test")
        .is_err());
    let mut clear = BTreeMap::new();
    clear.insert(crate::login::HASH_KEY.to_string(), None);
    assert!(state
        .settings
        .update(BTreeMap::new(), clear, "test")
        .is_err());
    assert_eq!(
        state.settings.secret_state(crate::login::HASH_KEY)["set"],
        true
    );
}

#[test]
fn harness_secrets_export_as_references_and_apply_back() {
    let scenario = json!({
        "name": "s",
        "identities": { "api_keys": { "values": ["key-aaaa", "key-bbbb"] },
                        "oauth": { "client_id": "c", "client_secret": "shh-very-secret" } },
        "request": { "headers": { "Authorization": "Bearer tok", "X-Plain": "ok" } }
    });
    let exported = crate::harness::report::redact_scenario_with(&scenario, SECRET_STORED);
    let text = exported.to_string();
    for secret in ["key-aaaa", "shh-very-secret", "Bearer tok"] {
        assert!(!text.contains(secret), "{text}");
    }
    assert!(text.contains("\"X-Plain\":\"ok\""));
    let mut back = exported.clone();
    fill_stored(&mut back, Some(&scenario), "").expect("filled from the saved one");
    assert_eq!(back, scenario);
    let mut orphan = exported;
    assert!(fill_stored(&mut orphan, None, "").is_err());
}

/// The YAML a document renders to is pinned byte for byte: a change of
/// YAML library must not churn exports and the diffs of git-backed
/// instances (`tests/snapshots/export.yaml`; regenerate with
/// `RUSTYBIN_UPDATE_SNAPSHOTS=1` only on purpose).
#[test]
fn the_yaml_of_an_export_is_pinned() {
    let state = state_with_everything();
    let mut file = super::export(&state, SecretsMode::Refs, true).file;
    // The parts that change per run (times, generated ids) are fixed.
    if let Some(instance) = file.instance.as_mut() {
        instance.title = Some("Snapshot: \"quotes\", colons: and # hashes".into());
    }
    // Users and custom roles (an instance with the password sign-in).
    file.roles = Some(vec![crate::rbac::CustomRole {
        name: "catalog-reader".into(),
        description: "Reads mocks and traffic".into(),
        permissions: vec!["mocks.read".into(), "traffic.read".into()],
    }]);
    file.users = Some(vec![
        super::UserDoc {
            name: "payments".into(),
            roles: vec!["admin".into()],
            ..Default::default()
        },
        super::UserDoc {
            name: "alice".into(),
            display_name: Some("Alice Doe".into()),
            email: Some("alice@example.com".into()),
            roles: vec!["editor".into(), "catalog-reader".into()],
            disabled: false,
            ..Default::default()
        },
        super::UserDoc {
            name: "bob".into(),
            roles: vec!["viewer".into()],
            disabled: true,
            ..Default::default()
        },
    ]);
    let mut text = to_yaml(&file);
    // Plus the shapes a spec and a scenario take (multiline text, numbers,
    // booleans, nulls, keys that need quoting, empty collections).
    let extra = json!({
        "multiline": "line one\nline two\n",
        "number": 1.5,
        "integer": 42,
        "negative": -7,
        "flag": true,
        "nothing": null,
        "on": "yes",
        "1234": "numeric key",
        "list": ["a", "b: c", "- d", "", " leading space"],
        "empty_list": [],
        "empty_map": {},
        "nested": { "deep": { "deeper": [1, { "x": "y" }] } },
        "unicode": "caf\u{e9} \u{2192}",
        "date_like": "2026-11-02",
        "version_like": "1.0",
        "octal_like": "0755",
    });
    text.push_str("---\n");
    text.push_str(&serde_yaml::to_string(&extra).expect("yaml"));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/export.yaml");
    if std::env::var("RUSTYBIN_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &text).expect("write snapshot");
    }
    let pinned = std::fs::read_to_string(&path).expect("tests/snapshots/export.yaml");
    assert_eq!(text, pinned, "the YAML output changed");
}

/// A document that names an existing mock it cannot load never deletes it:
/// not when pruning, not in the dry run, not on disk.
#[test]
fn a_failed_definition_is_never_pruned() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut c = Config::for_tests();
    c.mocks_state_dir = Some(dir.path().display().to_string());
    let state = test_state_with(c);
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
    assert!(dir.path().join("orders.json").exists());
    // `orders` with a spec that cannot be read here (a file path, no base),
    // and `broken` whose inline spec does not parse.
    let doc = || {
        crate::mocks::config_file::parse(
            "version: 2\nmocks:\n  - name: orders\n    spec: missing.yaml\n  - name: broken\n    spec_inline: \"{not json\"\n",
        )
        .expect("parse")
    };
    let mut opts = ApplyOptions {
        prune: true,
        dry_run: true,
        principal: "test".into(),
        base: None,
        untrusted: false,
        caller: None,
        base_url: None,
    };
    let preview = apply(&state, doc(), &opts);
    assert_eq!(
        preview["results"]["mocks"]["would_delete"],
        json!([]),
        "{preview}"
    );
    opts.dry_run = false;
    let applied = apply(&state, doc(), &opts);
    assert_eq!(
        applied["results"]["mocks"]["deleted"],
        json!([]),
        "{applied}"
    );
    assert!(
        applied["problems"].to_string().contains("mocks.orders"),
        "{applied}"
    );
    assert!(state.mocks.get("orders").is_some(), "the mock survives");
    assert!(
        dir.path().join("orders.json").exists(),
        "and its saved copy"
    );
    // The REST apply of the mocks API follows the same rule.
    let svc_report = crate::mocks::store::MockStore::apply_keeping(
        &state.mocks,
        Vec::new(),
        Origin::Api,
        Some(Origin::Api),
        &["orders".to_string()],
    );
    assert!(svc_report.deleted.is_empty());
    assert!(state.mocks.get("orders").is_some());
    // A document from git whose `orders` entry does not even deserialize
    // (a typo in a field name) still names it: never pruned.
    let (lenient, failed) = crate::mocks::config_file::parse_lenient(
        "version: 2\nmocks:\n  - name: orders\n    enabeld: false\n",
    )
    .expect("lenient");
    assert_eq!(failed.len(), 1);
    opts.untrusted = true;
    let applied = apply(&state, lenient, &opts);
    assert_eq!(
        applied["results"]["mocks"]["deleted"],
        json!([]),
        "{applied}"
    );
    assert!(state.mocks.get("orders").is_some(), "still there");
}

/// The saved state after a burst of concurrent changes is the last one
/// acknowledged, across a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_changes_persist_the_last_acknowledged_state() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut c = Config::for_tests();
    c.mocks_state_dir = Some(dir.path().display().to_string());
    let state = test_state_with(c.clone());
    state.mocks.apply(
        vec![Definition {
            name: "orders".into(),
            source: SPEC.into(),
            settings: Default::default(),
            enabled: true,
        }],
        Origin::Api,
        None,
    );
    let mut tasks = Vec::new();
    for i in 0..250 {
        let s = state.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            s.mocks.set_enabled("orders", i % 2 == 0).is_ok()
        }));
    }
    for t in tasks {
        assert!(t.await.expect("join"));
    }
    let last = state.mocks.get("orders").expect("mock").enabled;
    let again = test_state_with(c);
    let reloaded = again.mocks.get("orders").expect("reloaded");
    assert_eq!(reloaded.enabled, last, "the saved copy is the last state");
}

/// A preview validates like the apply: what the apply refuses, the dry run
/// reports, and it changes nothing.
#[test]
fn a_preview_refuses_what_the_apply_refuses() {
    let state = test_state_with(Config::for_tests());
    let doc = || {
        crate::mocks::config_file::parse(
            "version: 2\nsettings:\n  console.title: 42\nmocks:\n  - name: broken\n    spec_inline: \"{not json\"\n",
        )
        .expect("parse")
    };
    let opts = |dry_run| ApplyOptions {
        prune: false,
        dry_run,
        principal: "test".into(),
        base: None,
        untrusted: false,
        caller: None,
        base_url: None,
    };
    let preview = apply(&state, doc(), &opts(true));
    assert_eq!(preview["ok"], false, "{preview}");
    let p = preview["problems"].to_string();
    assert!(p.contains("console.title"), "{p}");
    assert!(p.contains("mocks.broken"), "{p}");
    assert!(
        state.settings.string("console.title").is_none(),
        "nothing changed"
    );
    assert!(state.mocks.get("broken").is_none());
    let applied = apply(&state, doc(), &opts(false));
    assert_eq!(applied["ok"], false);
    for key in ["console.title", "mocks.broken"] {
        assert!(applied["problems"].to_string().contains(key), "{applied}");
    }
    // A locked setting is reported by the preview too.
    let mut c = Config::for_tests();
    c.settings_lock = true;
    c.console_title = Some("From the environment".into());
    let locked = test_state_with(c);
    let doc = crate::mocks::config_file::parse("version: 2\nsettings:\n  console.title: Other\n")
        .expect("parse");
    let preview = apply(&locked, doc, &opts(true));
    assert_eq!(preview["ok"], false, "{preview}");
    assert!(
        preview["problems"].to_string().contains("locked"),
        "{preview}"
    );
}

/// A document from the repository never repoints where the instance sends
/// requests: the egress and assist settings, a test's gateway and a
/// scenario's stored secret for new targets are refused, and reported.
#[test]
fn an_untrusted_document_cannot_repoint_outbound_requests() {
    let state = crate::test_support::test_state();
    let doc = crate::mocks::config_file::parse(
        "version: 2\nsettings:\n  egress.allow: [attacker.example]\n  assist.base_url: https://attacker.example/v1\n  console.title: From git\ntests:\n  - name: leak\n    gateway: https://attacker.example\n    request: {method: GET, path: /get}\n    assert: [{status: 200}]\n",
    )
    .expect("parse");
    let opts = ApplyOptions {
        prune: true,
        dry_run: false,
        principal: "git:test".into(),
        base: None,
        untrusted: true,
        caller: None,
        base_url: None,
    };
    let r = apply(&state, doc, &opts);
    let problems = r["problems"].to_string();
    assert!(problems.contains("settings.egress.allow"), "{r}");
    assert!(problems.contains("settings.assist.base_url"), "{r}");
    assert!(problems.contains("tests.leak"), "{r}");
    assert_eq!(state.settings.get("console.title").0, json!("From git"));
    assert_ne!(
        state.settings.get("egress.allow").0,
        json!(["attacker.example"])
    );
    assert!(state.saved_tests.get("leak").is_none());

    // The same document from a trusted caller (the admin API) applies.
    let trusted = ApplyOptions {
        untrusted: false,
        ..opts
    };
    let doc =
        crate::mocks::config_file::parse("version: 2\nsettings:\n  egress.allow: [gw.example]\n")
            .expect("parse");
    let r = apply(&state, doc, &trusted);
    assert_eq!(r["problems"], json!([]), "{r}");
    assert_eq!(state.settings.get("egress.allow").0, json!(["gw.example"]));
    // A pull that carries the value the instance already has is quiet.
    let doc =
        crate::mocks::config_file::parse("version: 2\nsettings:\n  egress.allow: [gw.example]\n")
            .expect("parse");
    let r = apply(
        &state,
        doc,
        &ApplyOptions {
            untrusted: true,
            ..trusted
        },
    );
    assert_eq!(r["problems"], json!([]), "{r}");
}

/// A prune deletes the instance's own API mocks the document does not name,
/// never a visitor's, an API key's or a running gateway scenario's: not in
/// the dry run, not in the apply, not through the mocks API.
#[test]
fn a_prune_leaves_visitors_keys_and_scenarios_mocks_alone() {
    use crate::mocks::visits::{EmailInput, Owner};
    let mut c = Config::for_tests();
    c.self_service.enabled = true;
    let state = test_state_with(c);
    let store = &state.mocks;
    let compiled = || store.compile(SPEC, Default::default()).expect("compile");
    let (_, hash) = store
        .visits()
        .expect("self-service")
        .start("ip:192.0.2.1/s", &EmailInput::default())
        .expect("visit");
    store
        .create_owned(
            "visitor-mock",
            compiled(),
            Origin::Api,
            Owner::Visit(hash),
            true,
            true,
            false,
        )
        .expect("visitor");
    store
        .create_owned(
            "key-mock",
            compiled(),
            Origin::Api,
            Owner::Key("k1".into()),
            true,
            true,
            false,
        )
        .expect("key");
    store
        .create_scratch("scn-run", compiled(), std::time::Duration::from_secs(600))
        .expect("scratch");
    store
        .create("instance-mock", compiled(), Origin::Api, true, true, false)
        .expect("instance");
    let kept = ["visitor-mock", "key-mock", "scn-run"];
    let doc = || crate::mocks::config_file::parse("version: 2\nmocks: []\n").expect("parse");
    let mut opts = ApplyOptions {
        prune: true,
        dry_run: true,
        principal: "test".into(),
        base: None,
        untrusted: false,
        caller: None,
        base_url: None,
    };
    let preview = apply(&state, doc(), &opts);
    assert_eq!(
        preview["results"]["mocks"]["would_delete"],
        json!(["instance-mock"]),
        "{preview}"
    );
    // The mocks API's dry run says the same.
    let svc = crate::mocks::service::Svc::from_request(
        &state,
        &axum::http::HeaderMap::new(),
        &axum::http::Extensions::new(),
        &"/_rustybin/mocks/apply".parse().expect("uri"),
    );
    let dry = svc
        .apply_dry_run("version: 2\nmocks: []\n", true)
        .expect("dry run");
    assert_eq!(dry["would_delete"], json!(["instance-mock"]), "{dry}");
    let report = svc.apply("version: 2\nmocks: []\n", true).expect("apply");
    assert_eq!(report["deleted"], json!(["instance-mock"]), "{report}");
    opts.dry_run = false;
    let applied = apply(&state, doc(), &opts);
    assert_eq!(
        applied["results"]["mocks"]["deleted"],
        json!([]),
        "{applied}"
    );
    for name in kept {
        assert!(store.get(name).is_some(), "{name} was pruned");
    }
}

/// A gateway scenario's scratch mock (it holds the run's credential and ends
/// with the run) is never in an export, nor in the mocks API's.
#[test]
fn an_export_leaves_out_a_scenarios_scratch_mock() {
    let state = test_state_with(Config::for_tests());
    let compiled = || {
        state
            .mocks
            .compile(SPEC, Default::default())
            .expect("compile")
    };
    state
        .mocks
        .create_scratch("scn-run", compiled(), std::time::Duration::from_secs(600))
        .expect("scratch");
    state
        .mocks
        .create("orders", compiled(), Origin::Api, true, true, false)
        .expect("orders");
    let yaml = export(&state, SecretsMode::Refs, false).yaml();
    assert!(yaml.contains("name: orders"), "{yaml}");
    assert!(!yaml.contains("scn-run"), "{yaml}");
    let svc = crate::mocks::service::Svc::from_request(
        &state,
        &axum::http::HeaderMap::new(),
        &axum::http::Extensions::new(),
        &"/_rustybin/mocks/export".parse().expect("uri"),
    );
    let mocks_yaml = svc.export_all();
    assert!(mocks_yaml.contains("orders"), "{mocks_yaml}");
    assert!(!mocks_yaml.contains("scn-run"), "{mocks_yaml}");
}
