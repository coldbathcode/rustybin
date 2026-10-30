//! What can be published, what the portal shows, and the catalogue
//! (`GET /portal/api/catalog`): every published item whose target exists,
//! with what the portal needs to render its page. Snippets use the portal's
//! base URL (`portal.base_url`, else the URL the portal was opened with);
//! demo credentials appear only when the item says so, and never a secret of
//! the instance.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::{ItemId, Published, KINDS, MCP_SERVERS, NEW_DAYS};
use crate::catalog::category;
use crate::mcp::Profile;
use crate::mocks::checks;
use crate::mocks::service::Links;
use crate::mocks::store::Snapshot;
use crate::mocks::visits::Owner;
use crate::state::AppState;

/// The tools item: these catalogue categories (echo, status, auth,
/// reliability and the protocol playgrounds).
pub const TOOL_CATEGORIES: &[&str] = &[
    category::ECHO,
    category::STATUS,
    category::SHAPING,
    category::STREAMING,
    category::REDIRECTS,
    category::INFO,
    category::AUTH_BASIC,
    category::AUTH_HMAC,
    category::AUTH_JWT,
    category::AUTH_OIDC,
    category::GRAPHQL,
    category::SOAP,
    category::WEBSOCKET,
    category::RELIABILITY,
];

/// The LLM item's providers: (id, title, base path, routes, sample method,
/// sample path, extra headers, sample body).
type Provider = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

pub const PROVIDERS: &[Provider] = &[
    (
        "openai",
        "OpenAI",
        "/ai/openai/v1",
        &[
            "chat/completions",
            "completions",
            "embeddings",
            "models",
            "moderations",
            "images/generations",
            "audio/transcriptions",
            "responses",
        ],
        "/ai/openai/v1/chat/completions",
        &[],
        r#"{"model": "gpt-4o", "messages": [{"role": "user", "content": "Hello"}]}"#,
    ),
    (
        "anthropic",
        "Anthropic",
        "/ai/anthropic",
        &["v1/messages", "v1/messages/count_tokens", "v1/models"],
        "/ai/anthropic/v1/messages",
        &[("anthropic-version", "2023-06-01")],
        r#"{"model": "claude-sonnet-4-5", "max_tokens": 256, "messages": [{"role": "user", "content": "Hello"}]}"#,
    ),
    (
        "gemini",
        "Gemini",
        "/ai/gemini",
        &[
            "v1beta/models",
            "v1beta/models/{model}:generateContent",
            "v1beta/models/{model}:streamGenerateContent",
            "v1beta/models/{model}:countTokens",
            "v1beta/models/{model}:embedContent",
        ],
        "/ai/gemini/v1beta/models/gemini-2.5-flash:generateContent",
        &[],
        r#"{"contents": [{"parts": [{"text": "Hello"}]}]}"#,
    ),
    (
        "azure",
        "Azure OpenAI",
        "/ai/azure",
        &[
            "openai/deployments/{deployment}/chat/completions",
            "openai/deployments/{deployment}/completions",
            "openai/deployments/{deployment}/embeddings",
        ],
        "/ai/azure/openai/deployments/gpt-4o/chat/completions?api-version=2024-10-21",
        &[],
        r#"{"messages": [{"role": "user", "content": "Hello"}]}"#,
    ),
    (
        "bedrock",
        "AWS Bedrock Runtime",
        "/ai/bedrock",
        &[
            "model/{modelId}/converse",
            "model/{modelId}/converse-stream",
            "model/{modelId}/invoke",
            "model/{modelId}/invoke-with-response-stream",
        ],
        "/ai/bedrock/model/anthropic.claude-3-haiku-20240307-v1:0/converse",
        &[],
        r#"{"messages": [{"role": "user", "content": [{"text": "Hello"}]}]}"#,
    ),
    (
        "ollama",
        "Ollama",
        "/ai/ollama",
        &["api/chat", "api/generate", "api/tags", "api/embed"],
        "/ai/ollama/api/chat",
        &[],
        r#"{"model": "llama3.2", "stream": false, "messages": [{"role": "user", "content": "Hello"}]}"#,
    ),
    (
        "cohere",
        "Cohere",
        "/ai/cohere",
        &["v2/rerank", "v2/embed"],
        "/ai/cohere/v2/rerank",
        &[],
        r#"{"model": "rerank-v3.5", "query": "gateways", "documents": ["an API gateway", "a garden gate"]}"#,
    ),
];

