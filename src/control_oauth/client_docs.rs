//! Client ID metadata documents on the built-in authorization server: an
//! agent's client names itself to `/_rustybin/oauth/authorize` with the
//! https URL of a document that describes it, instead of registering.
//!
//! To honour one the instance fetches that URL, a call out on the word of a
//! caller who has not signed in yet. So:
//!
//! - it is off until someone turns it on (the setting
//!   [`SETTING`], which a document from git never changes), and the
//!   authorization server's metadata says `false` while it is off;
//! - the fetch goes through the outbound policy under
//!   `Purpose::ClientMetadata`, which has no default: only hosts the operator
//!   named in `egress.allow` are ever called, and a host outside it is
//!   refused before any call;
//! - `https` only (plain `http` only for a loopback host, which the policy
//!   refuses unless `egress.allow_loopback` is on: a document served on the
//!   operator's own computer), no redirect followed, at most
//!   [`MAX_DOCUMENT_BYTES`], a short timeout;
//! - the document's `client_id` must be the URL it was fetched from, and it
//!   describes a public client with redirect URIs registration would take;
//! - answers are kept for [`CACHE_SECS`] (refusals for [`REFUSAL_SECS`]) in
//!   a bounded cache, so a loop of sign-in requests is not a loop of calls,
//!   and a sign-in request counts against the address's allowance before
//!   the instance calls out for it;
//! - what the document says (its name) is the client's own claim: the
//!   consent view shows it as that, beside the host it was fetched from.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::Client;
use crate::egress::Purpose;
use crate::settings::{Kind, Settings, Spec};
use crate::state::AppState;

/// The switch (off by default).
pub const SETTING: &str = "oauth.client_metadata_documents";
/// Largest document read.
pub const MAX_DOCUMENT_BYTES: usize = 5 * 1024;
/// Longest client id URL.
pub const MAX_URL_LEN: usize = 1024;
/// Redirect URIs a document may list.
pub const MAX_REDIRECT_URIS: usize = 10;
/// How long a fetched document is used before it is fetched again.
pub const CACHE_SECS: u64 = 300;
/// How long a refusal is answered again without another call.
pub const REFUSAL_SECS: u64 = 30;
/// Documents and refusals kept.
pub const MAX_CACHED: usize = 64;
/// The whole fetch.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

pub fn specs() -> Vec<Spec> {
    vec![Spec::new(
        SETTING,
        "agents",
        "Client ID metadata documents",
        "Lets an agent's client name itself at the OAuth sign-in to /_rustybin/mcp with the https URL of a document that describes it, instead of registering. The instance fetches that URL before anyone has signed in, so it is off by default and reaches only hosts named in the outbound allowlist (egress.allow; purpose client_metadata has no default): https only, no redirects, at most 5 KiB, kept for five minutes. The consent page shows the document's host and the name it claims. A document from git never changes this setting.",
        Kind::Bool,
        json!(false),
    )]
}

/// Is the feature on?
pub fn on(settings: &Settings) -> bool {
    settings.bool(SETTING)
}

/// The diagnostics check `client_documents`: the feature is on and the
/// outbound policy permits no host for it, so every client's document is
/// refused before any call. `None` (no check) while the feature is off.
/// Whether anything is permitted is the policy's own answer
/// ([`crate::egress::Egress::permits_any`]), never a reading of its lists.
pub fn diagnostics(state: &AppState) -> Option<(&'static str, String, Option<&'static str>)> {
    if !on(&state.settings) {
        return None;
    }
    let purpose = Purpose::ClientMetadata;
    Some(if state.egress.permits_any(purpose) {
        (
            "ok",
            format!(
                "{SETTING} is on and the outbound policy names hosts for purpose {}: a client's document is fetched from those only",
                purpose.as_str()
            ),
            None,
        )
    } else {
        (
            "warn",
            format!(
                "{SETTING} is on but the outbound policy permits no host for purpose {} (it has no default and egress.allow is empty): every client's document is refused before any call, and a sign-in that names one stops at an error page",
                purpose.as_str()
            ),
            Some("name the hosts the clients' documents are served from in egress.allow (Management, Network, or RUSTYBIN_EGRESS_ALLOW), or turn oauth.client_metadata_documents off; GET /_rustybin/egress/check?purpose=client_metadata&url=... says what one URL would be told"),
        )
    })
}

