//! Event mock tools (`crate::events`): the same functions as the
//! `/_rustybin/mocks/{name}/channels*` and `/subscribers*` handlers. What a
//! client published on a channel is third-party content: under
//! `untrusted_*`, cut, never in the text summary. A webhook subscriber
//! sends messages out, so adding one is confirmed.

use serde_json::{json, Value};

use super::access::Access;
use super::tools::{arg_str, arg_u64, confirm, err, need_str, ok, service_error, Confirm};
use crate::events;
use crate::mcp::core::CallCtx;

/// Bytes of one message's payload a tool answers.
const MAX_PAYLOAD: usize = 2048;

/// `list_channels`.
pub fn list_channels(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match events::channels(&access.svc(&ctx.app), name) {
        Ok(v) => {
            let lines: Vec<String> = v["channels"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    format!(
                        "{} ({}): sends {}, receives {}",
                        c["id"].as_str().unwrap_or("?"),
                        c["pattern"].as_str().unwrap_or("?"),
                        c["send"].as_array().map_or(0, Vec::len),
                        c["receive"].as_array().map_or(0, Vec::len),
                    )
                })
                .collect();
            let summary = format!(
                "Mock {name} has {} channel(s), each at /events/{name}/<id> (WebSocket, SSE or POST):\n{}",
                lines.len(),
                lines.join("\n")
            );
            ok(ctx, summary, v)
        }
        Err(e) => service_error(&e),
    }
}

/// A message for a tool answer: what Rustybin published as it is (it came
/// from the document or the mock's settings), what a client published
/// under `untrusted_*`.
fn message_row(m: &Value) -> Value {
    let received = m["direction"] == "received";
    let mut row = json!({
        "seq": m["seq"],
        "timestamp": m["timestamp"],
        "direction": m["direction"],
        "message": m["message"],
        "valid": m["valid"],
        "size": m["size"],
    });
    for key in ["transport", "source", "fault", "delivered"] {
        if !m[key].is_null() {
            row[key] = m[key].clone();
        }
    }
    let body = if m["payload"].is_null() {
        m["text"].as_str().map(str::to_string)
    } else {
        Some(m["payload"].to_string())
    };
    if let Some(mut text) = body {
        let cut = text.len() > MAX_PAYLOAD;
        if cut {
            let mut end = MAX_PAYLOAD;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        row["untrusted_payload"] = json!(text);
        if cut {
            row["payload_truncated"] = json!(true);
        }
    }
    if received && !m["issues"].is_null() {
        row["untrusted_issues"] = m["issues"].clone();
    }
    row
}

/// `list_channel_messages`.
pub fn list_channel_messages(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, channel) = match (need_str(args, "name"), need_str(args, "channel")) {
        (Ok(n), Ok(c)) => (n, c),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let limit = arg_u64(args, "limit").map(|l| (l as usize).clamp(1, 100));
    let after = arg_u64(args, "after");
    match events::messages(
        &access.svc(&ctx.app),
        name,
        channel,
        limit.or(Some(20)),
        after,
    ) {
        Ok(v) => {
            let rows: Vec<Value> = v["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .map(message_row)
                .collect();
            let count = |d: &str| rows.iter().filter(|r| r["direction"] == d).count();
            let invalid = rows.iter().filter(|r| r["valid"] == false).count();
            let summary = format!(
                "Channel {channel} of mock {name}: showing {} message(s), {} sent and {} received, {} not valid. Payloads are under untrusted_payload.",
                rows.len(),
                count("sent"),
                count("received"),
                invalid
            );
            ok(
                ctx,
                summary,
                json!({
                    "mock": name,
                    "channel": channel,
                    "total": v["total"],
                    "last": v["last"],
                    "count": rows.len(),
                    "messages": rows,
                }),
            )
        }
        Err(e) => service_error(&e),
    }
}

/// `publish_event`.
pub fn publish_event(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, channel) = match (need_str(args, "name"), need_str(args, "channel")) {
        (Ok(n), Ok(c)) => (n, c),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let body = events::PublishBody {
        payload: args.get("payload").filter(|p| !p.is_null()).cloned(),
        count: arg_u64(args, "count").map(|c| c.min(u64::from(u32::MAX)) as u32),
    };
    match events::publish(&access.svc(&ctx.app), name, channel, body) {
        Ok(v) => {
            let rows: Vec<Value> = v["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .map(message_row)
                .collect();
            let first = rows.first().cloned().unwrap_or(Value::Null);
            let summary = format!(
                "Published {} message(s) on channel {channel} of mock {name}: to {} WebSocket, {} SSE and {} webhook subscriber(s).",
                rows.len(),
                first["delivered"]["ws"],
                first["delivered"]["sse"],
                first["delivered"]["webhook"],
            );
            ok(
                ctx,
                summary,
                json!({ "mock": name, "channel": channel, "published": rows.len(), "messages": rows }),
            )
        }
        Err(e) => service_error(&e),
    }
}

/// `list_event_subscribers`.
pub fn list_event_subscribers(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let name = match need_str(args, "name") {
        Ok(n) => n,
        Err(e) => return e,
    };
    match events::subscribers(&access.svc(&ctx.app), name) {
        Ok(v) => {
            let summary = format!(
                "Mock {name} has {} webhook subscriber(s).",
                v["count"].as_u64().unwrap_or(0)
            );
            ok(ctx, summary, v)
        }
        Err(e) => service_error(&e),
    }
}

/// `add_event_subscriber` (confirmed: messages go out to the URL).
pub async fn add_event_subscriber(
    ctx: &CallCtx,
    access: &Access,
    args: &Value,
    params: &Value,
) -> Value {
    let (name, channel, url) = match (
        need_str(args, "name"),
        need_str(args, "channel"),
        need_str(args, "url"),
    ) {
        (Ok(n), Ok(c), Ok(u)) => (n, c, u),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    let host = url.split('?').next().unwrap_or(url);
    let action = format!("post every message of channel {channel} of mock {name} to {host}");
    if let Confirm::Answer(v) = confirm(ctx, "add_event_subscriber", args, params, &action).await {
        return v;
    }
    let body = events::SubscribeBody {
        channel: channel.to_string(),
        url: url.to_string(),
        ttl_seconds: arg_u64(args, "ttl_seconds"),
    };
    match events::subscribe(&access.svc(&ctx.app), name, body) {
        Ok(v) => {
            let summary = format!(
                "Subscriber {} added: channel {channel} of mock {name} posts its messages to {} until {}.",
                v["id"].as_str().unwrap_or("?"),
                v["url"].as_str().unwrap_or("?"),
                v["expires_at"].as_str().unwrap_or("?"),
            );
            ok(ctx, summary, v)
        }
        Err(e) => service_error(&e),
    }
}

/// `remove_event_subscriber`.
pub fn remove_event_subscriber(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let (name, id) = match (need_str(args, "name"), arg_str(args, "id")) {
        (Ok(n), Some(i)) => (n, i),
        (Err(e), _) => return e,
        (_, None) => return err("missing required argument: id (list_event_subscribers)"),
    };
    match events::unsubscribe(&access.svc(&ctx.app), name, id) {
        Ok(v) => ok(ctx, format!("Subscriber {id} of mock {name} removed."), v),
        Err(e) => service_error(&e),
    }
}
