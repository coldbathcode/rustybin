//! Captured email addresses (self-service visits, `docs/plans/hosted-mocks.md`
//! section 8): posted to the operator's webhook (`RUSTYBIN_LEADS_WEBHOOK_URL`,
//! any automation or CRM endpoint) and optionally appended to a JSON lines
//! file (`RUSTYBIN_LEADS_FILE`).
//!
//! An address is personal data: it is never logged, captured or counted
//! with a label, and it only stays in memory until delivered. The queue is
//! bounded ([`MAX_QUEUE`]); a lead that cannot be delivered for an hour is
//! dropped (and counted).

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

use crate::config::SelfServiceConfig;
use crate::metrics::Metrics;

/// Leads waiting for the webhook.
pub const MAX_QUEUE: usize = 1_000;
/// How long a lead is retried.
const GIVE_UP_AFTER: Duration = Duration::from_secs(3600);
const POST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize)]
pub struct Lead {
    pub email: String,
    /// The visitor ticked "send me product news" (always false with the
    /// `notice` consent mode, where there is no checkbox).
    pub marketing_consent: bool,
    /// `checkbox` or `notice`: what the form showed.
    pub consent_mode: &'static str,
    /// `mock_start` (asked before the first mock) or `mock_extend`.
    pub source: &'static str,
    pub captured_at: DateTime<Utc>,
    /// Minutes of mock time the visit had when the address was given.
    pub visit_minutes: u64,
    /// Names of the visit's mocks (no content).
    pub mocks: Vec<String>,
}

struct Pending {
    lead: Lead,
    first: Instant,
    next: Instant,
    attempts: u32,
}

pub struct Leads {
    webhook: Option<String>,
    token: Option<String>,
    file: Option<PathBuf>,
    queue: Mutex<VecDeque<Pending>>,
    notify: Notify,
    metrics: Option<Arc<Metrics>>,
}

impl Leads {
    pub fn new(cfg: &SelfServiceConfig, metrics: Arc<Metrics>) -> Self {
        let on = cfg.enabled && cfg.leads_enabled();
        Self {
            webhook: cfg.leads_webhook_url.clone().filter(|_| on),
            token: cfg.leads_webhook_token.clone(),
            file: cfg.leads_file.clone().filter(|_| on),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            metrics: Some(metrics),
        }
    }

    /// Delivers nothing (tests, self-service off).
    pub fn disabled() -> Self {
        Self {
            webhook: None,
            token: None,
            file: None,
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            metrics: None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Pending>> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn count(&self, result: &'static str) {
        if let Some(m) = &self.metrics {
            m.count_lead(result);
        }
    }

    pub fn pending(&self) -> usize {
        self.lock().len()
    }

    /// Hand a lead over: appended to the file now, queued for the webhook.
    pub fn submit(&self, lead: Lead) {
        if self.webhook.is_none() && self.file.is_none() {
            return;
        }
        self.count("captured");
        if let Some(path) = &self.file {
            let line = serde_json::to_string(&lead).unwrap_or_default();
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                tracing::warn!("lead not written to {}: {e}", path.display());
            }
        }
        if self.webhook.is_none() {
            return;
        }
        let mut q = self.lock();
        if q.len() >= MAX_QUEUE {
            drop(q);
            self.count("dropped");
            tracing::warn!("lead dropped: the delivery queue is full");
            return;
        }
        let now = Instant::now();
        q.push_back(Pending {
            lead,
            first: now,
            next: now,
            attempts: 0,
        });
        drop(q);
        self.notify.notify_one();
    }

    fn due(&self) -> Vec<Pending> {
        let now = Instant::now();
        let mut q = self.lock();
        let (due, wait): (VecDeque<Pending>, VecDeque<Pending>) =
            q.drain(..).partition(|p| p.next <= now);
        *q = wait;
        due.into()
    }

    fn requeue(&self, mut p: Pending) {
        if p.first.elapsed() >= GIVE_UP_AFTER {
            self.count("dropped");
            tracing::warn!("lead dropped after {} delivery attempts", p.attempts);
            return;
        }
        let backoff = Duration::from_secs(5u64.saturating_mul(1 << p.attempts.min(7)));
        p.next = Instant::now() + backoff.min(Duration::from_secs(600));
        self.count("retried");
        let mut q = self.lock();
        if q.len() < MAX_QUEUE {
            q.push_back(p);
        } else {
            drop(q);
            self.count("dropped");
        }
    }

    async fn post(&self, client: &reqwest::Client, url: &str, lead: &Lead) -> Result<(), String> {
        let body = serde_json::to_vec(lead).map_err(|e| e.to_string())?;
        let mut req = client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("status {}", r.status().as_u16())),
            Err(e) => Err(if e.is_timeout() {
                "timeout".to_string()
            } else {
                "connection failed".to_string()
            }),
        }
    }

    /// Deliver queued leads until `shutdown` resolves (a no-op without a
    /// webhook).
    pub async fn run<F>(self: Arc<Self>, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        let Some(url) = self.webhook.clone() else {
            return;
        };
        let client = match client() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("lead delivery disabled: {e}");
                return;
            }
        };
        tokio::pin!(shutdown);
        loop {
            for mut p in self.due() {
                p.attempts += 1;
                match self.post(&client, &url, &p.lead).await {
                    Ok(()) => self.count("delivered"),
                    Err(e) => {
                        tracing::warn!("lead delivery failed ({e}), will retry");
                        self.requeue(p);
                    }
                }
            }
            tokio::select! {
                _ = &mut shutdown => {
                    let left = self.pending();
                    if left > 0 {
                        tracing::warn!("{left} lead(s) not delivered before shutdown");
                    }
                    return;
                }
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }
}