/// Headers that shape the mock LLM's answers: (header, effect).
pub const LLM_HEADERS: &[(&str, &str)] = &[
    (
        "X-Rustybin-Latency-Ms",
        "Wait N ms before the response headers.",
    ),
    (
        "X-Rustybin-TTFT-Ms",
        "Time to first token: streams wait N ms before the first token.",
    ),
    (
        "X-Rustybin-Tokens-Per-Second",
        "Streaming pace (default 100; 0 streams as fast as possible).",
    ),
    (
        "X-Rustybin-Fail",
        "Answer with this status instead (a provider-shaped error).",
    ),
];

// ── Candidates (the console's list) ─────────────────────────────────

/// One publishable thing, as the console lists it.
pub struct Candidate {
    pub id: String,
    pub kind: &'static str,
    /// The item's own title and summary (the published ones override them).
    pub title: String,
    pub summary: String,
    /// `live`, `off` or `draft` for a mock, else `serving`.
    pub state: String,
    pub owner: Option<Owner>,
}

fn first_paragraph(text: Option<&str>) -> String {
    let text = text.unwrap_or("").trim();
    let para = text.split("\n\n").next().unwrap_or("").trim();
    let mut out: String = para.chars().take(400).collect();
    if para.chars().count() > 400 {
        out.push_str("...");
    }
    out
}

fn mock_candidate(s: &Snapshot) -> Candidate {
    let c = s.live.as_ref().unwrap_or(&s.draft);
    Candidate {
        id: ItemId::Mock(s.name.clone()).as_string(),
        kind: "api",
        title: c.spec.title.clone(),
        summary: first_paragraph(c.spec.description.as_deref()),
        state: s.state().to_string(),
        owner: Some(s.owner.clone()),
    }
}

fn static_candidate(id: &ItemId) -> Candidate {
    let (title, summary) = match id {
        ItemId::Mcp(key) => MCP_SERVERS
            .iter()
            .find(|(k, ..)| k == key)
            .map(|(_, _, t, s)| (t.to_string(), s.to_string()))
            .unwrap_or_default(),
        ItemId::Llm => (
            "Mock LLM".into(),
            "OpenAI, Anthropic, Gemini, Azure OpenAI, Bedrock, Ollama and Cohere compatible endpoints with deterministic answers, streaming, pacing and faults.".into(),
        ),
        ItemId::Agent(a) => crate::a2a::agents::find(a)
            .map(|a| (a.name.to_string(), first_paragraph(Some(a.description))))
            .unwrap_or_default(),
        ItemId::Tools => (
            "HTTP tools".into(),
            "Echo, status codes, response shaping, streaming, auth and reliability endpoints for testing clients and gateways.".into(),
        ),
        ItemId::Registry => (
            "MCP registry".into(),
            "A mock MCP registry in the registry API's /v0.1 shape that lists the MCP servers of this instance.".into(),
        ),
        ItemId::Mock(_) => (String::new(), String::new()),
    };
    Candidate {
        id: id.as_string(),
        kind: id.kind(),
        title,
        summary,
        state: "serving".into(),
        owner: None,
    }
}

/// Every publishable item the caller may see, in display order.
pub fn candidates(state: &AppState, can_read: impl Fn(&Snapshot) -> bool) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = state
        .mocks
        .list()
        .iter()
        .filter(|s| can_read(s))
        .map(mock_candidate)
        .collect();
    for (key, ..) in MCP_SERVERS {
        out.push(static_candidate(&ItemId::Mcp(key)));
    }
    out.push(static_candidate(&ItemId::Llm));
    for a in crate::a2a::agents::AGENTS {
        out.push(static_candidate(&ItemId::Agent(a.id)));
    }
    out.push(static_candidate(&ItemId::Tools));
    out.push(static_candidate(&ItemId::Registry));
    out
}

/// The candidate for one id (`None` for a mock that does not exist).
pub fn candidate(state: &AppState, id: &ItemId) -> Option<Candidate> {
    match id {
        ItemId::Mock(name) => state.mocks.get(name).as_ref().map(mock_candidate),
        other => Some(static_candidate(other)),
    }
}

