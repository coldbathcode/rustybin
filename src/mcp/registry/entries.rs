//! The registry's entries: generated for this instance's MCP servers, from
//! `rustybin.yaml` (`registry.servers`) and published through the API, plus
//! what the scenarios add (synthetic pages, stale dates, a broken entry);
//! filtering and cursor pagination as the registry API defines them.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{json, Value};

use super::validate::SCHEMA_URL;
use super::Scenario;
use crate::mocks::visits::Owner;
use crate::state::AppState;

/// The namespace of the generated entries (publishing into it is refused).
pub const NAMESPACE: &str = "io.rustybin";
/// Synthetic entries of the `large` scenario.
pub const LARGE_COUNT: usize = 2000;
/// The `publisher-provided` key the generated entries describe themselves under.
pub const PUBLISHER_META: &str = "io.modelcontextprotocol.registry/publisher-provided";
pub const OFFICIAL_META: &str = "io.modelcontextprotocol.registry/official";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Generated,
    File,
    Published,
    /// Added by a scenario (`large`, `invalid`).
    Scenario,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Generated => "generated",
            Source::File => "file",
            Source::Published => "published",
            Source::Scenario => "scenario",
        }
    }
}

pub const STATUSES: &[&str] = &["active", "deprecated", "deleted"];

/// One server version.
#[derive(Clone, Debug)]
pub struct Entry {
    pub server: Value,
    pub source: Source,
    pub status: &'static str,
    pub status_message: Option<String>,
    pub published_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status_changed_at: DateTime<Utc>,
    pub is_latest: bool,
}

