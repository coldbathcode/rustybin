//! Settings and egress tools: the instance settings (never secrets) and the
//! outbound policy with a dry check of a URL against it.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_str, confirm, err, ok, Confirm};
use crate::egress::{forbidden_address, parse_url, Purpose};
use crate::mcp::core::CallCtx;
use crate::settings::UpdateError;

/// A settings refusal as a tool error.
pub fn update_error(e: UpdateError) -> Value {
    match e {
        UpdateError::Locked(keys) => {
            let mut v = err(format!(
                "these settings are locked by RUSTYBIN_SETTINGS_LOCK (the environment is authoritative): {}",
                keys.join(", ")
            ));
            v["structuredContent"] = json!({ "locked": keys });
            v
        }
        UpdateError::Invalid(problems) => {
            let mut v = err(format!("invalid settings: {}", problems.join("; ")));
            v["structuredContent"] = json!({ "problems": problems });
            v
        }
        UpdateError::Io(e) => err(format!("settings not saved: {e}")),
    }
}

/// `get_settings` (admin): every setting, or one group's, or one key.
pub fn get_settings(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let mut view = ctx.app.settings.view();
    let group = arg_str(args, "group")
        .map(str::trim)
        .filter(|g| !g.is_empty());
    let key = arg_str(args, "key")
        .map(str::trim)
        .filter(|k| !k.is_empty());
    if let Some(k) = key {
        if ctx.app.settings.spec(k).is_none() {
            return err(format!("no setting named {k:?} (get_settings lists them)"));
        }
    }
    if let Some(list) = view.get_mut("settings").and_then(Value::as_array_mut) {
        list.retain(|s| {
            group.is_none_or(|g| s["group"].as_str() == Some(g))
                && key.is_none_or(|k| s["key"].as_str() == Some(k))
        });
    }
    let shown = view["settings"].as_array().map(Vec::len).unwrap_or(0);
    let summary = match (group, key) {
        (_, Some(k)) => {
            let s = &view["settings"][0];
            format!(
                "{k} = {} (source {}{})",
                s["value"],
                s["source"].as_str().unwrap_or("?"),
                if s["locked"].as_bool() == Some(true) {
                    ", locked"
                } else {
                    ""
                }
            )
        }
        (Some(g), None) => format!(
            "{shown} setting(s) in group {g}{}",
            if view["locked"].as_bool() == Some(true) {
                "; the environment is authoritative (RUSTYBIN_SETTINGS_LOCK)"
            } else {
                ""
            }
        ),
        (None, None) => format!(
            "{shown} settings in {} groups{}{}; secrets show only {{set, hint}}",
            view["groups"].as_array().map(Vec::len).unwrap_or(0),
            if view["persisted"].as_bool() == Some(true) {
                ""
            } else {
                " (not persisted: no RUSTYBIN_STATE_DIR, changes last until a restart)"
            },
            if view["locked"].as_bool() == Some(true) {
                "; the environment is authoritative (RUSTYBIN_SETTINGS_LOCK)"
            } else {
                ""
            }
        ),
    };
    ok(ctx, summary, view)
}

/// Setting groups whose change widens what the instance reaches or who may
/// sign in: the outbound policy, the agents' ceilings, the console sign-in
/// git, single sign-on and the trusted proxy header (plus the AI provider
/// and base URL). A change there is confirmed by the user, after the
/// permission check.
pub const SENSITIVE_PREFIXES: &[&str] =
    &["egress.", "agents.", "console.", "git.", "sso.", "proxy."];

fn sensitive(key: &str) -> bool {
    SENSITIVE_PREFIXES.iter().any(|p| key.starts_with(p))
        || crate::settings::SECURITY_KEYS.contains(&key)
}