/// Does this client id claim to be a document's URL (rather than a
/// registered client's signed id)?
pub fn looks_like_document(id: &str) -> bool {
    let lower = id.get(..8).unwrap_or(id).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The rules a client id URL follows before anything is fetched: `https`
/// (`http` on a loopback host only), a host, a path that names a document,
/// no credentials, no fragment, no dot segments, a bounded length.
pub fn check_url(id: &str) -> Result<reqwest::Url, String> {
    if id.len() > MAX_URL_LEN {
        return Err(format!(
            "the client id is longer than {MAX_URL_LEN} characters"
        ));
    }
    if id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the client id holds whitespace or control characters".into());
    }
    if id.contains('#') {
        return Err("the client id must not have a fragment".into());
    }
    // Checked on the text: a URL parser resolves dot segments away.
    let after_scheme = id.split_once("://").map(|(_, r)| r).unwrap_or("");
    let path = after_scheme
        .split(['?', '#'])
        .next()
        .and_then(|a| a.find('/').map(|i| &a[i..]))
        .unwrap_or("");
    if path.split('/').any(|seg| seg == "." || seg == "..") {
        return Err("the client id must not have . or .. path segments".into());
    }
    if path.is_empty() || path == "/" {
        return Err("the client id must name a document (a URL with a path)".into());
    }
    let url = reqwest::Url::parse(id).map_err(|_| "the client id is not a URL".to_string())?;
    let host = url
        .host_str()
        .ok_or("the client id has no host")?
        .to_string();
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the client id must not carry credentials".into());
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&host) => {}
        _ => return Err("the client id must be an https URL".into()),
    }
    Ok(url)
}

/// The host a document was fetched from, as the consent view shows it.
pub fn host_of(id: &str) -> String {
    reqwest::Url::parse(id)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host,
            })
        })
        .unwrap_or_default()
}

/// What a fetch gave: the client, or why it was refused.
type Fetched = Result<Client, String>;

/// Fetched documents and refusals, by client id: bounded, short-lived.
#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<String, (Instant, Fetched)>>,
}

impl Cache {
    fn life(entry: &Result<Client, String>) -> Duration {
        Duration::from_secs(match entry {
            Ok(_) => CACHE_SECS,
            Err(_) => REFUSAL_SECS,
        })
    }

    fn get(&self, id: &str) -> Option<Result<Client, String>> {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(id) {
            Some((at, entry)) if at.elapsed() < Self::life(entry) => Some(entry.clone()),
            Some(_) => {
                map.remove(id);
                None
            }
            None => None,
        }
    }

