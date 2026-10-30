//! Configuration as code and source control tools: the whole instance as a
//! version 2 `rustybin.yaml` (export, diff, apply) and the git repository
//! (status, init, commit, push, pull, resolve, the deploy key).

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, service_error, Confirm};
use crate::configdoc::{self, ApplyOptions, SecretsMode};
use crate::mcp::core::CallCtx;
use crate::sourcecontrol::GitError;

/// `export_config`: one mock's `rustybin.yaml` (version 1), every mock the
/// caller may see (version 1, non-admin), or the whole instance (version 2,
/// admin): settings, secret references, mocks, registry, portal, harness,
/// agents and health.
pub fn export_config(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let s = access.svc(&ctx.app);
    if let Some(name) = arg_str(args, "name").filter(|n| !n.is_empty()) {
        return match s.export_one(name) {
            Ok(text) => ok(
                ctx,
                format!(
                    "rustybin.yaml (version 1) for mock {name:?} ({} bytes){}",
                    text.len(),
                    if s.readable(name)
                        .map(|snap| s.caller.reveals(&snap))
                        .unwrap_or(false)
                    {
                        ""
                    } else {
                        "; security secrets are redacted (placeholders) because the caller may not change this mock"
                    }
                ),
                json!({ "file_name": format!("{name}.rustybin.yaml"), "version": 1, "yaml": text }),
            ),
            Err(e) => service_error(&e),
        };
    }
    if !access.is_admin() {
        let text = s.export_all();
        return ok(
            ctx,
            format!(
                "rustybin.yaml (version 1: the mocks the caller may see, {} bytes); the whole instance (version 2) needs admin rights",
                text.len()
            ),
            json!({ "file_name": "rustybin.yaml", "version": 1, "yaml": text }),
        );
    }
    let secrets = match arg_str(args, "secrets").unwrap_or("refs") {
        "refs" => SecretsMode::Refs,
        "omit" => SecretsMode::Omit,
        other => return err(format!("secrets must be refs or omit, not {other:?}")),
    };
    let format = arg_str(args, "format").unwrap_or("yaml");
    if !matches!(format, "yaml" | "json") {
        return err("format must be yaml or json (the bundle is GET /_rustybin/config/export?format=bundle or `rustybin config export --format bundle`)");
    }
    let export = configdoc::export(&ctx.app, secrets, false);
    let sections = export.file.sections();
    let mocks = export.file.mocks.as_ref().map(|m| m.len()).unwrap_or(0);
    let mut structured = json!({
        "file_name": if format == "json" { "rustybin.json" } else { "rustybin.yaml" },
        "version": export.file.version,
        "secrets": match secrets { SecretsMode::Refs => "refs", SecretsMode::Omit => "omit" },
        "sections": sections,
        "mocks": mocks,
    });
    if format == "json" {
        structured["document"] = serde_json::to_value(&export.file).unwrap_or(Value::Null);
    } else {
        structured["yaml"] = json!(export.yaml());
    }
    ok(
        ctx,
        format!(
            "rustybin.yaml version {} for the whole instance: sections {}; {mocks} mock(s); secrets as {} (never values)",
            export.file.version,
            structured["sections"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                .unwrap_or_default(),
            match secrets {
                SecretsMode::Refs => "references (${VAR} or secret:stored)",
                SecretsMode::Omit => "omitted",
            }
        ),
        structured,
    )
}

/// The document argument: YAML or JSON text, or the object.
fn document_arg(args: &Value) -> Result<crate::mocks::config_file::MocksFile, Value> {
    let raw = args
        .get("document")
        .or_else(|| args.get("yaml"))
        .ok_or_else(|| {
            err("document is required: the rustybin.yaml text (YAML or JSON) or the object")
        })?;
    let value: Value = match raw {
        Value::String(text) => {
            if text.len() > 4 * 1024 * 1024 {
                return Err(err("the document is larger than 4 MiB"));
            }
            serde_yaml::from_str(text)
                .map_err(|e| err(format!("the document is not YAML or JSON: {e}")))?
        }
        Value::Object(_) => raw.clone(),
        _ => return Err(err("document must be text or an object")),
    };
    configdoc::parse_document(&value).map_err(err)
}

fn apply_summary(r: &Value, verb: &str) -> String {
    let changes = r["changes"].as_array().map(Vec::len).unwrap_or(0);
    let problems = r["problems"].as_array().map(Vec::len).unwrap_or(0);
    let lines: Vec<&str> = r["summary"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).take(8).collect())
        .unwrap_or_default();
    let mut s = format!(
        "{verb}: {changes} change(s), {problems} problem(s); sections {}",
        r["sections"]
            .as_array()
            .map(|a| a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "))
            .unwrap_or_default()
    );
    if !lines.is_empty() {
        s.push_str(": ");
        s.push_str(&lines.join("; "));
        if changes > lines.len() {
            s.push_str(&format!("; and {} more", changes - lines.len()));
        }
    }
    s
}

/// `diff_config` (admin): what applying a document would change (a dry run).
pub fn diff_config(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let file = match document_arg(args) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let opts = ApplyOptions {
        prune: arg_bool(args, "prune").unwrap_or(false),
        dry_run: true,
        principal: access.principal(),
        base: None,
    };
    let r = configdoc::apply(&ctx.app, file, &opts);
    let summary = apply_summary(&r, "Diff (nothing changed)");
    ok(ctx, summary, r)
}

/// `apply_config` (admin): apply a version 1 or 2 document; `dry_run`
/// reports only; `prune` (confirmed) removes what a present section does
/// not name.
pub async fn apply_config(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let file = match document_arg(args) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let dry_run = arg_bool(args, "dry_run").unwrap_or(false);
    let prune = arg_bool(args, "prune").unwrap_or(false);
    if prune && !dry_run {
        let action = format!(
            "apply the document and remove what its present sections ({}) do not name (prune)",
            file.sections().join(", ")
        );
        if let Confirm::Answer(v) = confirm(ctx, "apply_config", args, params, &action).await {
            return v;
        }
    }
    let opts = ApplyOptions {
        prune,
        dry_run,
        principal: access.principal(),
        base: None,
    };
    let r = configdoc::apply(&ctx.app, file, &opts);
    let verb = if dry_run {
        "Dry run (nothing changed)"
    } else if r["ok"].as_bool() == Some(true) {
        "Applied"
    } else {
        "Applied with problems"
    };
    let summary = apply_summary(&r, verb);
    if r["ok"].as_bool() == Some(false) && !dry_run {
        let mut v = err(summary);
        v["structuredContent"] = r;
        return v;
    }
    ok(ctx, summary, r)
}

// ── Source control ──────────────────────────────────────────────────

fn git_error(e: GitError) -> Value {
    let mut v = err(format!("{} ({})", e.message, e.status.as_u16()));
    let mut body = e.body();
    if let Value::Object(m) = &mut body {
        m.insert("status".into(), json!(e.status.as_u16()));
    }
    v["structuredContent"] = body;
    v
}

fn git_summary(s: &Value) -> String {
    if s["remote"].is_null() || s["remote"]["url"].is_null() {
        return format!(
            "No repository configured: set the git.remote setting (update_settings, or RUSTYBIN_CONFIG_GIT) then git_init{}",
            if s["git"]["available"].as_bool() == Some(true) {
                ""
            } else {
                "; the git binary is not installed on the instance"
            }
        );
    }
    let mut parts = vec![format!(
        "remote {} branch {}",
        s["remote"]["url"].as_str().unwrap_or("?"),
        s["branch"].as_str().unwrap_or("?")
    )];
    if s["initialised"].as_bool() != Some(true) {
        parts.push("not initialised (git_init)".into());
    }
    if let Some(head) = s["head"].as_str() {
        parts.push(format!("head {}", head.chars().take(8).collect::<String>()));
    }
    if let (Some(a), Some(b)) = (s["ahead"].as_u64(), s["behind"].as_u64()) {
        parts.push(format!("ahead {a}, behind {b}"));
    }
    if let Some(drift) = s["drift"].as_object() {
        let n = drift
            .get("changes")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        parts.push(if n == 0 {
            "the working copy matches the instance".into()
        } else {
            format!("{n} change(s) between the working copy and the instance (git_commit)")
        });
    }
    if let Some(auto) = s["auto"].as_str() {
        if auto != "off" {
            parts.push(format!("automatic mode {auto}"));
        }
    }
    parts.join("; ")
}

/// `git_status` (admin): the repository, the working copy, the drift, the
/// timeline, the last commits; `fetch` asks the remote for ahead and behind.
pub async fn git_status(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let fetch = arg_bool(args, "fetch").unwrap_or(false);
    let mut status = ctx.app.git.status(&ctx.app, fetch).await;
    if let Value::Object(m) = &mut status {
        let history = match ctx.app.git.history().await {
            Ok(commits) => json!(commits),
            Err(_) => Value::Array(Vec::new()),
        };
        m.insert("commits".into(), history);
    }
    let summary = git_summary(&status);
    ok(ctx, summary, status)
}

/// `git_init` (admin): clone the remote, or start an empty working copy.
pub async fn git_init(ctx: &CallCtx, access: &Access, _args: &Value) -> Value {
    match ctx.app.git.init(&access.principal()).await {
        Ok(v) => ok(
            ctx,
            format!(
                "Repository connected: {}",
                v["detail"].as_str().unwrap_or("the working copy is ready")
            ),
            v,
        ),
        Err(e) => git_error(e),
    }
}

/// `git_commit` (admin): export the instance into the working copy and commit.
pub async fn git_commit(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let message = match need_str(args, "message") {
        Ok(m) => m,
        Err(e) => return e,
    };
    match ctx
        .app
        .git
        .commit(&ctx.app, message, &access.principal())
        .await
    {
        Ok(v) => ok(
            ctx,
            if v["committed"].as_bool() == Some(false) {
                "Nothing to commit: the working copy already matches the instance".to_string()
            } else {
                format!(
                    "Committed {}",
                    v["detail"]
                        .as_str()
                        .unwrap_or("the instance's configuration")
                )
            },
            v,
        ),
        Err(e) => git_error(e),
    }
}

/// `git_push` (admin, confirmed: it publishes the configuration to the remote).
pub async fn git_push(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let remote = ctx
        .app
        .git
        .remote()
        .ok()
        .flatten()
        .map(|r| r.url)
        .unwrap_or_else(|| "the configured remote".to_string());
    let action = format!(
        "push branch {} to {remote} (the instance's configuration leaves this host)",
        ctx.app.git.branch()
    );
    if let Confirm::Answer(v) = confirm(ctx, "git_push", args, params, &action).await {
        return v;
    }
    match ctx.app.git.push(&access.principal()).await {
        Ok(v) => ok(
            ctx,
            v["detail"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("pushed {} to {remote}", ctx.app.git.branch())),
            v,
        ),
        Err(e) => git_error(e),
    }
}

/// `git_pull` (admin): fetch and fast-forward; `apply` applies the
/// repository's document (confirmed unless `dry_run`).
pub async fn git_pull(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let apply = arg_bool(args, "apply").unwrap_or(false);
    let dry_run = arg_bool(args, "dry_run").unwrap_or(false);
    let prune = arg_bool(args, "prune").unwrap_or(false);
    if apply && !dry_run {
        let action = format!(
            "pull the repository and apply its document to this instance{}",
            if prune {
                " (prune: the document is the whole truth for its sections)"
            } else {
                ""
            }
        );
        if let Confirm::Answer(v) = confirm(ctx, "git_pull", args, params, &action).await {
            return v;
        }
    }
    match ctx
        .app
        .git
        .pull(&ctx.app, apply, dry_run, prune, &access.principal())
        .await
    {
        Ok(v) => {
            let applied = &v["applied"];
            let summary = if applied.is_object() {
                apply_summary(
                    applied,
                    if dry_run {
                        "Pulled; dry run of the document (nothing changed)"
                    } else {
                        "Pulled and applied"
                    },
                )
            } else {
                format!(
                    "Pulled: {}{}",
                    v["detail"]
                        .as_str()
                        .unwrap_or("the working copy is up to date"),
                    if v["fast_forwarded"].as_bool() == Some(true) {
                        " (fast-forwarded)"
                    } else {
                        ""
                    }
                )
            };
            ok(ctx, summary, v)
        }
        Err(e) => git_error(e),
    }
}

/// `git_resolve` (admin, confirmed): after a diverged branch, the
/// instance's copy wins (`with: instance`) or the repository's.
pub async fn git_resolve(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let with = match need_str(args, "with") {
        Ok(w) => w,
        Err(e) => return e,
    };
    if !matches!(with, "instance" | "repository") {
        return err("with must be instance (this instance's configuration replaces the branch) or repository (the repository's document is applied here)");
    }
    let message = arg_str(args, "message").unwrap_or("");
    let action = match with {
        "instance" => "resolve the diverged branch with this instance's configuration: the remote branch is replaced (force push)",
        _ => "resolve the diverged branch with the repository's document: it is applied to this instance and the local commits are dropped",
    };
    if let Confirm::Answer(v) = confirm(ctx, "git_resolve", args, params, action).await {
        return v;
    }
    match ctx
        .app
        .git
        .resolve(&ctx.app, with, message, &access.principal())
        .await
    {
        Ok(v) => ok(
            ctx,
            format!(
                "Resolved with the {with}'s copy: {}",
                v["detail"].as_str().unwrap_or("")
            ),
            v,
        ),
        Err(e) => git_error(e),
    }
}

/// `git_deploy_key` (admin): the deploy key's public half; `generate` makes
/// one, `replace` (confirmed) replaces an existing key.
pub async fn git_deploy_key(ctx: &CallCtx, access: &Access, args: &Value, params: &Value) -> Value {
    let generate = arg_bool(args, "generate").unwrap_or(false);
    let replace = arg_bool(args, "replace").unwrap_or(false);
    if !generate {
        return match ctx.app.git.public_key() {
            Some(k) => ok(
                ctx,
                "The deploy key's public half (add it to the repository's deploy keys with write access)",
                json!({ "deploy_key": k }),
            ),
            None => ok(
                ctx,
                "No deploy key yet: call again with generate true (a token works too, set as the secret git.token in the console or the CLI)",
                json!({ "deploy_key": null }),
            ),
        };
    }
    if replace {
        let action = "replace the deploy key: the remote stops accepting the old one until the new public key is registered";
        if let Confirm::Answer(v) = confirm(ctx, "git_deploy_key", args, params, action).await {
            return v;
        }
    }
    match ctx.app.git.generate_key(replace, &access.principal()).await {
        Ok(v) => ok(
            ctx,
            "Deploy key generated: register its public half on the repository with write access",
            v,
        ),
        Err(e) => git_error(e),
    }
}
