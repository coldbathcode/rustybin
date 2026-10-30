//! `GET /_rustybin/registry/browse`: one page of a real registry (or this
//! instance's mock one), fetched through the egress policy under
//! `Purpose::Registry` (`registry.modelcontextprotocol.io` and the saved
//! registries' hosts by default, others through `RUSTYBIN_EGRESS_ALLOW`),
//! cached for a minute, answered as entries the inspector can connect to.
//! Everything that comes back is the registry's data, shown as text.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use super::entries::{OFFICIAL_META, PUBLISHER_META};
use super::{validate, Registry};
use crate::egress::{url_entry, Allowlist, Purpose, Rule};
use crate::mcp::inspector::client;
use crate::state::AppState;

/// The registry browsed by default.
pub const DEFAULT_REGISTRY: &str = "https://registry.modelcontextprotocol.io";
const DEFAULT_HOST: &str = "registry.modelcontextprotocol.io";
/// Pages kept, and for how long.
pub const CACHE_MAX: usize = 64;
pub const CACHE_TTL: Duration = Duration::from_secs(60);
/// Largest page read.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

/// The page cache (bounded, a minute per page).
#[derive(Default)]
pub struct Cache {
    pages: Mutex<VecDeque<(String, Instant, Value)>>,
}

impl Cache {
    fn get(&self, key: &str) -> Option<Value> {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.retain(|(_, at, _)| at.elapsed() < CACHE_TTL);
        pages
            .iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, _, v)| v.clone())
    }

    fn put(&self, key: String, value: Value) {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.retain(|(k, at, _)| k != &key && at.elapsed() < CACHE_TTL);
        while pages.len() >= CACHE_MAX {
            pages.pop_front();
        }
        pages.push_back((key, Instant::now(), value));
    }

    pub fn len(&self) -> usize {
        self.pages.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The API base of a registry URL: `https://r.example` and
/// `https://r.example/v0.1` both give `https://r.example/v0.1`; a path on
/// this instance (`/mcp-registry`) stays a path.
pub fn api_base(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err(
            "url is required: a registry base URL, or /mcp-registry for this instance".into(),
        );
    }
    if url.contains(['?', '#']) || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("a registry base URL has no query, fragment or spaces".into());
    }
    if url.starts_with('/') {
        if !(url == "/mcp-registry" || url.starts_with("/mcp-registry/")) {
            return Err("a path on this instance must be under /mcp-registry".into());
        }
    } else {
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("not a URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("the URL must be http(s) with a host".into());
        }
    }
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with("/v0.1") || trimmed.ends_with("/v0") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}/v0.1"))
    }
}

/// The `Purpose::Registry` rule: the public registry plus the saved ones.
pub fn rule(saved: &[String]) -> Rule {
    let mut items = vec![DEFAULT_HOST.to_string()];
    items.extend(saved.iter().filter_map(|u| url_entry(u)));
    let (allow, _) = Allowlist::parse(&items);
    Rule {
        allow,
        ..Rule::default()
    }
}

/// How a remote expects credentials, from the headers it declares.
pub fn auth_hint(headers: &Value) -> Value {
    for h in headers.as_array().into_iter().flatten() {
        let name = h["name"].as_str().unwrap_or("");
        if name.eq_ignore_ascii_case("authorization") {
            let basic = h["value"]
                .as_str()
                .is_some_and(|v| v.trim_start().to_ascii_lowercase().starts_with("basic"));
            return json!({ "kind": if basic { "basic" } else { "bearer" }, "required": h["isRequired"] });
        }
        if !name.is_empty() && h["isSecret"] == true {
            return json!({ "kind": "api_key", "name": name, "required": h["isRequired"] });
        }
    }
    json!({ "kind": "none" })
}