// ── Published entries ───────────────────────────────────────────────

/// A published item: from the API or from `rustybin.yaml`.
pub struct Entry {
    pub id: ItemId,
    pub key: String,
    pub published: Published,
    /// `api` or `file`.
    pub source: &'static str,
}

/// Every published item (file items first; an id the file lists ignores
/// an API publish of the same id).
pub fn entries(state: &AppState) -> Vec<Entry> {
    let (file, loaded_at) = state.mocks.file_portal();
    let mut out: Vec<Entry> = Vec::new();
    for item in file {
        let Ok(id) = ItemId::parse(&item.id) else {
            continue;
        };
        let meta = item.meta();
        out.push(Entry {
            key: id.as_string(),
            id,
            published: Published {
                title: meta.title,
                summary: meta.summary,
                tags: meta.tags.unwrap_or_default(),
                contact: meta.contact,
                order: meta.order.unwrap_or(0),
                credentials: meta.credentials.unwrap_or(true),
                published_at: loaded_at,
                updated_at: loaded_at,
                by: "file".into(),
                owner: None,
            },
            source: "file",
        });
    }
    for (key, published) in state.portal.published() {
        if out.iter().any(|e| e.key == key) {
            continue;
        }
        if let Ok(id) = ItemId::parse(&key) {
            out.push(Entry {
                id,
                key,
                published,
                source: "api",
            });
        }
    }
    out
}

/// Why a published item is not on the portal (`None`: it is).
pub fn hidden_reason(state: &AppState, e: &Entry) -> Option<&'static str> {
    let ItemId::Mock(name) = &e.id else {
        return None;
    };
    let Some(s) = state.mocks.get(name) else {
        return Some("the mock does not exist");
    };
    if e.published.owner.as_ref().unwrap_or(&Owner::Instance) != &s.owner {
        return Some("another owner's mock has this name");
    }
    if s.live.is_none() {
        return Some("the mock is not deployed");
    }
    None
}

/// The published items the portal shows.
pub fn public_entries(state: &AppState) -> Vec<Entry> {
    entries(state)
        .into_iter()
        .filter(|e| hidden_reason(state, e).is_none())
        .collect()
}

// ── The catalogue ───────────────────────────────────────────────────

/// Where the portal's snippets point.
pub struct Ctx {
    /// The portal's base URL: `portal.base_url`, else the request's origin.
    pub base: String,
    /// The base is a configured gateway URL.
    pub gateway: bool,
    pub links: Links,
}

impl Ctx {
    /// A mock's root URL: the visitor host of a visitor mock when there is
    /// one, else `{base}/mock/{name}`.
    pub fn mock_base(&self, owner: &Owner, name: &str) -> String {
        if owner.is_visitor() {
            let host = self.links.mock_base(owner, name);
            if !host.ends_with(&format!("/mock/{name}")) {
                return host;
            }
        }
        format!("{}/mock/{name}", self.base)
    }
}

fn kind_rank(kind: &str) -> usize {
    KINDS
        .iter()
        .position(|(k, _)| *k == kind)
        .unwrap_or(KINDS.len())
}

/// A fingerprint of everything the catalogue depends on (the cache key).
pub fn fingerprint(state: &AppState, ctx: &Ctx) -> String {
    let mut fp = format!(
        "{}|{}|{}|{}|{}|{}",
        state.portal.generation(),
        ctx.base,
        state.mocks.file_portal().1.timestamp_micros(),
        super::branding(&state.settings),
        super::auth_mode(&state.settings).as_str(),
        state
            .settings
            .string("registry.scenario")
            .unwrap_or_default(),
    );
    for e in entries(state) {
        if let ItemId::Mock(name) = &e.id {
            if let Some(s) = state.mocks.get(name) {
                fp.push_str(&format!(
                    "|{name}:{}:{}:{}",
                    s.live.as_ref().map(|l| l.revision).unwrap_or(0),
                    s.enabled,
                    s.updated_at.timestamp_micros()
                ));
            }
        }
    }
    fp
}

