//! Push notifications: URL policy (SSRF protection), delivery and the
//! built-in webhook sink (`/a2a/webhook-sink/{id}`).
//!
//! Policy (checked when a config is created):
//! - URLs pointing at this server's own sink (same host as the request, or a
//!   bare `/a2a/webhook-sink/{id}` path) are always accepted and delivered
//!   in-process, so nothing leaves the server.
//! - Hosts listed in `RUSTYBIN_A2A_PUSH_ALLOWLIST` (comma separated `host`
//!   or `host:port`) are delivered over HTTP(S).
//! - `RUSTYBIN_A2A_PUSH_ALLOW_ALL=true` accepts any http(s) URL, including
//!   localhost and private addresses, but never in public mode.
//! - Everything else is rejected.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Semaphore;

use super::model::PushConfig;

/// Maximum concurrent outbound deliveries (extra notifications are dropped).
const MAX_INFLIGHT: usize = 32;
/// Outbound delivery timeout (spec recommends 10 to 30 seconds).
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default)]
pub struct PushPolicy {
    pub allowlist: Vec<String>,
    pub allow_all: bool,
    pub public_mode: bool,
}

/// Where a validated push URL goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Sink(String),
    Remote(String),
}

pub const SINK_PREFIX: &str = "/a2a/webhook-sink/";

/// Sink ids: 1..64 of `[A-Za-z0-9._-]`.
pub fn valid_sink_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn sink_id_from_path(path: &str) -> Option<String> {
    let id = path.strip_prefix(SINK_PREFIX)?.trim_end_matches('/');
    valid_sink_id(id).then(|| id.to_string())
}

impl PushPolicy {
    pub fn from_env(public_mode: bool) -> Self {
        let allowlist = std::env::var("RUSTYBIN_A2A_PUSH_ALLOWLIST")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let allow_all = std::env::var("RUSTYBIN_A2A_PUSH_ALLOW_ALL")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        Self {
            allowlist,
            allow_all,
            public_mode,
        }
    }

    /// Validate a client supplied webhook URL. `own_authority` is the
    /// `host[:port]` the client used to reach this server.
    pub fn check(&self, url: &str, own_authority: &str) -> Result<Target, String> {
        let url = url.trim();
        if url.starts_with('/') {
            return sink_id_from_path(url).map(Target::Sink).ok_or_else(|| {
                format!("relative push URLs must be {SINK_PREFIX}{{id}} (id: 1-64 of A-Z a-z 0-9 . _ -)")
            });
        }
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid push URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("push URLs must use http or https".into());
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("push URLs must not contain credentials".into());
        }
        let host = parsed
            .host_str()
            .ok_or("push URL has no host")?
            .to_ascii_lowercase();
        let authority = match parsed.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        if authority == own_authority.to_ascii_lowercase() {
            if let Some(id) = sink_id_from_path(parsed.path()) {
                return Ok(Target::Sink(id));
            }
        }
        if self.allowlist.iter().any(|a| *a == host || *a == authority) {
            return Ok(Target::Remote(parsed.to_string()));
        }
        if self.allow_all && !self.public_mode {
            return Ok(Target::Remote(parsed.to_string()));
        }
        Err(format!(
            "push URL {url} is not allowed: use this server's own sink ({SINK_PREFIX}{{id}} on {own_authority}) \
             or a host listed in RUSTYBIN_A2A_PUSH_ALLOWLIST (RUSTYBIN_A2A_PUSH_ALLOW_ALL=true allows any host outside public mode)"
        ))
    }
}

// ── Webhook sink ────────────────────────────────────────────────────

struct SinkEntry {
    touched: Instant,
    items: VecDeque<Value>,
}

/// A tiny request bin for push notifications. Bounded: at most `max_sinks`
/// ids (least recently used evicted), `max_items` notifications each, and
/// idle sinks expire after `ttl`.
pub struct WebhookSink {
    inner: Mutex<HashMap<String, SinkEntry>>,
    max_sinks: usize,
    max_items: usize,
    ttl: Duration,
}

/// Largest stored notification body (bytes of JSON text).
const MAX_STORED_BODY: usize = 64 * 1024;

