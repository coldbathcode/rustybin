//! Resources: static text/JSON/blob resources, a ticking clock and URI templates.

use base64::Engine;
use serde_json::{json, Value};

use super::core::{cacheable, CallCtx};
use super::data;
use super::protocol::RpcError;
use super::Profile;

pub const README_URI: &str = "rustybin://docs/readme";
pub const CUSTOMERS_URI: &str = "rustybin://data/customers.json";
pub const LOGO_URI: &str = "rustybin://images/logo.png";
pub const CLOCK_URI: &str = "rustybin://clock";
pub const CUSTOMER_TEMPLATE: &str = "rustybin://customers/{id}";
pub const WEATHER_TEMPLATE: &str = "rustybin://weather/{city}";

struct StaticResource {
    uri: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    mime: &'static str,
    servers: &'static [&'static str],
}

static RESOURCES: &[StaticResource] = &[
    StaticResource {
        uri: README_URI,
        name: "readme",
        title: "Rustybin MCP README",
        description: "What this demo server offers (markdown).",
        mime: "text/markdown",
        servers: &["devtools"],
    },
    StaticResource {
        uri: CUSTOMERS_URI,
        name: "customers",
        title: "CRM customers",
        description: "All fake CRM customers (same people as the GraphQL users).",
        mime: "application/json",
        servers: &["crm"],
    },
    StaticResource {
        uri: LOGO_URI,
        name: "logo",
        title: "Rustybin logo",
        description: "A tiny PNG (binary blob resource).",
        mime: "image/png",
        servers: &["devtools"],
    },
    StaticResource {
        uri: CLOCK_URI,
        name: "clock",
        title: "Server clock",
        description: "Current server time. Subscribe to receive notifications/resources/updated on every tick.",
        mime: "text/plain",
        servers: &["weather", "devtools"],
    },
];

struct Template {
    uri_template: &'static str,
    prefix: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    servers: &'static [&'static str],
}

static TEMPLATES: &[Template] = &[
    Template {
        uri_template: CUSTOMER_TEMPLATE,
        prefix: "rustybin://customers/",
        name: "customer",
        title: "CRM customer",
        description: "One customer with their orders; id is u1..u5.",
        servers: &["crm"],
    },
    Template {
        uri_template: WEATHER_TEMPLATE,
        prefix: "rustybin://weather/",
        name: "weather",
        title: "City weather",
        description: "Deterministic fake weather report for a city (URL-encode spaces).",
        servers: &["weather"],
    },
];

fn visible(profile: &Profile, servers: &[&str]) -> bool {
    profile.server.is_none_or(|s| servers.contains(&s))
}

fn resource_json(r: &StaticResource) -> Value {
    let mut v = json!({
        "uri": r.uri,
        "name": r.name,
        "title": r.title,
        "description": r.description,
        "mimeType": r.mime,
    });
    if r.uri == LOGO_URI {
        v["size"] = json!(data::logo_png().len());
    }
    v
}

pub fn list(profile: &Profile) -> Vec<Value> {
    RESOURCES
        .iter()
        .filter(|r| visible(profile, r.servers))
        .map(resource_json)
        .collect()
}