/// One `ServerResponse` as the console shows it.
pub fn summarize(item: &Value) -> Value {
    let s = &item["server"];
    let off = &item["_meta"][OFFICIAL_META];
    let remotes: Vec<Value> = s["remotes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            json!({
                "type": r["type"],
                "url": r["url"],
                "headers": r["headers"].as_array().into_iter().flatten().filter_map(|h| h["name"].as_str()).collect::<Vec<_>>(),
                "auth": auth_hint(&r["headers"]),
            })
        })
        .collect();
    let packages: Vec<Value> = s["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "registryType": p["registryType"],
                "identifier": p["identifier"],
                "version": p["version"],
                "transport": p["transport"]["type"],
                "runtimeHint": p["runtimeHint"],
            })
        })
        .collect();
    let check = validate::validate(s);
    json!({
        "name": s["name"],
        "title": s["title"],
        "description": s["description"],
        "version": s["version"],
        "status": off["status"],
        "isLatest": off["isLatest"],
        "publishedAt": off["publishedAt"],
        "updatedAt": off["updatedAt"],
        "websiteUrl": s["websiteUrl"],
        "repository": s["repository"],
        "remotes": remotes,
        "packages": packages,
        "rustybin": s["_meta"][PUBLISHER_META]["rustybin"],
        "valid": check["valid"],
        "issues": check["issues"].as_array().map(Vec::len).unwrap_or(0),
        "server": s,
    })
}

enum Failure {
    Refused(String),
    Failed(String),
}