impl WebhookSink {
    pub fn new(max_sinks: usize, max_items: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_sinks,
            max_items,
            ttl,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SinkEntry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record one notification; returns the number stored for `id`.
    pub fn record(&self, id: &str, headers: Value, body: Value, source: &str) -> usize {
        let body = if body.to_string().len() > MAX_STORED_BODY {
            json!({ "truncated": true, "note": "body larger than 64 KiB was not stored" })
        } else {
            body
        };
        let item = json!({
            "receivedAt": super::model::now_ts(),
            "source": source,
            "headers": headers,
            "body": body,
        });
        let mut map = self.lock();
        let ttl = self.ttl;
        map.retain(|_, e| e.touched.elapsed() < ttl);
        if !map.contains_key(id) && map.len() >= self.max_sinks {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        let entry = map.entry(id.to_string()).or_insert_with(|| SinkEntry {
            touched: Instant::now(),
            items: VecDeque::new(),
        });
        entry.touched = Instant::now();
        entry.items.push_back(item);
        while entry.items.len() > self.max_items {
            entry.items.pop_front();
        }
        entry.items.len()
    }

    pub fn list(&self, id: &str) -> Vec<Value> {
        let map = self.lock();
        map.get(id)
            .filter(|e| e.touched.elapsed() < self.ttl)
            .map(|e| e.items.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear(&self, id: &str) -> bool {
        self.lock().remove(id).is_some()
    }
}

// ── Delivery ────────────────────────────────────────────────────────

/// rustls client configuration (aws-lc-rs provider, Mozilla roots).
fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

pub struct PushService {
    pub policy: PushPolicy,
    pub sink: Arc<WebhookSink>,
    client: Option<reqwest::Client>,
    permits: Arc<Semaphore>,
}

impl PushService {
    pub fn new(policy: PushPolicy, sink: Arc<WebhookSink>) -> Self {
        // TLS with an explicit provider: the process may have several rustls
        // providers compiled in, so never rely on a process-wide default.
        let mut builder = reqwest::Client::builder();
        match tls_config() {
            Ok(tls) => builder = builder.use_preconfigured_tls(tls),
            Err(e) => tracing::warn!("A2A push: https webhooks unavailable ({e})"),
        }
        let client = builder
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DELIVERY_TIMEOUT)
            .user_agent(concat!("rustybin-a2a/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| tracing::warn!("A2A push client unavailable: {e}"))
            .ok();
        Self {
            policy,
            sink,
            client,
            permits: Arc::new(Semaphore::new(MAX_INFLIGHT)),
        }
    }

    /// Deliver `payload` for one config (fire and forget).
    pub fn deliver(&self, cfg: &PushConfig, payload: Value) {
        let content_type = match cfg.version {
            super::model::Version::V10 => "application/a2a+json",
            super::model::Version::V03 => "application/json",
        };
        let mut headers: Vec<(String, String)> = vec![("content-type".into(), content_type.into())];
        if let Some(t) = &cfg.token {
            headers.push(("x-a2a-notification-token".into(), t.clone()));
        }
        if let Some(a) = &cfg.authentication {
            match &a.credentials {
                Some(c) => headers.push(("authorization".into(), format!("{} {}", a.scheme, c))),
                None => headers.push(("authorization".into(), a.scheme.clone())),
            }
        }
        if let Some(id) = &cfg.sink_id {
            let h: serde_json::Map<String, Value> = headers
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect();
            self.sink
                .record(id, Value::Object(h), payload, "in-process push");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            tracing::warn!(task_id = %cfg.task_id, "A2A push dropped: too many deliveries in flight");
            return;
        };
        let url = cfg.url.clone();
        let task_id = cfg.task_id.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut req = client.post(&url).body(payload.to_string());
            for (k, v) in headers {
                req = req.header(k, v);
            }
            match req.send().await {
                Ok(resp) => {
                    tracing::debug!(%task_id, %url, status = %resp.status(), "A2A push delivered")
                }
                Err(e) => tracing::warn!(%task_id, %url, "A2A push failed: {e}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PushPolicy {
        PushPolicy {
            allowlist: vec!["hooks.example.com".into(), "10.1.2.3:9000".into()],
            allow_all: false,
            public_mode: false,
        }
    }

    #[test]
    fn own_sink_is_allowed() {
        let p = policy();
        assert_eq!(
            p.check(
                "http://localhost:8080/a2a/webhook-sink/abc",
                "localhost:8080"
            ),
            Ok(Target::Sink("abc".into()))
        );
        assert_eq!(
            p.check("/a2a/webhook-sink/x-1", "h"),
            Ok(Target::Sink("x-1".into()))
        );
        assert!(p.check("/a2a/webhook-sink/", "h").is_err());
        assert!(p.check("/etc/passwd", "h").is_err());
    }

    #[test]
    fn ssrf_targets_are_rejected() {
        let p = policy();
        for url in [
            "http://127.0.0.1/a2a/webhook-sink/abc",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost:8080/admin",
            "http://10.0.0.1/hook",
            "http://[::1]/hook",
            "file:///etc/passwd",
            "gopher://x",
            "http://user:pw@hooks.example.com/",
            "https://evil.example.org/hook",
        ] {
            assert!(
                p.check(url, "localhost:8080").is_err(),
                "{url} must be rejected"
            );
        }
    }

    #[test]
    fn allowlist_and_allow_all() {
        let p = policy();
        assert!(matches!(
            p.check("https://hooks.example.com/a2a", "h"),
            Ok(Target::Remote(_))
        ));
        assert!(matches!(
            p.check("http://10.1.2.3:9000/x", "h"),
            Ok(Target::Remote(_))
        ));
        assert!(p.check("http://10.1.2.3:9001/x", "h").is_err());
        let mut all = policy();
        all.allow_all = true;
        assert!(all.check("http://127.0.0.1:9/x", "h").is_ok());
        all.public_mode = true;
        assert!(all.check("http://127.0.0.1:9/x", "h").is_err());
    }

    #[test]
    fn push_client_builds_without_a_default_crypto_provider() {
        assert!(tls_config().is_ok());
        let svc = PushService::new(
            policy(),
            Arc::new(WebhookSink::new(1, 1, Duration::from_secs(1))),
        );
        assert!(svc.client.is_some());
    }

    #[test]
    fn sink_is_bounded() {
        let s = WebhookSink::new(2, 3, Duration::from_secs(60));
        for i in 0..5 {
            s.record("a", json!({}), json!({ "n": i }), "test");
        }
        let items = s.list("a");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["body"]["n"], 2);
        s.record("b", json!({}), json!(1), "test");
        s.record("c", json!({}), json!(1), "test");
        assert!(s.list("a").is_empty(), "oldest sink evicted");
        assert!(s.clear("c"));
        assert!(!s.clear("c"));
    }
}