/// The whole catalogue.
pub fn catalog_json(state: &AppState, ctx: &Ctx) -> Value {
    let now = Utc::now();
    let mut list: Vec<(usize, i64, String, Value, DateTime<Utc>)> = Vec::new();
    for e in public_entries(state) {
        let Some(item) = item_json(state, &e, ctx, now) else {
            continue;
        };
        let title = item["title"].as_str().unwrap_or("").to_lowercase();
        let changed = item["changed_at"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(e.published.updated_at);
        list.push((
            kind_rank(e.id.kind()),
            e.published.order,
            title,
            item,
            changed,
        ));
    }
    list.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let mut whats_new: Vec<(DateTime<Utc>, String)> = list
        .iter()
        .filter(|l| l.3["new"] == true)
        .map(|l| (l.4, l.3["id"].as_str().unwrap_or("").to_string()))
        .collect();
    whats_new.sort_by_key(|a| std::cmp::Reverse(a.0));
    let kinds: Vec<Value> = KINDS
        .iter()
        .map(|(k, t)| {
            json!({ "id": k, "title": t, "count": list.iter().filter(|l| l.3["kind"] == *k).count() })
        })
        .filter(|k| k["count"] != 0)
        .collect();
    let mut portal = super::branding(&state.settings);
    portal["auth"] = json!(super::auth_mode(&state.settings).as_str());
    portal["base_url"] = json!(ctx.base);
    portal["gateway"] = json!(ctx.gateway);
    portal["version"] = json!(env!("CARGO_PKG_VERSION"));
    json!({
        "portal": portal,
        "kinds": kinds,
        "items": list.into_iter().map(|l| l.3).collect::<Vec<_>>(),
        "whats_new": whats_new.into_iter().take(6).map(|(_, id)| id).collect::<Vec<_>>(),
        "generated_at": now.to_rfc3339(),
    })
}

/// One item of the catalogue (`None` when its target is gone).
fn item_json(state: &AppState, e: &Entry, ctx: &Ctx, now: DateTime<Utc>) -> Option<Value> {
    let cand = candidate(state, &e.id)?;
    let p = &e.published;
    let (detail, changed) = match &e.id {
        ItemId::Mock(name) => {
            let s = state.mocks.get(name)?;
            let changed = s.updated_at.max(p.updated_at);
            (mock_detail(state, &s, p, ctx)?, changed)
        }
        ItemId::Mcp(key) => (mcp_detail(key, ctx), p.updated_at),
        ItemId::Llm => (llm_detail(ctx), p.updated_at),
        ItemId::Agent(a) => (agent_detail(a, ctx)?, p.updated_at),
        ItemId::Tools => (tools_detail(), p.updated_at),
        ItemId::Registry => (registry_detail(state, ctx), p.updated_at),
    };
    let new = (now - changed).num_days() < NEW_DAYS;
    Some(json!({
        "id": e.key,
        "kind": e.id.kind(),
        "title": p.title.clone().unwrap_or(cand.title),
        "summary": p.summary.clone().unwrap_or(cand.summary),
        "tags": p.tags,
        "contact": p.contact,
        "visitor": p.visitor(),
        "published_at": p.published_at.to_rfc3339(),
        "changed_at": changed.to_rfc3339(),
        "new": new,
        "detail": detail,
    }))
}

fn curl(method: &str, url: &str, headers: &[(String, String)], body: Option<&str>) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let mut out = String::from("curl");
    if method != "GET" {
        out.push_str(&format!(" -X {method}"));
    }
    out.push_str(&format!(" {}", q(url)));
    for (k, v) in headers {
        out.push_str(&format!(" \\\n  -H {}", q(&format!("{k}: {v}"))));
    }
    if let Some(b) = body {
        out.push_str(&format!(" \\\n  -d {}", q(b)));
    }
    out
}

fn pairs(v: &[(String, String)]) -> Vec<Value> {
    v.iter()
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect()
}

