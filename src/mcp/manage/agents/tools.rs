//! The operator's agent tools: who talks to this server, their rights,
//! revocation and sessions (admin).

use serde_json::{json, Value};

use super::{AgentError, Ceiling, Policy, Rights};
use crate::mcp::core::CallCtx;
use crate::mcp::manage::access::Access;
use crate::mcp::manage::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};

fn refused(e: AgentError) -> Value {
    match e {
        AgentError::NotFound => err("no such agent (list_agents shows the ids)"),
        AgentError::Invalid(m) => err(m),
        AgentError::Io(m) => err(format!("the agents state could not be saved: {m}")),
    }
}

/// `list_agents` (admin): every agent, the policy and the ceilings.
pub fn list_agents(ctx: &CallCtx, _access: &Access, _args: &Value) -> Value {
    let state = &ctx.app;
    let policy = Policy::of(state);
    let agents = state.agents.list(&policy);
    let revoked = agents
        .iter()
        .filter(|a| a["status"].as_str() == Some("revoked"))
        .count();
    let summary = format!(
        "{} agent(s){}; default ceiling {}, {} rule(s) from rustybin.yaml{}",
        agents.len(),
        if revoked > 0 {
            format!(" ({revoked} revoked)")
        } else {
            String::new()
        },
        policy.view()["default_ceiling"].as_str().unwrap_or("full"),
        policy.view()["rules"].as_array().map(Vec::len).unwrap_or(0),
        if state.config.control_auth.as_str() == "open" {
            "; the control plane is open, so rights are advisory"
        } else {
            ""
        }
    );
    ok(
        ctx,
        summary,
        json!({
            "agents": agents,
            "policy": policy.view(),
            "capacity": super::MAX_AGENTS,
            "persisted": state.agents.persisted(),
            "control_auth": state.config.control_auth.as_str(),
            "ceilings": super::CEILINGS,
        }),
    )
}

/// `get_agent` (admin): one agent with its calls per tool, last errors,
/// sessions, rights and recent activity.
pub fn get_agent(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    let policy = Policy::of(&ctx.app);
    let Some(mut v) = ctx.app.agents.get(id, &policy) else {
        return refused(AgentError::NotFound);
    };
    if let Some(activity) = ctx.app.agents.activity(id, &policy) {
        v["activity"] = activity;
    }
    let summary = format!(
        "Agent {} ({}): status {}, {} call(s) in total, ceiling {}{}",
        v["name"].as_str().unwrap_or("unknown"),
        v["credential"]["label"]
            .as_str()
            .or(v["credential"]["kind"].as_str())
            .unwrap_or("?"),
        v["status"].as_str().unwrap_or("?"),
        v["calls"]["total"].as_u64().unwrap_or(0),
        v["rights"]["ceiling"].as_str().unwrap_or("full"),
        v["note"]
            .as_str()
            .map(|n| format!("; note: {n}"))
            .unwrap_or_default()
    );
    ok(ctx, summary, v)
}

fn tool_list(args: &Value, key: &str) -> Result<Option<Vec<String>>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => Ok(Some(
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        )),
        Some(Value::String(s)) => Ok(Some(
            s.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        )),
        Some(_) => Err(err(format!("{key} must be a list of tool names"))),
    }
}

/// `set_agent_rights` (admin): a ceiling, allow and deny lists and a note;
/// `reset` goes back to the rules and the default.
pub fn set_agent_rights(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    let reset = arg_bool(args, "reset").unwrap_or(false);
    let ceiling = match arg_str(args, "ceiling") {
        None => None,
        Some(c) => match Ceiling::parse(c) {
            Some(c) => Some(c),
            None => {
                return err(format!(
                    "ceiling must be one of {}",
                    super::CEILINGS.join(", ")
                ))
            }
        },
    };
    let allow = match tool_list(args, "allow") {
        Ok(a) => a,
        Err(e) => return e,
    };
    let deny = match tool_list(args, "deny") {
        Ok(d) => d,
        Err(e) => return e,
    };
    let rights = if reset {
        Some(None)
    } else if ceiling.is_some() || allow.is_some() || deny.is_some() {
        Some(Some(Rights {
            ceiling,
            allow,
            deny,
        }))
    } else {
        None
    };
    let note = match args.get("note") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(n)) => Some(Some(n.clone())),
        Some(_) => return err("note must be a string (null clears it)"),
    };
    if rights.is_none() && note.is_none() {
        return err("give a ceiling, allow, deny, a note, or reset true");
    }
    if let Err(e) = ctx.app.agents.set_rights(id, rights, note) {
        return refused(e);
    }
    let policy = Policy::of(&ctx.app);
    match ctx.app.agents.get(id, &policy) {
        Some(v) => ok(
            ctx,
            format!(
                "Rights of agent {} saved: ceiling {}, {} tool(s) allowed{}",
                v["name"].as_str().unwrap_or(id),
                v["rights"]["ceiling"].as_str().unwrap_or("full"),
                v["tools_allowed"].as_u64().unwrap_or(0),
                if reset { " (own rights cleared)" } else { "" }
            ),
            v,
        ),
        None => refused(AgentError::NotFound),
    }
}

/// `revoke_agent` (admin, confirmed): refuse the agent's calls, end its
/// sessions, revoke its tokens. An agent cannot revoke itself.
pub async fn revoke_agent(
    ctx: &CallCtx,
    access: &Access,
    own_id: &str,
    args: &Value,
    params: &Value,
) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if id == own_id {
        return err("an agent cannot revoke itself: ask the operator to do it in the console or with `rustybin agents revoke`");
    }
    let policy = Policy::of(&ctx.app);
    let Some(current) = ctx.app.agents.get(id, &policy) else {
        return refused(AgentError::NotFound);
    };
    let action = format!(
        "revoke agent {} ({}): its calls are refused, its sessions end and its tokens are revoked",
        current["name"].as_str().unwrap_or(id),
        current["credential"]["label"]
            .as_str()
            .or(current["credential"]["kind"].as_str())
            .unwrap_or("?")
    );
    if let Confirm::Answer(v) = confirm(ctx, "revoke_agent", args, params, &action).await {
        return v;
    }
    match ctx
        .app
        .agents
        .revoke(id, &access.principal(), &ctx.app.config.console_tokens)
    {
        Ok(v) => ok(
            ctx,
            format!(
                "Revoked agent {}: {} session(s) ended, {} token(s) revoked",
                current["name"].as_str().unwrap_or(id),
                v["sessions_ended"].as_u64().unwrap_or(0),
                v["tokens_revoked"].as_u64().unwrap_or(0)
            ),
            v,
        ),
        Err(e) => refused(e),
    }
}

/// `restore_agent` (admin): undo a revocation.
pub fn restore_agent(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    match ctx.app.agents.restore(id, &ctx.app.config.console_tokens) {
        Ok(v) => ok(ctx, format!("Restored agent {id}"), v),
        Err(e) => refused(e),
    }
}

/// `end_agent_sessions` (admin): close the agent's handshake-era sessions
/// without revoking it.
pub fn end_agent_sessions(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    match ctx.app.agents.end_sessions(id) {
        Ok(n) => ok(
            ctx,
            format!("Ended {n} session(s) of agent {id}"),
            json!({ "id": id, "sessions_ended": n }),
        ),
        Err(e) => refused(e),
    }
}
