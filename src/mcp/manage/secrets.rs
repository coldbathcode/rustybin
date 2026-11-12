//! Secret requests: an agent asks for a secret setting to be set and reads
//! how its request stands. A secret is never a tool input or output: the
//! value is typed by a person in the console or given to the CLI on stdin
//! (`crate::secret_requests`), and these tools carry the setting's key, a
//! reason and a request's state only.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_str, err, need_str, ok};
use crate::mcp::core::CallCtx;
use crate::mcp::tools as mock_tools;
use crate::secret_requests::{self as requests, Asker, Request, Status};

/// The console's Management view: where a person fulfils or declines.
fn console_url(access: &Access) -> String {
    format!(
        "{}/ui/#/manage/general",
        access.links.base_url().trim_end_matches('/')
    )
}

/// What the agent is told to do next, by state.
fn next(r: &Request) -> &'static str {
    match r.status {
        Status::Pending => "A person with the permission to set this secret types it in the console (Management) or gives it to `rustybin settings fulfil` on stdin, or declines. Read the state again with get_secret_request. The value never passes through this conversation: do not ask for it here.",
        Status::Fulfilled => "The secret was set by a person. Carry on with what needed it.",
        Status::Declined => "A person declined. Do not ask again unless the user tells you to.",
        Status::Expired => "Nobody answered in time. Ask the user before asking again.",
    }
}

/// Does the client take URL elicitations inside a call's own answer (a
/// 2026-07-28 client that declares `elicitation.url`)?
fn takes_url_elicitation(ctx: &CallCtx) -> bool {
    ctx.version.is_modern()
        && ctx
            .client_caps
            .get("elicitation")
            .and_then(|e| e.get("url"))
            .is_some_and(Value::is_object)
}

/// The URL elicitation that tells the person at the agent where to go.
/// Never a form: nothing typed at the agent could be the value.
fn elicitation(access: &Access, r: &Request) -> Value {
    json!({
        "mode": "url",
        "elicitationId": r.id,
        "url": console_url(access),
        "message": format!(
            "An agent asked for the secret setting {} to be set. Open Rustybin's Management view and sign in to type it there, or decline. Never paste the secret into this conversation.",
            r.key
        ),
    })
}

/// `request_secret`: ask for a secret setting to be set by a person.
pub async fn request_secret(
    ctx: &CallCtx,
    access: &Access,
    agent: &str,
    args: &Value,
    params: &Value,
) -> Value {
    let app = &ctx.app;
    let key = match need_str(args, "setting") {
        Ok(k) => k.trim(),
        Err(e) => return e,
    };
    crate::audit::target("setting", key);
    let reason = match need_str(args, "reason") {
        Ok(r) => requests::clean_reason(r),
        Err(e) => return e,
    };
    if reason.is_empty() {
        return err(
            "reason: say in a sentence what needs the secret (the person who sets it reads it)",
        );
    }
    if !requests::enabled(&app.settings) {
        return err(format!(
            "secret requests are off on this instance (the {} setting): an agent never sets a secret, and nobody is asked. The operator sets it in the console (Management), with `rustybin settings set-secret {key}`, or through the environment",
            requests::SWITCH_KEY
        ));
    }
    if let Err(why) = requests::askable(&app.settings, key) {
        return err(why);
    }
    // What changing a setting of this group needs, as update_settings
    // checks it: everything a person setting it needs but secrets.write.
    if let Some(p) = crate::settings::extra_permissions(key, false)
        .into_iter()
        .find(|p| !access.resolved.has(*p))
    {
        return err(format!(
            "permission_required: {}. Asking for {key} needs it, as changing a setting of its group does (a role that grants it, or the admin token).",
            p.name()
        ));
    }
    let policy = super::agents::Policy::of(app);
    let name = app
        .agents
        .get(agent, &policy)
        .and_then(|a| a["name"].as_str().map(str::to_string))
        .unwrap_or_default();
    let asker = Asker {
        agent: agent.to_string(),
        name,
        credential: access.credential.clone(),
    };
    let (r, new) = match app.secret_requests.ask(asker, key, &reason) {
        Ok(x) => x,
        Err(e) => return err(e.message()),
    };
    crate::audit::event("secret_request");
    crate::audit::target("secret-request", &r.id);
    // Where the client can show the person a link, tell them where to go.
    // A 2026-07-28 client answers the elicitation by calling again: that
    // call finds the request it made and gets the answer below. (An older
    // session is not asked: the call would wait on the client's answer.)
    let answered = matches!(
        mock_tools::input_response(ctx, "request_secret", params, "open"),
        Ok(Some(_))
    );
    if new && takes_url_elicitation(ctx) {
        let req = json!({
            "method": "elicitation/create",
            "params": elicitation(access, &r),
        });
        return mock_tools::input_required(ctx, "request_secret", "open", req);
    }
    let view = app.secret_requests.agent_view(&r, &app.settings);
    ok(
        ctx,
        format!(
            "{} for {key} to be set: request {} is {}{}. {}",
            if new || answered {
                "Asked"
            } else {
                "Already asked"
            },
            r.id,
            r.status.as_str(),
            if r.status == Status::Pending {
                format!(", waiting until {}", r.expires.to_rfc3339())
            } else {
                String::new()
            },
            next(&r)
        ),
        json!({
            "request": view,
            "asked": new || answered,
            "console": console_url(access),
            "cli": format!("rustybin settings fulfil {}", r.id),
        }),
    )
}

/// `get_secret_request`: the state of one of the calling agent's requests,
/// or all of them.
pub fn get_secret_request(ctx: &CallCtx, _access: &Access, agent: &str, args: &Value) -> Value {
    let app = &ctx.app;
    let store = &app.secret_requests;
    let id = arg_str(args, "id").map(str::trim).filter(|i| !i.is_empty());
    match id {
        Some(id) => {
            // Another agent's request does not exist for this one.
            let Some(r) = store.get(id).filter(|r| r.asker.agent == agent) else {
                return err(format!(
                    "no secret request {id:?} of this agent (a request is gone an hour after it was set, declined or expired; get_secret_request without an id lists yours)"
                ));
            };
            // A note is the word of the person who declined (cleaned and
            // capped): theirs to the agent, so it is in the text too.
            let note = r
                .note
                .as_deref()
                .map(|n| format!(" The person who declined left a note: {n:?}."))
                .unwrap_or_default();
            ok(
                ctx,
                format!(
                    "Request {} for {} is {}. {}{note}",
                    r.id,
                    r.key,
                    r.status.as_str(),
                    next(&r)
                ),
                json!({ "request": store.agent_view(&r, &app.settings) }),
            )
        }
        None => {
            let mine = store.of_agent(agent);
            let pending = mine.iter().filter(|r| r.status == Status::Pending).count();
            let views: Vec<Value> = mine
                .iter()
                .map(|r| store.agent_view(r, &app.settings))
                .collect();
            ok(
                ctx,
                format!(
                    "{} secret request(s) of this agent, {pending} pending{}",
                    mine.len(),
                    if requests::enabled(&app.settings) {
                        ""
                    } else {
                        "; secret requests are off on this instance"
                    }
                ),
                json!({ "requests": views, "enabled": requests::enabled(&app.settings) }),
            )
        }
    }
}