fn ts(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Entry {
    pub fn new(server: Value, source: Source, at: DateTime<Utc>) -> Self {
        Self {
            server,
            source,
            status: "active",
            status_message: None,
            published_at: at,
            updated_at: at,
            status_changed_at: at,
            is_latest: true,
        }
    }

    pub fn name(&self) -> &str {
        self.server
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    pub fn version(&self) -> &str {
        self.server
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    /// The registry's `_meta.io.modelcontextprotocol.registry/official`.
    pub fn official(&self) -> Value {
        let mut m = json!({
            "status": self.status,
            "statusChangedAt": ts(&self.status_changed_at),
            "publishedAt": ts(&self.published_at),
            "updatedAt": ts(&self.updated_at),
            "isLatest": self.is_latest,
        });
        if let Some(msg) = &self.status_message {
            m["statusMessage"] = json!(msg);
        }
        m
    }

    /// A `ServerResponse`.
    pub fn response(&self) -> Value {
        json!({ "server": self.server, "_meta": { OFFICIAL_META: self.official() } })
    }

    /// The cursor naming this entry (`name:version`, as the registry does).
    pub fn cursor(&self) -> String {
        format!("{}:{}", self.name(), self.version())
    }
}

/// Text cut to `max` characters (the schema's limits on description and title).
pub fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

/// A version the schema and the semantic checks accept (`latest` and
/// ranges are not; an empty one neither).
fn exact_version(v: &str) -> String {
    let v = v.trim();
    if v.is_empty()
        || v.len() > 64
        || v == "latest"
        || v.starts_with(['^', '~', '>', '<', '='])
        || v.contains(char::is_whitespace)
    {
        "0.0.0".to_string()
    } else {
        v.to_string()
    }
}

/// The headers a remote declares for an auth kind of the inspector's list.
fn remote_headers(auth: &Value) -> Value {
    match auth["kind"].as_str().unwrap_or("none") {
        "api_key" => json!([{
            "name": auth["name"].as_str().unwrap_or("X-API-Key"),
            "description": "API key",
            "isRequired": true,
            "isSecret": true
        }]),
        "bearer" => json!([{
            "name": "Authorization",
            "description": clip(auth["hint"].as_str().unwrap_or("A bearer token"), 200),
            "isRequired": true,
            "isSecret": true,
            "value": "Bearer {token}",
            "variables": { "token": { "description": "The bearer token", "isRequired": true, "isSecret": true } }
        }]),
        "basic" => json!([{
            "name": "Authorization",
            "description": "Basic credentials of the mock's Security settings",
            "isRequired": true,
            "isSecret": true
        }]),
        "spec" => json!([{
            "name": "Authorization",
            "description": "The credentials the spec's security schemes ask for",
            "isRequired": false,
            "isSecret": true
        }]),
        _ => json!([]),
    }
}

/// The `server.json` of one server of this instance (an item of
/// `inspector::server_list`) under `base`.
pub fn instance_server_json(s: &Value, base: &str, version: &str) -> Value {
    let id = s["id"].as_str().unwrap_or("server");
    let path = s["path"].as_str().unwrap_or("/mcp");
    let legacy = s["transports"]
        .as_array()
        .is_some_and(|t| t.len() == 1 && t[0] == "legacy");
    let mut remote = json!({
        "type": if legacy { "sse" } else { "streamable-http" },
        "url": format!("{base}{path}"),
    });
    let headers = remote_headers(&s["auth"]);
    if headers.as_array().is_some_and(|h| !h.is_empty()) {
        remote["headers"] = headers;
    }
    let title = s["title"].as_str().unwrap_or(id);
    json!({
        "$schema": SCHEMA_URL,
        "name": format!("{NAMESPACE}/{id}"),
        "title": clip(title, 100),
        "description": clip(title, 100),
        "version": exact_version(version),
        "websiteUrl": format!("{base}/ui/#/mcp/server/{}", urlencode(path)),
        "remotes": [remote],
        "_meta": { PUBLISHER_META: { "rustybin": {
            "kind": s["kind"],
            "path": path,
            "auth": s["auth"],
            "mock": s.get("mock").cloned().unwrap_or(Value::Null),
            "tools": s.get("tools").cloned().unwrap_or(Value::Null),
        } } }
    })
}

fn urlencode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The generated entries: the `/mcp` variants, the management server and
/// each instance mock whose MCP server is serving.
pub fn generated(state: &AppState, base: &str, started: DateTime<Utc>) -> Vec<Entry> {
    let list = crate::mcp::inspector::server_list(state, |s| matches!(s.owner, Owner::Instance));
    let mocks = state.mocks.list();
    let mut out = Vec::new();
    for s in &list {
        if s["state"] != "serving" {
            continue;
        }
        match s["mock"].as_str() {
            Some(name) => {
                let Some(snap) = mocks.iter().find(|m| m.name == name) else {
                    continue;
                };
                let Some(live) = snap.live.as_ref() else {
                    continue;
                };
                let mut server = instance_server_json(s, base, &live.spec.version);
                server["title"] = json!(clip(&live.spec.title, 100));
                let description = live
                    .spec
                    .description
                    .as_deref()
                    .map(|d| d.split(['\n', '.']).next().unwrap_or(d).trim().to_string())
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| {
                        format!(
                            "Mock API {} as MCP tools (one per operation)",
                            live.spec.title
                        )
                    });
                server["description"] = json!(clip(&description, 100));
                server["name"] = json!(format!("{NAMESPACE}/mock-{name}"));
                let mut e = Entry::new(server, Source::Generated, snap.created_at);
                e.updated_at = live.created_at.max(snap.created_at);
                out.push(e);
            }
            None => {
                let mut server = instance_server_json(s, base, env!("CARGO_PKG_VERSION"));
                if s["id"] == "manage" {
                    server["name"] = json!(format!("{NAMESPACE}/manage"));
                }
                out.push(Entry::new(server, Source::Generated, started));
            }
        }
    }
    out
}

/// Synthetic entry `i` (1-based) of the `large` scenario; every tenth
/// server has a second version, every fifth a package.
pub fn synthetic(i: usize) -> Vec<Entry> {
    let at = Utc
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .unwrap_or_else(Utc::now)
        + chrono::Duration::minutes(i as i64);
    let name = format!("io.example.synthetic/server-{i:04}");
    let mut server = json!({
        "$schema": SCHEMA_URL,
        "name": name,
        "title": format!("Synthetic server {i}"),
        "description": format!("Synthetic entry {i} of {LARGE_COUNT} (the large scenario, for pagination tests)"),
        "version": "1.0.0",
        "remotes": [{ "type": "streamable-http", "url": format!("https://server-{i:04}.synthetic.example/mcp") }]
    });
    if i.is_multiple_of(5) {
        server["packages"] = json!([{
            "registryType": "npm",
            "identifier": format!("@synthetic/server-{i:04}"),
            "version": "1.0.0",
            "transport": { "type": "stdio" }
        }]);
    }
    let mut out = vec![Entry::new(server.clone(), Source::Scenario, at)];
    if i.is_multiple_of(10) {
        server["version"] = json!("1.1.0");
        if let Some(p) = server
            .get_mut("packages")
            .and_then(|p| p.get_mut(0))
            .and_then(Value::as_object_mut)
        {
            p.insert("version".into(), json!("1.1.0"));
        }
        out.push(Entry::new(
            server,
            Source::Scenario,
            at + chrono::Duration::days(1),
        ));
    }
    out
}

/// The entry of the `invalid` scenario: no namespace, no description, a
/// transport the schema does not know.
pub fn invalid_entry(at: DateTime<Utc>) -> Entry {
    Entry::new(
        json!({
            "name": "invalid entry without a namespace",
            "version": "",
            "remotes": [{ "type": "websocket", "url": "ws://invalid.example/mcp" }]
        }),
        Source::Scenario,
        at,
    )
}

/// The date every entry carries in the `stale` scenario.
pub fn stale_date() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 1, 15, 9, 30, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Mark the latest version per name (the last published; generated and
/// file entries have one version each). `list` is sorted by name.
pub fn mark_latest(list: &mut [Entry]) {
    let mut i = 0;
    while i < list.len() {
        let mut j = i;
        while j < list.len() && list[j].name() == list[i].name() {
            j += 1;
        }
        let latest = (i..j)
            .filter(|&k| list[k].status != "deleted")
            .max_by_key(|&k| list[k].published_at)
            .unwrap_or(j - 1);
        for (k, e) in list.iter_mut().enumerate().take(j).skip(i) {
            e.is_latest = k == latest;
        }
        i = j;
    }
}

/// Every entry under the scenario, sorted by name then version (text).
pub fn assemble(mut entries: Vec<Entry>, scenario: Scenario, at: DateTime<Utc>) -> Vec<Entry> {
    match scenario {
        Scenario::Empty => return Vec::new(),
        Scenario::Large => {
            for i in 1..=LARGE_COUNT {
                entries.extend(synthetic(i));
            }
        }
        Scenario::Invalid => entries.push(invalid_entry(at)),
        _ => {}
    }
    if scenario == Scenario::Stale {
        let old = stale_date();
        for e in &mut entries {
            e.published_at = old;
            e.updated_at = old;
            e.status_changed_at = old;
        }
    }
    entries.sort_by(|a, b| {
        a.name()
            .cmp(b.name())
            .then_with(|| a.version().cmp(b.version()))
    });
    mark_latest(&mut entries);
    entries
}

/// The list filters of `GET /v0.1/servers`.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub search: Option<String>,
    pub version: Option<String>,
    pub updated_since: Option<DateTime<Utc>>,
    pub include_deleted: bool,
    pub cursor: Option<String>,
    pub limit: usize,
}

/// One page and the cursor of the next (when more entries follow).
pub fn page(entries: &[Entry], f: &Filter) -> (Vec<Entry>, Option<String>) {
    let search = f.search.as_deref().map(str::to_ascii_lowercase);
    let include_deleted = f.include_deleted || f.updated_since.is_some();
    let after = f.cursor.as_deref().map(|c| match c.split_once(':') {
        Some((n, v)) => (n.to_string(), v.to_string()),
        None => (c.to_string(), String::new()),
    });
    let mut matching = entries.iter().filter(|e| {
        (include_deleted || e.status != "deleted")
            && search
                .as_deref()
                .is_none_or(|s| e.name().to_ascii_lowercase().contains(s))
            && match f.version.as_deref() {
                None => true,
                Some("latest") => e.is_latest,
                Some(v) => e.version() == v,
            }
            && f.updated_since.is_none_or(|t| e.updated_at > t)
            && after
                .as_ref()
                .is_none_or(|(n, v)| (e.name(), e.version()) > (n.as_str(), v.as_str()))
    });
    let items: Vec<Entry> = matching.by_ref().take(f.limit).cloned().collect();
    let next = if items.len() == f.limit && matching.next().is_some() {
        items.last().map(Entry::cursor)
    } else {
        None
    };
    (items, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, version: &str, minutes: i64) -> Entry {
        Entry::new(
            json!({ "name": name, "version": version, "description": "d" }),
            Source::Published,
            Utc::now() + chrono::Duration::minutes(minutes),
        )
    }

    #[test]
    fn latest_is_the_last_published() {
        let mut list = vec![
            e("a/x", "1.0.0", 0),
            e("a/x", "2.0.0", -5),
            e("b/y", "1", 0),
        ];
        list.sort_by(|a, b| a.name().cmp(b.name()).then(a.version().cmp(b.version())));
        mark_latest(&mut list);
        assert!(list[0].is_latest, "1.0.0 was published last");
        assert!(!list[1].is_latest);
        assert!(list[2].is_latest);
    }

    #[test]
    fn pages_follow_the_cursor() {
        let all = assemble(Vec::new(), Scenario::Large, Utc::now());
        assert_eq!(all.len(), LARGE_COUNT + LARGE_COUNT / 10);
        let mut f = Filter {
            limit: 100,
            ..Filter::default()
        };
        let mut seen = 0;
        let mut pages = 0;
        loop {
            let (items, next) = page(&all, &f);
            seen += items.len();
            pages += 1;
            match next {
                Some(c) => f.cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seen, all.len());
        assert_eq!(pages, all.len().div_ceil(100));
        // Latest only: one per name.
        f.cursor = None;
        f.version = Some("latest".into());
        f.limit = 100;
        let (items, _) = page(&all, &f);
        assert!(items.iter().all(|e| e.is_latest));
        let tenth: Vec<_> = items
            .iter()
            .filter(|e| e.name() == "io.example.synthetic/server-0010")
            .collect();
        assert_eq!(tenth.len(), 1);
        assert_eq!(tenth[0].version(), "1.1.0");
        // Search is a substring of the name.
        let f = Filter {
            search: Some("SERVER-01".into()),
            limit: 30,
            ..Filter::default()
        };
        let (items, next) = page(&all, &f);
        assert!(items.iter().all(|e| e.name().contains("server-01")));
        assert!(next.is_some());
        // A cursor that is not an entry still orders as text.
        let f = Filter {
            cursor: Some("io.example.synthetic/server-1999".into()),
            limit: 30,
            ..Filter::default()
        };
        let (items, _) = page(&all, &f);
        assert_eq!(items[0].name(), "io.example.synthetic/server-1999");
    }

    #[test]
    fn deleted_and_updated_since() {
        let mut list = vec![e("a/x", "1", -60), e("a/y", "1", 0)];
        list[0].status = "deleted";
        let f = Filter {
            limit: 10,
            ..Filter::default()
        };
        assert_eq!(page(&list, &f).0.len(), 1);
        let f = Filter {
            limit: 10,
            include_deleted: true,
            ..Filter::default()
        };
        assert_eq!(page(&list, &f).0.len(), 2);
        let f = Filter {
            limit: 10,
            updated_since: Some(Utc::now() - chrono::Duration::minutes(30)),
            ..Filter::default()
        };
        let (items, _) = page(&list, &f);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name(), "a/y");
    }

    #[test]
    fn scenarios_shape_the_list() {
        let base = vec![e("a/x", "1", 0)];
        assert!(assemble(base.clone(), Scenario::Empty, Utc::now()).is_empty());
        let stale = assemble(base.clone(), Scenario::Stale, Utc::now());
        assert_eq!(stale[0].updated_at, stale_date());
        let invalid = assemble(base, Scenario::Invalid, Utc::now());
        assert_eq!(invalid.len(), 2);
        let bad = invalid
            .iter()
            .find(|e| e.source == Source::Scenario)
            .unwrap();
        assert_eq!(
            super::super::validate::validate(&bad.server)["valid"],
            false
        );
        for e in synthetic(10) {
            assert_eq!(
                super::super::validate::validate(&e.server)["valid"],
                true,
                "{}",
                e.server
            );
        }
    }
}