async fn fetch(state: &AppState, url: &str) -> Result<(u16, Vec<u8>), Failure> {
    let headers = [("accept".to_string(), "application/json".to_string())];
    if url.starts_with('/') {
        let resp = client::send_local(state, url, "GET", &headers, None, TIMEOUT)
            .await
            .map_err(Failure::Failed)?;
        let status = resp.status;
        let body = client::read_all(resp.body, MAX_BODY)
            .await
            .map_err(Failure::Failed)?;
        return Ok((status, body));
    }
    state.egress.permit(
        Purpose::Registry,
        rule(&state.settings.list("registry.saved")),
    );
    let pinned = state
        .egress
        .resolve(url, Purpose::Registry)
        .await
        .map_err(|e| Failure::Refused(e.0))?;
    let http = state
        .egress
        .client(&pinned, TIMEOUT)
        .map_err(Failure::Failed)?;
    let resp = http
        .get(&pinned.url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            Failure::Failed(if e.is_timeout() {
                "the registry did not answer in time".to_string()
            } else if e.is_connect() {
                "could not connect to the registry".to_string()
            } else {
                "the request failed".to_string()
            })
        })?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| Failure::Failed("the answer broke off".into()))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(Failure::Failed(format!(
                "the page is larger than {} MiB",
                MAX_BODY / 1024 / 1024
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

#[derive(Deserialize, Default)]
pub struct BrowseQuery {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub version: Option<String>,
}

fn query_string(q: &BrowseQuery) -> String {
    let mut ser = form_urlencoded::Serializer::new(String::new());
    ser.append_pair("limit", &q.limit.unwrap_or(30).clamp(1, 100).to_string());
    for (k, v) in [
        ("search", &q.search),
        ("cursor", &q.cursor),
        ("version", &q.version),
    ] {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            ser.append_pair(k, v);
        }
    }
    ser.finish()
}

/// Browse a registry: the page, or `(status, {error, reason})`. Shared by
/// `GET /_rustybin/registry/browse` and the management tool `browse_registry`.
pub async fn browse_json(
    state: &AppState,
    reg: &Registry,
    q: &BrowseQuery,
) -> Result<Value, (StatusCode, Value)> {
    let fail = |status: StatusCode, message: String, reason: &str| {
        (status, json!({ "error": message, "reason": reason }))
    };
    let raw = q
        .url
        .clone()
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    let api = api_base(&raw).map_err(|e| fail(StatusCode::BAD_REQUEST, e, "invalid_url"))?;
    let list_url = format!("{api}/servers?{}", query_string(q));
    if let Some(mut hit) = reg.browse.get(&list_url) {
        hit["cached"] = json!(true);
        return Ok(hit);
    }
    let (status, body) = match fetch(state, &list_url).await {
        Ok(r) => r,
        Err(Failure::Refused(e)) => {
            return Err(fail(
                StatusCode::FORBIDDEN,
                format!("the egress policy refused the registry: {e}"),
                "egress_refused",
            ))
        }
        Err(Failure::Failed(e)) => return Err(fail(StatusCode::BAD_GATEWAY, e, "unreachable")),
    };
    let parsed: Option<Value> = serde_json::from_slice(&body).ok();
    if !(200..300).contains(&status) {
        let detail = parsed
            .as_ref()
            .and_then(|v| v["detail"].as_str().or(v["error"].as_str()))
            .map(|d| d.chars().take(500).collect::<String>());
        return Err((
            StatusCode::BAD_GATEWAY,
            json!({
                "error": format!("the registry answered {status}{}", detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default()),
                "reason": "upstream_status",
                "status": status,
                "request": list_url,
            }),
        ));
    }
    let Some(doc) = parsed.filter(|d| d["servers"].is_array() || d["servers"].is_null()) else {
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "the answer is not a registry server list ({servers, metadata})".into(),
            "not_a_registry",
        ));
    };
    let servers: Vec<Value> = doc["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(summarize)
        .collect();
    let out = json!({
        "registry": api,
        "request": list_url,
        "status": status,
        "cached": false,
        "fetched_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "count": servers.len(),
        "next_cursor": doc["metadata"]["nextCursor"],
        "servers": servers,
    });
    reg.browse.put(list_url, out.clone());
    tracing::info!(target: "rustybin::registry", registry = %api.split('?').next().unwrap_or(""), count = servers_len(&out), "registry browsed");
    Ok(out)
}

pub async fn browse(
    State(state): State<AppState>,
    Extension(reg): Extension<Arc<Registry>>,
    Query(q): Query<BrowseQuery>,
) -> Response {
    match browse_json(&state, &reg, &q).await {
        Ok(v) => Json(v).into_response(),
        Err((status, body)) => (status, Json(body)).into_response(),
    }
}

fn servers_len(v: &Value) -> usize {
    v["servers"].as_array().map(Vec::len).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_bases() {
        assert_eq!(
            api_base("https://r.example").unwrap(),
            "https://r.example/v0.1"
        );
        assert_eq!(
            api_base("https://r.example/").unwrap(),
            "https://r.example/v0.1"
        );
        assert_eq!(
            api_base("https://r.example/v0").unwrap(),
            "https://r.example/v0"
        );
        assert_eq!(
            api_base("https://r.example/api/v0.1/").unwrap(),
            "https://r.example/api/v0.1"
        );
        assert_eq!(api_base("/mcp-registry").unwrap(), "/mcp-registry/v0.1");
        assert!(api_base("/mcp").is_err());
        assert!(api_base("ftp://r.example").is_err());
        assert!(api_base("https://r.example?x=1").is_err());
        assert!(api_base("").is_err());
    }

    #[test]
    fn auth_hints() {
        assert_eq!(
            auth_hint(&json!([{ "name": "Authorization", "value": "Bearer {t}" }]))["kind"],
            "bearer"
        );
        assert_eq!(
            auth_hint(&json!([{ "name": "Authorization", "value": "Basic {c}" }]))["kind"],
            "basic"
        );
        let k = auth_hint(&json!([{ "name": "X-Api-Key", "isSecret": true }]));
        assert_eq!(
            (k["kind"].as_str(), k["name"].as_str()),
            (Some("api_key"), Some("X-Api-Key"))
        );
        assert_eq!(auth_hint(&json!([{ "name": "X-Trace" }]))["kind"], "none");
        assert_eq!(auth_hint(&Value::Null)["kind"], "none");
    }

    #[test]
    fn the_rule_allows_saved_hosts() {
        let r = rule(&[
            "https://reg.example.net:8443/x".into(),
            "/mcp-registry".into(),
        ]);
        assert!(r.allow.allows_host("registry.modelcontextprotocol.io", 443));
        assert!(r.allow.allows_host("reg.example.net", 8443));
        assert!(!r.allow.allows_host("reg.example.net", 443));
    }

    #[test]
    fn the_cache_is_bounded() {
        let c = Cache::default();
        for i in 0..(CACHE_MAX + 10) {
            c.put(format!("k{i}"), json!(i));
        }
        assert_eq!(c.len(), CACHE_MAX);
        assert!(c.get("k0").is_none());
        assert_eq!(
            c.get(&format!("k{}", CACHE_MAX + 9)),
            Some(json!(CACHE_MAX + 9))
        );
    }
}