/// `update_settings` (admin): set values and/or forget stored ones. Secrets
/// are refused: keys and passwords are set in the console, the CLI or the
/// environment, never through an agent. A change of `egress.*`, `agents.*`,
/// `console.*` or `git.*` asks the user to confirm.
pub async fn update_settings(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let settings = &ctx.app.settings;
    let values: BTreeMap<String, Value> = match args.get("values") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(_) => return err("values must be an object of setting key to value"),
    };
    let unset: Vec<String> = match args.get("unset") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        Some(_) => return err("unset must be a list of setting keys"),
    };
    if values.is_empty() && unset.is_empty() {
        return err("give values to set and/or keys to unset");
    }
    for key in values.keys().chain(unset.iter()) {
        match settings.spec(key) {
            None => {
                return err(format!(
                    "no setting named {key:?} (get_settings lists them)"
                ))
            }
            Some(spec) if spec.secret => {
                return err(format!(
                    "{key} is a secret: an agent never sets secrets. The operator sets it in the \
                     console (Management), with `rustybin settings set-secret {key}` or through {}",
                    spec.env.unwrap_or("its environment variable")
                ))
            }
            Some(_) => {}
        }
    }
    // The permissions the keys and the values need, exactly as PATCH
    // /_rustybin/settings checks them (a confirmation is no permission).
    let keys = values
        .keys()
        .chain(unset.iter())
        .map(|k| (k.as_str(), false));
    if let Some(p) = crate::settings::keys_need(&access.resolved, settings, keys)
        .or_else(|| crate::settings::values_need(&access.resolved, &ctx.app.users, &values))
    {
        return err(format!(
            "permission_required: {}. Changing these settings needs it, as PATCH /_rustybin/settings does (a role that grants it, or the admin token).",
            p.name()
        ));
    }
    let mut touched: Vec<&str> = values
        .keys()
        .chain(unset.iter())
        .map(String::as_str)
        .filter(|k| sensitive(k))
        .collect();
    touched.sort_unstable();
    touched.dedup();
    if !touched.is_empty() {
        let action = format!(
            "change {} (the outbound policy, the agents' rights, the console sign-in or git)",
            touched.join(", ")
        );
        if let Confirm::Answer(v) = confirm(ctx, "update_settings", args, params, &action).await {
            return v;
        }
    }
    let who = access.principal();
    let mut changed = Vec::new();
    if !values.is_empty() {
        match settings.update(values, BTreeMap::new(), &who) {
            Ok(c) => changed.extend(c),
            Err(e) => return update_error(e),
        }
    }
    for key in &unset {
        match settings.unset(key, &who) {
            Ok(true) => changed.push(key.clone()),
            Ok(false) => {}
            Err(e) => return update_error(e),
        }
    }
    ctx.app.assist.sync_egress();
    let after: Vec<Value> = changed
        .iter()
        .map(|k| {
            let (value, source) = settings.get(k);
            json!({ "key": k, "value": value, "source": source.as_str() })
        })
        .collect();
    ok(
        ctx,
        if changed.is_empty() {
            "Nothing changed (the values were already in place)".to_string()
        } else {
            format!("Changed {}", changed.join(", "))
        },
        json!({ "changed": changed, "settings": after, "persisted": settings.persisted() }),
    )
}

fn purpose_of(name: &str) -> Option<Purpose> {
    Purpose::ALL.iter().copied().find(|p| p.as_str() == name)
}