fn mock_detail(state: &AppState, s: &Snapshot, p: &Published, ctx: &Ctx) -> Option<Value> {
    let c = s.live.as_ref()?;
    let base = ctx.mock_base(&s.owner, &s.name);
    let reveal = p.credentials;
    let coverage = checks::coverage(c);
    let operations: Vec<Value> = c
        .spec
        .operations
        .iter()
        .map(|op| {
            let mut sample = checks::sample_request(c, op);
            if !reveal {
                checks::hide_credentials(c, op, &mut sample);
            }
            let mut headers = sample.headers.clone();
            if let Some(ct) = &sample.content_type {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("Content-Type".into(), ct.clone()));
                }
            }
            let body = sample.body.as_ref().map(|b| match b {
                Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            });
            let query = if sample.query.is_empty() {
                String::new()
            } else {
                format!(
                    "?{}",
                    form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(sample.query.iter())
                        .finish()
                )
            };
            let url = format!("{base}{}{query}", sample.path);
            json!({
                "id": op.id,
                "method": op.method,
                "path": op.path,
                "summary": op.summary,
                "description": op.description,
                "tags": op.tags,
                "deprecated": op.deprecated,
                "secured": !op.security.is_empty() && c.settings.security.enabled,
                "has_example": checks::missing_examples(c, op).is_empty(),
                "sample": {
                    "method": sample.method,
                    "path": format!("{}{query}", sample.path),
                    "headers": pairs(&headers),
                    "body": body,
                },
                "curl": curl(&sample.method, &url, &headers, body.as_deref()),
            })
        })
        .collect();
    let mcp = c.settings.mcp.enabled.then(|| {
        let url = format!("{base}/_rustybin/mcp");
        let tools = c.mcp_tools();
        let server = format!("{}-mock", s.name);
        json!({
            "url": url,
            "tools": tools.tools.iter().map(|t| json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "method": t.method,
                "path": t.path,
                "read_only": t.read_only,
            })).collect::<Vec<_>>(),
            "claude": format!("claude mcp add --transport http {server} {url}"),
            "json": json!({ "mcpServers": { server: { "type": "http", "url": url } } }),
        })
    });
    let registry = state
        .portal
        .get("registry")
        .is_some()
        .then(|| json!({ "search_url": format!("{}/mcp-registry/v0.1/servers?search={}", ctx.base, s.name) }));
    let downloads: Vec<Value> = crate::mocks::collections::FORMATS
        .iter()
        .map(|(f, _, ext)| {
            json!({
                "format": f,
                "file": format!("{}.{ext}", s.name),
                "path": format!("/portal/api/items/mock:{}/collections/{f}", s.name),
            })
        })
        .collect();
    Some(json!({
        "name": s.name,
        "api_title": c.spec.title,
        "version": c.spec.version,
        "description": c.spec.description,
        "state": s.state(),
        "base_url": base,
        "spec_path": format!("/mock/{}/_rustybin/openapi.json", s.name),
        "spec_url": format!("{base}/_rustybin/openapi.json"),
        "spec_yaml_url": format!("{base}/_rustybin/openapi.yaml"),
        "reference_path": format!("/portal/reference/mock:{}", s.name),
        "security": c.credentials_summary(reveal),
        "credentials": reveal,
        "token_url": format!("{}/oauth/token", ctx.base),
        "coverage": { "operations": coverage.operations, "with_examples": coverage.with_examples },
        "operations": operations,
        "downloads": downloads,
        "mcp": mcp,
        "registry": registry,
    }))
}

