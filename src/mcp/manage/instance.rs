//! Instance tools: what this Rustybin is, the route catalogue, the health
//! toggle, plan usage, key usage and the flaky counters.

use axum::http::Method;
use serde_json::{json, Value};

use super::access::Access;
use super::testing::in_process;
use super::tools::{arg_str, arg_usize, err, ok};
use crate::catalog::{self, BodyDef};
use crate::mcp::core::CallCtx;

/// Where the documentation lives.
pub const DOCS_URL: &str = "https://docs.rustybin.dev";

/// `get_instance`.
pub fn get_instance(ctx: &CallCtx, access: &Access) -> Value {
    let state = &ctx.app;
    let config = &state.config;
    let base = access.links.base_url();
    let categories: Vec<Value> = catalog::grouped()
        .into_iter()
        .map(|(name, eps)| json!({ "name": name, "endpoints": eps.len() }))
        .collect();
    let mut self_service = crate::mocks::selfservice::info(config);
    if let Value::Object(m) = &mut self_service {
        m.insert(
            "paused".into(),
            json!(state.mocks.visits().map(|v| v.paused())),
        );
    }
    let structured = json!({
        "instance_id": config.instance_id,
        "version": crate::control::version_info(),
        "modes": {
            "public": config.public_mode,
            "shared": state.keys.shared(),
            "hosted": config.hosted_mode,
            "self_service": access.self_service,
            "require_key": state.keys.enabled() && !state.keys.shared(),
        },
        "control_auth": config.control_auth.as_str(),
        "admin_token_configured": config.admin_token.is_some(),
        "caller": access.describe(),
        "limits": {
            "plan": config.limits.public_view(),
            "mocks": crate::mocks::service::limits_json(&state.mocks),
            "body_limit_bytes": config.body_limit,
        },
        "config": config.public_view(),
        "self_service": self_service,
        "mocks": {
            "count": state.mocks.len(),
            "persistent": state.mocks.persist_dir().is_some(),
            "host_domain": config.mocks_host,
            "load_errors": state.mocks.load_errors(),
        },
        "catalogue": { "categories": categories, "endpoints": catalog::all().len() },
        "urls": {
            "base": base,
            "console": format!("{base}/ui/"),
            "docs": DOCS_URL,
            "openapi": format!("{base}/openapi.json"),
            "mcp": format!("{base}{}", super::MANAGE_PATH),
            "mock_mcp": format!("{base}/mcp"),
            "identity_provider": format!("{base}/.well-known/openid-configuration"),
        },
        "health": state.health.is_healthy(),
    });
    let summary = format!(
        "Rustybin {} ({}), instance {}: {} routes in {} categories, {} mocks; public mode {}, \
         shared mode {}, self-service {}, control auth {}. Caller rights: {}. Console at {base}/ui/.",
        env!("CARGO_PKG_VERSION"),
        crate::control::git_sha().unwrap_or("no commit"),
        config.instance_id,
        catalog::all().len(),
        catalog::grouped().len(),
        state.mocks.len(),
        config.public_mode,
        state.keys.shared(),
        access.self_service,
        config.control_auth.as_str(),
        structured["caller"]["rights"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .unwrap_or_default(),
    );
    ok(ctx, summary, structured)
}

fn example_json(ex: &catalog::Example) -> Value {
    let body = match &ex.body {
        Some(BodyDef::Json(j)) => serde_json::from_str::<Value>(j)
            .ok()
            .unwrap_or_else(|| json!(j)),
        Some(BodyDef::Form(fields)) => json!(fields
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")),
        Some(BodyDef::Xml(x)) => json!(x),
        None => Value::Null,
    };
    json!({
        "name": ex.name,
        "method": ex.method,
        "path": ex.path,
        "headers": ex.headers.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
        "body": body,
    })
}

/// `find_endpoints`.
pub fn find_endpoints(ctx: &CallCtx, args: &Value) -> Value {
    let query = arg_str(args, "query").unwrap_or("").trim().to_lowercase();
    let category = arg_str(args, "category")
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let limit = arg_usize(args, "limit").unwrap_or(25).clamp(1, 100);
    if query.is_empty() && category.is_none() {
        return err("give a query and/or a category (get_instance lists the categories)");
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<(usize, &catalog::Endpoint)> = catalog::all()
        .iter()
        .filter(|e| category.is_none_or(|c| e.category.eq_ignore_ascii_case(c)))
        .filter_map(|e| {
            if words.is_empty() {
                return Some((0, e));
            }
            let hay =
                format!("{} {} {} {}", e.path, e.summary, e.description, e.category).to_lowercase();
            let score: usize = words
                .iter()
                .map(|w| {
                    if e.path.to_lowercase().contains(w) {
                        3
                    } else if e.summary.to_lowercase().contains(w) {
                        2
                    } else {
                        usize::from(hay.contains(w))
                    }
                })
                .sum();
            let all = words.iter().all(|w| hay.contains(w));
            (all && score > 0).then_some((score, e))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(b.1.path)));
    let total = hits.len();
    let endpoints: Vec<Value> = hits
        .iter()
        .take(limit)
        .map(|(_, e)| {
            json!({
                "path": e.path,
                "methods": e.expanded_methods(),
                "category": e.category,
                "summary": e.summary,
                "description": e.description,
                "protocol": format!("{:?}", e.protocol).to_lowercase(),
                "example": e.examples.first().map(example_json),
            })
        })
        .collect();
    let summary = if total == 0 {
        format!("No endpoint matches {query:?}.")
    } else {
        format!(
            "{total} endpoint(s) match{}; showing {}: {}",
            if category.is_some() {
                " in the category"
            } else {
                ""
            },
            endpoints.len(),
            endpoints
                .iter()
                .filter_map(|e| e["path"].as_str())
                .take(10)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    ok(
        ctx,
        summary,
        json!({ "total": total, "endpoints": endpoints }),
    )
}

/// `get_health`.
pub fn get_health(ctx: &CallCtx) -> Value {
    let healthy = ctx.app.health.is_healthy();
    ok(
        ctx,
        format!(
            "/health answers {} ({})",
            if healthy { 200 } else { 503 },
            if healthy { "healthy" } else { "unhealthy" }
        ),
        json!({ "healthy": healthy, "status": if healthy { 200 } else { 503 } }),
    )
}

/// `set_health` (admin).
pub fn set_health(ctx: &CallCtx, args: &Value) -> Value {
    let Some(healthy) = args.get("healthy").and_then(Value::as_bool) else {
        return err("missing required argument: healthy (true or false)");
    };
    ctx.app.health.set(healthy);
    ok(
        ctx,
        format!(
            "/health now answers {}",
            if healthy {
                "200 (healthy)"
            } else {
                "503 (unhealthy)"
            }
        ),
        json!({ "healthy": healthy }),
    )
}

/// `get_usage`.
pub fn get_usage(ctx: &CallCtx, access: &Access) -> Value {
    let state = &ctx.app;
    let mut usage = match &access.key {
        Some(record) => crate::keys::key_usage(state, record),
        None => {
            let limiter = &state.limits;
            let key = (limiter.config().scope == crate::limits::Scope::Session)
                .then(|| access.caller.session.clone());
            let mut u = limiter.usage(key.as_deref());
            u["instance_id"] = json!(state.config.instance_id);
            u["control_auth"] = json!(state.config.control_auth.as_str());
            u
        }
    };
    if let Value::Object(m) = &mut usage {
        m.insert("test_session".into(), json!(access.test_session()));
    }
    let active = usage
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let summary = if !active {
        "No plan limits are active on this instance (every request is admitted).".to_string()
    } else {
        format!(
            "Plan {} ({} scope): requests {} used of {}, egress {} bytes used of {}; period resets in {} s.",
            usage["plan"].as_str().unwrap_or("?"),
            usage["scope"].as_str().unwrap_or("?"),
            usage["requests"]["used"],
            usage["requests"]["limit"],
            usage["egress"]["used_bytes"],
            usage["egress"]["limit_bytes"],
            usage["period"]["resets_in_secs"],
        )
    };
    ok(ctx, summary, usage)
}

/// `get_key_usage` (admin, shared mode).
pub async fn get_key_usage(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let state = &ctx.app;
    if !state.keys.shared() {
        return err("this instance does not run in shared mode (no per-key usage)");
    }
    let day = match arg_str(args, "day")
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        None => state.keys.today(),
        Some(raw) => match chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => return err("invalid day: expected YYYY-MM-DD"),
        },
    };
    let limit = arg_usize(args, "limit").unwrap_or(100).clamp(1, 500);
    let after = arg_str(args, "after")
        .map(str::trim)
        .filter(|a| !a.is_empty());
    if after.is_some_and(|a| !crate::session::is_valid_session(a)) {
        return err("invalid after: expected a key id");
    }
    match state.keys.usage_page(day, after, limit).await {
        Ok((rows, next)) => {
            let keys: Vec<Value> = rows
                .iter()
                .map(|t| json!({ "key_id": t.key_id, "requests": t.requests, "egress_bytes": t.egress_bytes }))
                .collect();
            let summary = format!(
                "{} key(s) with usage on {day}{}",
                keys.len(),
                if next.is_some() { " (more pages)" } else { "" }
            );
            ok(
                ctx,
                summary,
                json!({ "day": day.to_string(), "count": keys.len(), "keys": keys, "next": next }),
            )
        }
        Err(e) => err(format!("the quota store is unavailable: {e}")),
    }
}

/// `reset_flaky`: through the data-plane route, as the caller.
pub async fn reset_flaky(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let scope = arg_str(args, "scope").unwrap_or("session");
    if !matches!(scope, "session" | "all") {
        return err("scope must be session or all");
    }
    if scope == "all" && !access.is_admin() {
        return err(format!(
            "resetting everyone's counters needs admin rights: {}",
            access.denial(super::access::Right::Admin)
        ));
    }
    let session = arg_str(args, "session");
    let path = format!("/flaky/reset?scope={scope}");
    match in_process(
        ctx,
        access,
        Method::POST,
        &path,
        Vec::new(),
        None,
        session,
        scope == "all",
    )
    .await
    {
        Ok(a) if a.status == 200 => {
            let body = a.json().unwrap_or(Value::Null);
            ok(
                ctx,
                format!(
                    "Reset {} flaky counter(s) ({} scope{})",
                    body["counters_removed"],
                    scope,
                    if scope == "session" {
                        format!(", session {}", a.session)
                    } else {
                        String::new()
                    }
                ),
                body,
            )
        }
        Ok(a) => err(format!(
            "the reset answered {}: {}",
            a.status,
            super::testing::short(&a.body)
        )),
        Err(e) => e,
    }
}

/// `list_sessions` (admin): the console's sign-in sessions since the last
/// restart (the password sign-in must be on).
pub fn list_sessions(ctx: &CallCtx, _access: &Access) -> Value {
    let config = &ctx.app.config;
    if !crate::login::enabled(config) {
        return err(format!(
            "the console sign-in is off (control auth {}): there are no sign-in sessions",
            config.control_auth.as_str()
        ));
    }
    let sessions = config.console_tokens.sessions();
    let summary = format!(
        "{} sign-in session(s) (generation {}, at most {} listed)",
        sessions.len(),
        config.console_tokens.generation(),
        crate::login::MAX_SESSIONS
    );
    ok(
        ctx,
        summary,
        json!({
            "generation": config.console_tokens.generation(),
            "count": sessions.len(),
            "capacity": crate::login::MAX_SESSIONS,
            "sessions": sessions,
        }),
    )
}

/// `end_session` (admin): end one sign-in session by id, or every one
/// (`all`, confirmed: it signs everyone out, the caller's own console too).
pub async fn end_session(ctx: &CallCtx, _access: &Access, args: &Value, params: &Value) -> Value {
    let config = &ctx.app.config;
    if !crate::login::enabled(config) {
        return err(format!(
            "the console sign-in is off (control auth {}): there are no sign-in sessions",
            config.control_auth.as_str()
        ));
    }
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    if all {
        let action = "sign out everywhere: every console sign-in token issued so far is refused, the user's own console included";
        if let super::tools::Confirm::Answer(v) =
            super::tools::confirm(ctx, "end_session", args, params, action).await
        {
            return v;
        }
        let n = config.console_tokens.rotate();
        return ok(
            ctx,
            format!(
                "Signed out everywhere: {n} session(s) ended (generation {})",
                config.console_tokens.generation()
            ),
            json!({ "ended": n, "generation": config.console_tokens.generation(), "note": "every sign-in token issued before now is refused" }),
        );
    }
    let Some(id) = arg_str(args, "id").filter(|i| !i.is_empty()) else {
        return err("give the session id (list_sessions), or all true");
    };
    let ended = config.console_tokens.end(id);
    if !ended {
        return err(format!("no session {id:?} (list_sessions shows them)"));
    }
    ok(
        ctx,
        format!("Ended session {id}"),
        json!({ "ended": true, "id": id }),
    )
}

/// `reload_mocks` (admin): reload the file-defined mocks
/// (`RUSTYBIN_CONFIG_FILE`, `RUSTYBIN_MOCKS_DIR`).
pub fn reload_mocks(ctx: &CallCtx, access: &Access) -> Value {
    match access.svc(&ctx.app).reload() {
        Ok(r) => {
            let problems = r["problems"].as_array().map(Vec::len).unwrap_or(0);
            ok(
                ctx,
                format!(
                    "Reloaded the file-defined mocks: {} created, {} updated, {} deleted, {} unchanged; {problems} problem(s)",
                    r["report"]["created"].as_array().map(Vec::len).unwrap_or(0),
                    r["report"]["updated"].as_array().map(Vec::len).unwrap_or(0),
                    r["report"]["deleted"].as_array().map(Vec::len).unwrap_or(0),
                    r["report"]["unchanged"].as_array().map(Vec::len).unwrap_or(0)
                ),
                r,
            )
        }
        Err(e) => super::tools::service_error(&e),
    }
}
