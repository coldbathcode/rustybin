//! The agent tools: who talks to this server, their rights, revocation and
//! sessions. Everyone's agents with `agents.read` / `agents.manage`; a user
//! without them sees, limits and revokes their own (`super::may_manage`).

use serde_json::{json, Value};

use super::{Action, AgentError, Ceiling, Policy, Rights};
use crate::mcp::core::CallCtx;
use crate::mcp::manage::access::Access;
use crate::mcp::manage::tools::{arg_bool, arg_str, confirm, err, need_str, ok, Confirm};

fn refused(e: AgentError) -> Value {
    match e {
        AgentError::NotFound => err("no such agent (list_agents shows the ids)"),
        AgentError::Invalid(m) => err(m),
        AgentError::Io(m) => err(format!("the agents state could not be saved: {m}")),
        AgentError::Needs(p, m) => err(format!("permission_required: {}. {m}", p.name())),
    }
}

/// Refuse an operator action on the caller's own agent, or on any agent of
/// the caller's credential (rights and revocations follow the credential's
/// fingerprint, so a sibling client would lift the caller's own limits).
fn own_agent(ctx: &CallCtx, access: &Access, own_id: &str, id: &str, what: &str) -> Option<Value> {
    let same_credential = ctx
        .app
        .agents
        .credential_of(id)
        .is_some_and(|c| c.fingerprint() == access.credential.fingerprint());
    (id == own_id || same_credential).then(|| {
        err(format!(
            "an agent cannot {what} an agent of its own credential: ask the operator to do it \
             in the console (Management, AI agents) or with `rustybin agents`"
        ))
    })
}

/// `list_agents`: every agent (`agents.read`) or the caller's own, the
/// policy and the ceilings; `owner` keeps one user's.
pub fn list_agents(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let state = &ctx.app;
    let own = match super::visible_to(&access.resolved) {
        Ok(o) => o,
        Err(e) => return refused(e),
    };
    let asked = arg_str(args, "owner")
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .map(|o| super::owner_filter(&state.users, o));
    let owner = match (own, asked) {
        (Some(me), Some(o)) if o != me => {
            return refused(AgentError::Needs(
                crate::rbac::Permission::AgentsRead,
                "other people's agents need the agents.read permission".into(),
            ))
        }
        (Some(me), _) => Some(me),
        (None, asked) => asked,
    };
    let policy = Policy::of(state);
    let agents = state.agents.list(&policy, owner.as_deref());
    let revoked = agents
        .iter()
        .filter(|a| a["status"].as_str() == Some("revoked"))
        .count();
    let summary = format!(
        "{} agent(s){}{}; default ceiling {}, {} rule(s) from rustybin.yaml{}",
        agents.len(),
        if owner.is_some() { " of one user" } else { "" },
        if revoked > 0 {
            format!(" ({revoked} revoked)")
        } else {
            String::new()
        },
        policy.default_ceiling.as_str(),
        policy.rules.len(),
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
            "owner": owner,
            "you": access.resolved.user_id(),
            "policy": policy.view(),
            "capacity": super::MAX_AGENTS,
            "persisted": state.agents.persisted(),
            "control_auth": state.config.control_auth.as_str(),
            "ceilings": super::CEILINGS,
            "roles": super::ceiling_roles(state, &access.resolved),
        }),
    )
}

/// `get_agent`: one agent with its calls per tool, last errors, sessions,
/// rights and recent activity.
pub fn get_agent(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if let Err(e) = super::may_read(&ctx.app.agents, &access.resolved, id) {
        return refused(e);
    }
    let policy = Policy::of(&ctx.app);
    let Some(mut v) = ctx.app.agents.get(id, &policy) else {
        return refused(AgentError::NotFound);
    };
    if let Some(activity) = ctx.app.agents.activity(id, &policy) {
        v["activity"] = activity;
    }
    let summary = format!(
        "Agent {} ({}, for {}): status {}, {} call(s) in total, ceiling {}{}",
        v["name"].as_str().unwrap_or("unknown"),
        v["credential"]["label"]
            .as_str()
            .or(v["credential"]["kind"].as_str())
            .unwrap_or("?"),
        v["owner_label"].as_str().unwrap_or("?"),
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

/// `set_agent_rights`: a ceiling (a 0.3 ceiling or a role), allow and deny
/// lists and a note; `reset` goes back to the rules and the default.
/// Without `agents.manage`, only the caller's own agents, and only to
/// limit them.
pub fn set_agent_rights(ctx: &CallCtx, access: &Access, own_id: &str, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if let Some(refusal) = own_agent(ctx, access, own_id, id, "change the rights of") {
        return refusal;
    }
    let reset = arg_bool(args, "reset").unwrap_or(false);
    let ceiling = match arg_str(args, "ceiling") {
        None => None,
        Some(c) => match Ceiling::parse_any(c) {
            Some(c) => Some(c),
            None => {
                return err(format!(
                    "ceiling must be one of {} or a role name (list_roles)",
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
    if let Err(e) = super::check_rights(&rights, &ctx.app.users) {
        return refused(e);
    }
    let policy = Policy::of(&ctx.app);
    if let Err(e) = super::may_manage(
        &ctx.app.agents,
        &policy,
        &access.resolved,
        id,
        &Action::Rights(rights.clone()),
    ) {
        return refused(e);
    }
    if let Err(e) = ctx.app.agents.set_rights(id, rights, note) {
        return refused(e);
    }
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

/// `revoke_agent` (confirmed): refuse the agent's calls, end its sessions,
/// revoke its tokens. An agent cannot revoke itself.
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
    if let Err(e) = super::may_manage(
        &ctx.app.agents,
        &policy,
        &access.resolved,
        id,
        &Action::Revoke,
    ) {
        return refused(e);
    }
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
    match ctx.app.agents.revoke_as(
        id,
        &access.principal(),
        access.resolved.user_id(),
        &ctx.app.config.console_tokens,
    ) {
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

/// `restore_agent`: undo a revocation (a user without `agents.manage`
/// restores only an agent of theirs they revoked).
pub fn restore_agent(ctx: &CallCtx, access: &Access, own_id: &str, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if let Some(refusal) = own_agent(ctx, access, own_id, id, "restore") {
        return refusal;
    }
    if let Err(e) = super::may_manage(
        &ctx.app.agents,
        &Policy::of(&ctx.app),
        &access.resolved,
        id,
        &Action::Restore,
    ) {
        return refused(e);
    }
    match ctx.app.agents.restore(id, &ctx.app.config.console_tokens) {
        Ok(v) => ok(ctx, format!("Restored agent {id}"), v),
        Err(e) => refused(e),
    }
}

/// `end_agent_sessions`: close the agent's handshake-era sessions without
/// revoking it.
pub fn end_agent_sessions(ctx: &CallCtx, access: &Access, own_id: &str, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    if let Some(refusal) = own_agent(ctx, access, own_id, id, "end the sessions of") {
        return refusal;
    }
    if let Err(e) = super::may_manage(
        &ctx.app.agents,
        &Policy::of(&ctx.app),
        &access.resolved,
        id,
        &Action::EndSessions,
    ) {
        return refused(e);
    }
    match ctx.app.agents.end_sessions(id) {
        Ok(n) => ok(
            ctx,
            format!("Ended {n} session(s) of agent {id}"),
            json!({ "id": id, "sessions_ended": n }),
        ),
        Err(e) => refused(e),
    }
}