/// `check_egress` (admin): the outbound policy, and what it would say to a
/// URL for a purpose (the allowlists, the loopback switch and the IP
/// literal checks; the resolved addresses are checked when a call is made).
pub fn check_egress(ctx: &CallCtx, _access: &Access, args: &Value) -> Value {
    let egress = &ctx.app.egress;
    let mut view = egress.view();
    let url = arg_str(args, "url")
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let Some(url) = url else {
        let (allow, _) = egress.allowlist();
        return ok(
            ctx,
            format!(
                "Outbound policy: {} allowlist entr{}, loopback {}; purposes: {}",
                allow.len(),
                if allow.len() == 1 { "y" } else { "ies" },
                if egress.loopback_allowed() {
                    "allowed"
                } else {
                    "refused"
                },
                Purpose::ALL
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            view,
        );
    };
    let purpose = match arg_str(args, "purpose") {
        None => Purpose::McpRelay,
        Some(p) => match purpose_of(p) {
            Some(p) => p,
            None => {
                return err(format!(
                    "unknown purpose {p:?}: one of {}",
                    Purpose::ALL
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        },
    };
    let mut reasons: Vec<String> = Vec::new();
    let target = match parse_url(url, true) {
        Ok(t) => t,
        Err(e) => {
            return ok(
                ctx,
                format!("{url} would be refused for {}: {e}", purpose.as_str()),
                json!({ "url": url, "purpose": purpose.as_str(), "allowed": false, "reasons": [e] }),
            )
        }
    };
    let rule = egress.rule(purpose);
    let (global, _) = egress.allowlist();
    let merged = global.merged(&rule.allow);
    let unrestricted = rule.any && !ctx.app.config.public_mode;
    let listed = merged.allows_host(&target.host, target.port)
        || target
            .ip
            .is_some_and(|ip| merged.allows_ip(ip, target.port));
    if !(unrestricted || listed) {
        reasons.push(format!(
            "{}:{} is not in the allowlist for purpose {} (RUSTYBIN_EGRESS_ALLOW, the egress.allow setting, or {})",
            target.host,
            target.port,
            purpose.as_str(),
            purpose.configured_by()
        ));
    }
    if let Some(why) = target.ip.and_then(forbidden_address) {
        reasons.push(format!("{} is always refused: {why}", target.host));
    }
    let loopback = target.ip.is_some_and(|ip| ip.is_loopback()) || target.host == "localhost";
    if loopback && !(egress.loopback_allowed() || rule.loopback) {
        reasons.push(
            "loopback addresses need RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true (the egress.allow_loopback setting)"
                .into(),
        );
    }
    let allowed = reasons.is_empty();
    if let Value::Object(m) = &mut view {
        m.insert(
            "check".into(),
            json!({
                "url": url,
                "origin": target.origin(),
                "purpose": purpose.as_str(),
                "allowed": allowed,
                "reasons": reasons,
                "rule": { "any": rule.any, "loopback": rule.loopback, "allow": rule.allow.describe() },
                "note": "A dry check of the policy: the addresses the host resolves to are checked again when a call is made (forbidden ranges, loopback).",
            }),
        );
    }
    ok(
        ctx,
        if allowed {
            format!(
                "{} would be allowed for {} (before DNS)",
                target.origin(),
                purpose.as_str()
            )
        } else {
            format!(
                "{} would be refused for {}: {}",
                target.origin(),
                purpose.as_str(),
                view["check"]["reasons"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; "))
                    .unwrap_or_default()
            )
        },
        view,
    )
}

/// `test_sso`: single sign-on's status, and the provider's discovery
/// document and keys fetched now (as `POST /_rustybin/sso/test`).
pub async fn test_sso(ctx: &CallCtx, access: &Access, _args: &Value) -> Value {
    let app = &ctx.app;
    if !crate::login::enabled(&app.config) {
        return err("single sign-on works with the password sign-in only (RUSTYBIN_CONTROL_AUTH=password, outside shared mode)");
    }
    let callback = format!(
        "{}{}",
        access.links.base_url().trim_end_matches('/'),
        crate::sso::api::CALLBACK_PATH
    );
    let v = app.sso.test(&app.users, &callback).await;
    if v["ok"] == json!(true) {
        let keys = v["keys"].as_array().map(Vec::len).unwrap_or(0);
        ok(
            ctx,
            format!(
                "Single sign-on is ready: {} answers its discovery document and {keys} key(s). Register {callback} as the client's redirect URI.",
                v["issuer"].as_str().unwrap_or("the provider")
            ),
            v,
        )
    } else {
        let mut out = err(format!(
            "single sign-on is not working: {}",
            v["error"].as_str().unwrap_or("not set up")
        ));
        out["structuredContent"] = v;
        out
    }
}