fn mcp_detail(key: &str, ctx: &Ctx) -> Value {
    let (key, path, title, _) = MCP_SERVERS
        .iter()
        .find(|(k, ..)| *k == key)
        .copied()
        .unwrap_or(MCP_SERVERS[0]);
    let profile = match key {
        "protected" => Profile::protected(),
        "apikey" => Profile::apikey(),
        "main" | "sse" => Profile::open(),
        named => Profile::named(named).unwrap_or_else(Profile::open),
    };
    let version = if key == "sse" {
        crate::mcp::protocol::Version::V2024_11_05
    } else {
        crate::mcp::protocol::Version::LATEST
    };
    let tools: Vec<Value> = crate::mcp::tools::list(&profile)
        .into_iter()
        .map(|t| crate::mcp::tools::tool_json(t, version))
        .collect();
    let url = format!("{}{path}", ctx.base);
    let server = format!("rustybin-{key}");
    let (transport, auth) = match key {
        "sse" => ("sse", json!({ "kind": "none" })),
        "protected" => (
            "http",
            json!({ "kind": "oauth", "hint": "The client discovers the authorization server from the 401 (RFC 9728) and signs in with the built-in identity provider; clients that support MCP OAuth do this on their own." }),
        ),
        "apikey" => (
            "http",
            json!({ "kind": "api_key", "header": "X-API-Key", "hint": "Send any non-empty X-API-Key unless the operator set one." }),
        ),
        _ => ("http", json!({ "kind": "none" })),
    };
    let header = if key == "apikey" {
        " --header \"X-API-Key: demo-key\""
    } else {
        ""
    };
    let mut server_json = json!({ "type": transport, "url": url });
    if key == "apikey" {
        server_json["headers"] = json!({ "X-API-Key": "demo-key" });
    }
    json!({
        "path": path,
        "url": url,
        "title": title,
        "transport": transport,
        "auth": auth,
        "tools": tools,
        "claude": format!("claude mcp add --transport {transport} {server} {url}{header}"),
        "json": json!({ "mcpServers": { server: server_json } }),
        "inspector": format!("/ui/#/mcp/server/{}", path.replace('/', "%2F")),
    })
}

fn llm_detail(ctx: &Ctx) -> Value {
    let providers: Vec<Value> = PROVIDERS
        .iter()
        .map(|(id, title, base, routes, path, headers, body)| {
            let mut hs: Vec<(String, String)> = headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            hs.push(("Content-Type".into(), "application/json".into()));
            let url = format!("{}{path}", ctx.base);
            json!({
                "id": id,
                "title": title,
                "base_url": format!("{}{base}", ctx.base),
                "routes": routes,
                "sample": { "method": "POST", "path": path, "headers": pairs(&hs), "body": body },
                "curl": curl("POST", &url, &hs, Some(body)),
            })
        })
        .collect();
    json!({
        "providers": providers,
        "headers": LLM_HEADERS.iter().map(|(h, e)| json!({ "name": h, "effect": e })).collect::<Vec<_>>(),
        "note": "Any model name is accepted and echoed back; keys are not checked unless the request asks for it.",
    })
}

fn agent_detail(id: &str, ctx: &Ctx) -> Option<Value> {
    let a = crate::a2a::agents::find(id)?;
    let url = format!("{}/a2a/{}", ctx.base, a.id);
    let body = serde_json::to_string_pretty(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": { "message": {
            "messageId": "m-1",
            "role": "ROLE_USER",
            "parts": [{ "text": a.skills.first().and_then(|s| s.examples.first()).copied().unwrap_or("hello agent") }]
        } }
    }))
    .unwrap_or_default();
    let headers = vec![
        ("A2A-Version".to_string(), "1.0".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    Some(json!({
        "id": a.id,
        "name": a.name,
        "description": a.description,
        "url": url,
        "card_url": format!("{url}/.well-known/agent-card.json"),
        "secured": a.secured,
        "input_modes": a.input_modes,
        "output_modes": a.output_modes,
        "skills": a.skills.iter().map(|s| json!({
            "id": s.id, "name": s.name, "description": s.description, "tags": s.tags, "examples": s.examples,
        })).collect::<Vec<_>>(),
        "sample": { "method": "POST", "path": format!("/a2a/{}", a.id), "headers": pairs(&headers), "body": body },
        "curl": curl("POST", &url, &headers, Some(&body)),
    }))
}

fn tools_detail() -> Value {
    let groups = crate::catalog::grouped();
    let categories: Vec<Value> = groups
        .iter()
        .filter(|(name, _)| TOOL_CATEGORIES.contains(name))
        .map(|(name, eps)| {
            json!({
                "name": name,
                "endpoints": eps.iter().map(|ep| crate::ui::endpoint_json(ep)).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "categories": categories })
}

fn registry_detail(state: &AppState, ctx: &Ctx) -> Value {
    let url = format!("{}/mcp-registry", ctx.base);
    json!({
        "url": url,
        "servers_url": format!("{url}/v0.1/servers"),
        "scenario": state.settings.string("registry.scenario").unwrap_or_else(|| "normal".into()),
        "curl": curl("GET", &format!("{url}/v0.1/servers?limit=10"), &[], None),
    })
}
