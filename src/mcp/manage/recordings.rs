//! Recordings of a real API (`crate::recordings`): start one, send traffic
//! through its proxy URL, build a mock from it. What a recording kept is
//! third-party content (what the real API answered): under
//! `untrusted_recorded`, never in the text summary.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{confirm, err, need_str, ok, Confirm};
use crate::mcp::core::CallCtx;
use crate::recordings::{self, BuildBody, StartBody};

fn owner(ctx: &CallCtx) -> String {
    let ip = ctx.http.client_ip.as_deref().and_then(|s| s.parse().ok());
    crate::session::session_key(&ctx.http.headers, ip)
}

fn base(ctx: &CallCtx, access: &Access) -> String {
    access.svc(&ctx.app).links.base_url().to_string()
}

/// `list_recordings`.
pub fn list_recordings(ctx: &CallCtx, access: &Access) -> Value {
    let v = recordings::list(&ctx.app, &owner(ctx), &base(ctx, access));
    let n = v["recordings"].as_array().map_or(0, Vec::len);
    ok(ctx, format!("{n} recording(s) in this session."), v)
}

/// `start_recording`.
pub async fn start_recording(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, upstream) = match (need_str(args, "name"), need_str(args, "upstream")) {
        (Ok(n), Ok(u)) => (n, u),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let body = StartBody {
        name: name.to_string(),
        upstream: upstream.to_string(),
        mock: args.get("mock").and_then(Value::as_str).map(str::to_string),
    };
    match recordings::start(&ctx.app, &owner(ctx), &base(ctx, access), body).await {
        Ok(v) => {
            let allowed = v["egress"]["allowed"] == true;
            let text = format!(
                "Recording {name} started. Send requests to {} (as you would to {upstream}); then call build_mock_from_recording.{}",
                v["proxy_url"].as_str().unwrap_or("?"),
                if allowed {
                    String::new()
                } else {
                    " The outbound policy does not allow the upstream yet (purpose record): add its host to RUSTYBIN_EGRESS_ALLOW.".to_string()
                }
            );
            ok(ctx, text, v)
        }
        Err(e) => err(e.1),
    }
}

/// `get_recording`: what it kept, under `untrusted_recorded`.
pub fn get_recording(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match recordings::get_one(&ctx.app, &owner(ctx), &base(ctx, access), name) {
        Ok(mut v) => {
            if let Some(o) = v.as_object_mut() {
                if let Some(r) = o.remove("recorded") {
                    o.insert("untrusted_recorded".into(), r);
                }
            }
            let text = format!(
                "Recording {name}: {} exchange(s), {}.",
                v["exchanges"],
                v["state"].as_str().unwrap_or("?")
            );
            ok(ctx, text, v)
        }
        Err(e) => err(e.1),
    }
}

/// `stop_recording`.
pub fn stop_recording(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match recordings::stop(&ctx.app, &owner(ctx), &base(ctx, access), name) {
        Ok(v) => ok(ctx, format!("Recording {name} stopped."), v),
        Err(e) => err(e.1),
    }
}

/// `build_mock_from_recording`.
pub async fn build_mock_from_recording(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let body = BuildBody {
        mock: args.get("mock").and_then(Value::as_str).map(str::to_string),
        dry_run: args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let dry = body.dry_run;
    match recordings::build(&ctx.app, access.svc(&ctx.app), &owner(ctx), name, body).await {
        Ok(mut v) => {
            // The examples came from the real API: third-party content.
            if let Some(o) = v.as_object_mut() {
                if let Some(spec) = o.remove("spec") {
                    o.insert("untrusted_spec".into(), spec);
                }
            }
            let ops = v["operations"].as_array().map_or(0, Vec::len);
            let findings = v["personal_data"].as_array().map_or(0, Vec::len);
            let text = format!(
                "{} mock {} from recording {name}: {ops} operation(s), {} stored example(s){}.",
                if dry { "Would build" } else { "Built" },
                v["mock"].as_str().unwrap_or("?"),
                v["stored_examples"],
                if findings > 0 {
                    format!("; {findings} operation(s) answered what looks like personal data: review before publishing")
                } else {
                    String::new()
                }
            );
            ok(ctx, text, v)
        }
        Err(e) => err(e.1),
    }
}

/// `delete_recording` (confirmed).
pub async fn delete_recording(ctx: &CallCtx, args: &Value, params: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    let action = format!("delete recording {name} and what it kept (the mock it built stays)");
    if let Confirm::Answer(v) = confirm(ctx, "delete_recording", args, params, &action).await {
        return v;
    }
    match recordings::delete(&ctx.app, &owner(ctx), name) {
        Ok(v) => ok(ctx, format!("Recording {name} deleted."), v),
        Err(e) => err(e.1),
    }
}

/// The `name` property every recording tool takes.
pub fn name_prop() -> Value {
    json!({ "type": "string", "description": "The recording's name (a-z, 0-9, -)" })
}
