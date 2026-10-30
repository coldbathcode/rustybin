//! Dev portal tools: the items and their state, publishing and unpublishing,
//! the `portal.*` settings, the insights.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_bool, arg_str, confirm, err, need_str, ok, service_error, Confirm};
use crate::mcp::core::CallCtx;
use crate::portal::{self, api as portal_api, AuthMode, Meta};

/// `list_portal_items`: the portal's settings, every item the caller may
/// publish (mocks, MCP servers, the LLM, agents, the tools, the registry)
/// with its published metadata, whether it is hidden and why, and its
/// insights.
pub fn list_portal_items(ctx: &CallCtx, access: &Access, _args: &Value) -> Value {
    let view = portal_api::control_json(&ctx.app, &access.caller);
    let items = view["items"].as_array().map(Vec::len).unwrap_or(0);
    let summary = format!(
        "Portal {} (auth {}, {}): {} item(s) can be published, {} published, {} shown at {}",
        if view["enabled"].as_bool() == Some(true) {
            "on"
        } else {
            "off"
        },
        view["auth"].as_str().unwrap_or("?"),
        view["mode"].as_str().unwrap_or("?"),
        items,
        view["counts"]["published"],
        view["counts"]["shown"],
        access.links.base_url().to_string() + "/portal/"
    );
    ok(ctx, summary, view)
}

fn meta_from(args: &Value) -> Result<Meta, Value> {
    let tags = match args.get("tags") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>(),
        ),
        Some(Value::String(s)) => Some(
            s.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        Some(_) => return Err(err("tags must be a list of strings")),
    };
    Ok(Meta {
        title: arg_str(args, "title").map(str::to_string),
        summary: arg_str(args, "summary").map(str::to_string),
        tags,
        contact: arg_str(args, "contact").map(str::to_string),
        order: args.get("order").and_then(Value::as_i64),
        credentials: arg_bool(args, "credentials"),
    })
}

/// `publish_portal_item` (manage): publish an item with its metadata.
/// Confirmed when the portal is on and open to anyone (the item becomes
/// public at once).
pub async fn publish_portal_item(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    let meta = match meta_from(args) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let state = &ctx.app;
    let open = portal::active(state) && portal::auth_mode(&state.settings) == AuthMode::Open;
    if open {
        let action = format!(
            "publish {id} to the dev portal at {}/portal/, which is open to anyone who can reach this instance",
            access.links.base_url()
        );
        if let Confirm::Answer(v) = confirm(ctx, "publish_portal_item", args, params, &action).await
        {
            return v;
        }
    }
    let svc = access.svc(state);
    match portal_api::publish_item(state, &svc, &access.principal(), id, meta) {
        Ok((view, created)) => {
            let hidden = view["hidden"].as_str().map(str::to_string);
            let summary = format!(
                "{} {id} on the portal ({}/portal/){}{}",
                if created { "Published" } else { "Updated" },
                access.links.base_url(),
                if portal::active(state) {
                    ""
                } else {
                    "; the portal is off: set portal.enabled (set_portal_branding) to serve it"
                },
                hidden
                    .map(|h| format!("; not shown yet: {h}"))
                    .unwrap_or_default()
            );
            ok(ctx, summary, view)
        }
        Err(e) => service_error(&e),
    }
}

/// `unpublish_portal_item` (manage).
pub fn unpublish_portal_item(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let id = match need_str(args, "id") {
        Ok(i) => i,
        Err(e) => return e,
    };
    let svc = access.svc(&ctx.app);
    match portal_api::unpublish_item(&ctx.app, &svc, &access.principal(), id) {
        Ok(v) => ok(ctx, format!("Removed {id} from the portal"), v),
        Err(e) => service_error(&e),
    }
}

/// `set_portal_branding` (admin): the `portal.*` settings (title, intro,
/// contact, the switch, who may read it, the base URL).
pub fn set_portal_branding(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let settings = &ctx.app.settings;
    let given = match args.get("settings") {
        Some(Value::Object(m)) => m.clone(),
        _ => return err("settings must be an object: the portal.* keys (with or without the portal. prefix) and their values; get_settings with group portal lists them"),
    };
    let mut values: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in given {
        let key = if k.starts_with("portal.") {
            k
        } else {
            format!("portal.{k}")
        };
        match settings.spec(&key) {
            Some(spec) if spec.group == "portal" && !spec.secret => {}
            _ => {
                return err(format!(
                    "{key} is not a portal setting; the keys are {}",
                    settings
                        .specs()
                        .iter()
                        .filter(|s| s.group == "portal" && !s.secret)
                        .map(|s| s.key)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
        values.insert(key, v);
    }
    if values.is_empty() {
        return err("settings is empty");
    }
    match settings.update(values, BTreeMap::new(), &access.principal()) {
        Ok(changed) => ok(
            ctx,
            format!(
                "Portal settings changed: {}; the portal is {}",
                changed.join(", "),
                if portal::active(&ctx.app) {
                    "on"
                } else {
                    "off"
                }
            ),
            json!({
                "changed": changed,
                "enabled": portal::active(&ctx.app),
                "auth": portal::auth_mode(&ctx.app.settings).as_str(),
                "branding": portal::branding(&ctx.app.settings),
            }),
        ),
        Err(e) => super::settings::update_error(e),
    }
}

/// `get_portal_insights`: views, downloads and Try it calls per published item.
pub fn get_portal_insights(ctx: &CallCtx, access: &Access, _args: &Value) -> Value {
    let v = portal_api::insights_json(&ctx.app, &access.caller);
    let n = v["items"].as_array().map(Vec::len).unwrap_or(0);
    ok(
        ctx,
        format!(
            "Insights of {n} published item(s) over the last {} days (counts only)",
            v["days"]
        ),
        v,
    )
}