    fn put(&self, id: &str, entry: Result<Client, String>) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(id) && map.len() >= MAX_CACHED {
            map.retain(|_, (at, e)| at.elapsed() < Self::life(e));
            if map.len() >= MAX_CACHED {
                let oldest = map
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    map.remove(&k);
                }
            }
        }
        map.insert(id.to_string(), (Instant::now(), entry));
    }

    #[cfg(test)]
    pub fn put_for_tests(&self, id: &str) {
        self.put(id, Err("test".into()));
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The client a document describes, or why it is refused. `body` is what
/// came back from `id`.
pub(super) fn parse(id: &str, body: &[u8]) -> Result<Client, String> {
    let doc: Value =
        serde_json::from_slice(body).map_err(|_| "the document is not JSON".to_string())?;
    if !doc.is_object() {
        return Err("the document is not a JSON object".into());
    }
    // The one thing that ties the document to the URL it came from.
    if doc.get("client_id").and_then(Value::as_str) != Some(id) {
        return Err("the document's client_id is not the URL it was fetched from".into());
    }
    if ["client_secret", "client_secret_expires_at"]
        .iter()
        .any(|k| doc.get(*k).is_some())
    {
        return Err(
            "the document carries a client secret: only public clients are supported".into(),
        );
    }
    if let Some(m) = doc.get("token_endpoint_auth_method") {
        if m.as_str() != Some("none") {
            return Err(
                "only public clients are supported (token_endpoint_auth_method none, with PKCE)"
                    .into(),
            );
        }
    }
    if let Some(gt) = doc.get("grant_types") {
        let ok = gt.as_array().is_some_and(|a| {
            a.iter()
                .all(|g| matches!(g.as_str(), Some("authorization_code" | "refresh_token")))
        });
        if !ok {
            return Err("grant_types may only be authorization_code and refresh_token".into());
        }
    }
    if let Some(rt) = doc.get("response_types") {
        let ok = rt
            .as_array()
            .is_some_and(|a| a.iter().all(|r| r.as_str() == Some("code")));
        if !ok {
            return Err("response_types may only be code".into());
        }
    }
    let uris = doc
        .get("redirect_uris")
        .and_then(Value::as_array)
        .ok_or("the document lists no redirect_uris")?;
    if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
        return Err(format!(
            "the document must list 1 to {MAX_REDIRECT_URIS} redirect URIs"
        ));
    }
    let mut redirect_uris = Vec::new();
    for u in uris {
        let u = u.as_str().ok_or("redirect URIs must be strings")?;
        super::validate_redirect_uri(u).map_err(str::to_string)?;
        redirect_uris.push(u.to_string());
    }
    let name: String = doc
        .get("client_name")
        .and_then(Value::as_str)
        .unwrap_or("MCP client")
        .chars()
        .filter(|c| !c.is_control())
        .take(super::MAX_CLIENT_NAME)
        .collect();
    Ok(Client {
        redirect_uris,
        name,
    })
}

/// Fetch the document at `id` from its pinned, allowed destination.
async fn fetch(app: &AppState, id: &str, pinned: &crate::egress::Pinned) -> Result<Client, String> {
    let client = app
        .egress
        .client(pinned, FETCH_TIMEOUT)
        .map_err(|_| "the document could not be fetched".to_string())?;
    let unreachable = |_| "the document could not be fetched".to_string();
    let mut resp = client
        .get(&pinned.url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(unreachable)?;
    let status = resp.status();
    if status.is_redirection() {
        return Err(format!(
            "the document's URL answered {}: a redirect is not followed",
            status.as_u16()
        ));
    }
    if status != reqwest::StatusCode::OK {
        return Err(format!("the document's URL answered {}", status.as_u16()));
    }
    let too_large = || format!("the document is larger than {MAX_DOCUMENT_BYTES} bytes");
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_DOCUMENT_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(unreachable)? {
        if body.len() + chunk.len() > MAX_DOCUMENT_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    parse(id, &body)
}

/// The client `id` names: from the cache, else fetched. The error is this
/// module's own words, never the document's.
pub(super) async fn client(app: &AppState, cache: &Cache, id: &str) -> Result<Client, String> {
    check_url(id)?;
    if let Some(known) = cache.get(id) {
        return known;
    }
    // Refused here, before any call, when the operator did not name the
    // host. Not kept: nothing was called, and the operator may be about to
    // allow it.
    let pinned = app
        .egress
        .resolve(id, Purpose::ClientMetadata)
        .await
        .map_err(|e| format!("the outbound policy refused its host: {e}"))?;
    let fetched = fetch(app, id, &pinned).await;
    cache.put(id, fetched.clone());
    tracing::info!(
        target: "rustybin::control_oauth",
        client = %super::fingerprint(id),
        host = %host_of(id),
        accepted = fetched.is_ok(),
        "client metadata document fetched"
    );
    fetched
}