pub fn templates(profile: &Profile) -> Vec<Value> {
    TEMPLATES
        .iter()
        .filter(|t| visible(profile, t.servers))
        .map(|t| {
            json!({
                "uriTemplate": t.uri_template,
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "mimeType": "application/json",
            })
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hex = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Template match: (template, decoded variable).
fn match_template(profile: &Profile, uri: &str) -> Option<(&'static Template, String)> {
    TEMPLATES
        .iter()
        .filter(|t| visible(profile, t.servers))
        .find_map(|t| {
            let rest = uri.strip_prefix(t.prefix)?;
            if rest.is_empty() || rest.contains('/') || rest.len() > 100 {
                return None;
            }
            Some((t, percent_decode(rest)?))
        })
}

/// Metadata for a readable URI (used for resource links).
pub fn describe(profile: &Profile, uri: &str) -> Option<Value> {
    if let Some(r) = RESOURCES
        .iter()
        .find(|r| r.uri == uri && visible(profile, r.servers))
    {
        return Some(resource_json(r));
    }
    let (t, var) = match_template(profile, uri)?;
    if t.uri_template == CUSTOMER_TEMPLATE && data::find_customer(&var).is_none_or(|c| c.id != var)
    {
        return None;
    }
    Some(json!({
        "uri": uri,
        "name": format!("{}-{}", t.name, var),
        "title": format!("{} {}", t.title, var),
        "mimeType": "application/json",
    }))
}

pub fn exists(profile: &Profile, uri: &str) -> bool {
    describe(profile, uri).is_some()
}

/// Contents of a URI regardless of profile (`None` when unknown).
pub fn read_contents(uri: &str) -> Option<Vec<Value>> {
    let text = |mime: &str, text: String| json!({ "uri": uri, "mimeType": mime, "text": text });
    Some(match uri {
        README_URI => vec![text("text/markdown", data::README.to_string())],
        CUSTOMERS_URI => vec![text(
            "application/json",
            serde_json::to_string_pretty(&data::customers_json()).unwrap_or_default(),
        )],
        LOGO_URI => vec![json!({
            "uri": uri,
            "mimeType": "image/png",
            "blob": base64::engine::general_purpose::STANDARD.encode(data::logo_png()),
        })],
        CLOCK_URI => vec![text("text/plain", chrono::Utc::now().to_rfc3339())],
        _ => {
            if let Some(id) = uri.strip_prefix("rustybin://customers/") {
                let c = data::CUSTOMERS.iter().find(|c| c.id == id)?;
                let orders: Vec<Value> = data::ORDERS
                    .iter()
                    .filter(|o| o.customer_id == c.id)
                    .map(data::order_json)
                    .collect();
                let mut body = data::customer_json(c);
                body["orders"] = json!(orders);
                vec![text(
                    "application/json",
                    serde_json::to_string_pretty(&body).unwrap_or_default(),
                )]
            } else {
                let city = uri.strip_prefix("rustybin://weather/")?;
                let city = percent_decode(city).filter(|c| !c.trim().is_empty())?;
                vec![text(
                    "application/json",
                    serde_json::to_string_pretty(&data::weather_for(&city, "metric"))
                        .unwrap_or_default(),
                )]
            }
        }
    })
}

/// `resources/read`.
pub fn read(ctx: &CallCtx, params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Missing required parameter: uri"))?;
    if !exists(&ctx.profile, uri) {
        return Err(RpcError::resource_not_found(uri, ctx.version));
    }
    let contents =
        read_contents(uri).ok_or_else(|| RpcError::resource_not_found(uri, ctx.version))?;
    let ttl = if uri == CLOCK_URI { 0 } else { 60_000 };
    let customer_data = uri == CUSTOMERS_URI || uri.starts_with("rustybin://customers/");
    let scope = if customer_data && ctx.profile.access != super::Access::Open {
        "private"
    } else {
        "public"
    };
    Ok(cacheable(ctx, json!({ "contents": contents }), ttl, scope))
}

/// Completion values for a template variable.
pub fn completions(profile: &Profile, template: &str, arg: &str) -> Option<Vec<&'static str>> {
    let t = TEMPLATES
        .iter()
        .find(|t| t.uri_template == template && visible(profile, t.servers))?;
    Some(match (t.uri_template, arg) {
        (CUSTOMER_TEMPLATE, "id") => data::CUSTOMERS.iter().map(|c| c.id).collect(),
        (WEATHER_TEMPLATE, "city") => data::CITIES.to_vec(),
        _ => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matching() {
        let p = Profile::open();
        assert!(exists(&p, "rustybin://customers/u1"));
        assert!(!exists(&p, "rustybin://customers/u9"));
        assert!(exists(&p, "rustybin://weather/New%20York"));
        assert!(!exists(&p, "rustybin://weather/"));
        assert!(!exists(&p, "rustybin://nope"));
        let c = read_contents("rustybin://weather/New%20York").expect("weather");
        assert!(c[0]["text"].as_str().unwrap_or("").contains("New York"));
    }

    #[test]
    fn named_servers_filter_resources() {
        let weather = Profile::named("weather").expect("weather");
        let uris: Vec<Value> = list(&weather)
            .into_iter()
            .map(|r| r["uri"].clone())
            .collect();
        assert_eq!(uris, vec![json!(CLOCK_URI)]);
        assert!(!exists(&weather, CUSTOMERS_URI));
    }
}