fn client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();
    match crate::a2a::push::tls_config() {
        Ok(tls) => builder = builder.use_preconfigured_tls(tls),
        Err(e) => tracing::warn!("lead webhook: https unavailable ({e})"),
    }
    builder
        .redirect(reqwest::redirect::Policy::none())
        .timeout(POST_TIMEOUT)
        .user_agent(concat!("rustybin/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};

    fn lead(email: &str) -> Lead {
        Lead {
            email: email.into(),
            marketing_consent: true,
            consent_mode: "checkbox",
            source: "mock_extend",
            captured_at: Utc::now(),
            visit_minutes: 180,
            mocks: vec!["orders-abc123".into()],
        }
    }

    fn cfg(webhook: Option<String>, file: Option<PathBuf>) -> SelfServiceConfig {
        SelfServiceConfig {
            enabled: true,
            leads_webhook_url: webhook,
            leads_webhook_token: Some("hook-secret".into()),
            leads_file: file,
            privacy_url: Some("https://example.com/privacy".into()),
            ..SelfServiceConfig::default()
        }
    }

    #[tokio::test]
    async fn delivered_to_the_webhook_and_the_file() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, serde_json::Value)>(4);
        let app = Router::new().route(
            "/hook",
            post(
                move |headers: axum::http::HeaderMap, Json(v): Json<serde_json::Value>| {
                    let tx = tx.clone();
                    async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        let _ = tx.send((auth, v)).await;
                        "ok"
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let tmp = tempfile::tempdir().expect("tmp");
        let file = tmp.path().join("leads.jsonl");
        let metrics = Arc::new(Metrics::new());
        let leads = Arc::new(Leads::new(
            &cfg(Some(format!("http://{addr}/hook")), Some(file.clone())),
            metrics.clone(),
        ));
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(leads.clone().run(async move {
            let _ = stop_rx.await;
        }));
        leads.submit(lead("ada@example.com"));
        let (auth, body) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("delivered in time")
            .expect("delivered");
        assert_eq!(auth, "Bearer hook-secret");
        assert_eq!(body["email"], "ada@example.com");
        assert_eq!(body["marketing_consent"], true);
        assert_eq!(body["mocks"][0], "orders-abc123");
        let written = std::fs::read_to_string(&file).expect("file");
        assert_eq!(written.lines().count(), 1);
        assert!(written.contains("ada@example.com"));
        let _ = stop_tx.send(());
        task.await.expect("stopped");
        let text = metrics.render();
        assert!(text.contains("rustybin_mock_leads_total{result=\"captured\"} 1"));
        assert!(text.contains("rustybin_mock_leads_total{result=\"delivered\"} 1"));
    }

    #[tokio::test]
    async fn failures_are_retried_and_the_queue_is_bounded() {
        let metrics = Arc::new(Metrics::new());
        // Nothing listens on port 1.
        let leads = Arc::new(Leads::new(
            &cfg(Some("http://127.0.0.1:1/hook".into()), None),
            metrics.clone(),
        ));
        let client = client().expect("client");
        leads.submit(lead("bob@example.org"));
        for p in leads.due() {
            assert!(leads
                .post(&client, "http://127.0.0.1:1/hook", &p.lead)
                .await
                .is_err());
            leads.requeue(p);
        }
        assert_eq!(leads.pending(), 1);
        assert!(leads.due().is_empty(), "backed off");
        for _ in 0..MAX_QUEUE + 5 {
            leads.submit(lead("flood@example.org"));
        }
        assert_eq!(leads.pending(), MAX_QUEUE);
        let text = metrics.render();
        assert!(text.contains("rustybin_mock_leads_total{result=\"retried\"} 1"));
        assert!(text.contains("rustybin_mock_leads_total{result=\"dropped\"} 6"));
        // Without capture configured nothing is kept.
        let off = Leads::new(&SelfServiceConfig::default(), metrics);
        off.submit(lead("carol@example.org"));
        assert_eq!(off.pending(), 0);
    }
}
