//! The audit log tool (access control step 14): `list_audit`, the same
//! filters as `GET /_rustybin/audit` (`crate::audit`). Names people typed
//! (users, mocks, tests, a token's subject, the request id a client sent)
//! are third-party text, answered under `untrusted_*` and never in the
//! summary.

use chrono::Utc;
use serde_json::{json, Value};

use super::tools::{arg_str, err, ok};
use crate::audit::{Entry, Filter, FilterQuery};
use crate::mcp::core::CallCtx;

/// An entry as the tool answers it: kinds, actions and results as they
/// are; typed names under `untrusted_*`.
fn entry_view(e: &Entry) -> Value {
    let mut actor = json!({ "kind": e.actor.kind });
    if let Some(id) = &e.actor.id {
        actor["id"] = json!(id);
    }
    if let Some(a) = &e.actor.agent {
        actor["agent"] = json!(a);
    }
    json!({
        "id": e.id,
        "time": e.time,
        "actor": actor,
        "untrusted_actor_name": e.actor.name,
        "via": e.via,
        "action": e.action,
        "event": e.event,
        "untrusted_targets": e.targets,
        "result": e.result,
        "untrusted_request_id": e.request_id,
        "address": e.address,
    })
}

/// `list_audit`: the audit log, newest first, filtered and paged.
pub fn list_audit(ctx: &CallCtx, args: &Value) -> Value {
    let s = |k: &str| arg_str(args, k).map(str::to_string);
    let q = FilterQuery {
        actor: s("actor"),
        action: s("action"),
        target: s("target"),
        since: s("since"),
        until: s("until"),
        result: s("result"),
        cursor: s("cursor"),
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|l| l.to_string())
            .or_else(|| Some("50".into())),
    };
    let filter = match Filter::parse(&q, Utc::now()) {
        Ok(f) => f,
        Err(e) => return err(e),
    };
    let (entries, next) = ctx.app.audit.list(&filter);
    let refused = entries
        .iter()
        .filter(|e| e.result.outcome == "refused")
        .count();
    let summary = format!(
        "{} audit entr{} (newest first){}{}",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        if refused > 0 {
            format!(", {refused} refused")
        } else {
            String::new()
        },
        if next.is_some() {
            "; more with cursor = next_cursor"
        } else {
            ""
        }
    );
    ok(
        ctx,
        summary,
        json!({
            "entries": entries.iter().map(|e| entry_view(e)).collect::<Vec<_>>(),
            "next_cursor": next.map(|n| n.to_string()),
            "log": ctx.app.audit.status(),
        }),
    )
}
