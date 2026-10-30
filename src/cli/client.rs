//! A small client over the control API: the bearer from the profile, the
//! session and manage-token headers, JSON or text bodies, and SSE reading
//! for the tails.

use std::time::Duration;

use reqwest::Method;
use serde_json::Value;

use super::{Fail, Result};

/// What a request carries.
pub enum Body {
    None,
    Json(Value),
    Text(String, &'static str),
}

/// What came back.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub text: String,
    pub json: Option<Value>,
    pub content_type: String,
}

impl Reply {
    /// The answer as a value: the JSON, else the text.
    pub fn value(&self) -> Value {
        self.json
            .clone()
            .unwrap_or_else(|| Value::String(self.text.clone()))
    }

    fn error_message(&self) -> String {
        let from_json = self.json.as_ref().and_then(|j| {
            j.get("error")
                .or_else(|| j.get("detail"))
                .or_else(|| j.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        from_json.unwrap_or_else(|| {
            let short: String = self.text.chars().take(300).collect();
            if short.trim().is_empty() {
                reqwest::StatusCode::from_u16(self.status)
                    .ok()
                    .and_then(|s| s.canonical_reason())
                    .unwrap_or("error")
                    .to_string()
            } else {
                short
            }
        })
    }

    /// `Ok` for a 2xx; the failure otherwise. A 401, or a 403 about the
    /// credential (the admin token, a scope), is exit code 3; a 403 the
    /// instance's policy answers (egress, a visitor's mock) is a refusal.
    pub fn ok(self) -> Result<Reply> {
        let message = self.error_message();
        let about_credential = {
            let m = message.to_ascii_lowercase();
            m.contains("token")
                || m.contains("scope")
                || m.contains("sign in")
                || m.contains("credential")
        };
        match self.status {
            200..=299 => Ok(self),
            401 => Err(Fail::Auth(format!(
                "HTTP 401: {message} (rustybin login, or --token)"
            ))),
            403 if about_credential => Err(Fail::Auth(format!(
                "HTTP 403: {message} (rustybin login, or --token)"
            ))),
            _ => Err(Fail::Failed(format!("HTTP {}: {message}", self.status))),
        }
    }
}

pub struct Client {
    pub base: String,
    pub token: Option<String>,
    pub session: Option<String>,
    pub mock_token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("rustybin-cli/", env!("CARGO_PKG_VERSION")));
        if let Ok(tls) = crate::a2a::push::tls_config() {
            builder = builder.use_preconfigured_tls(tls);
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            session: None,
            mock_token: None,
            http: builder
                .build()
                .map_err(|e| Fail::Network(format!("the HTTP client could not be built: {e}")))?,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Body,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, self.url(path));
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if let Some(s) = &self.session {
            req = req.header("X-Rustybin-Session", s);
        }
        if let Some(m) = &self.mock_token {
            req = req.header("X-Rustybin-Mock-Token", m);
        }
        match body {
            Body::None => req,
            Body::Json(v) => req.json(&v),
            Body::Text(t, ct) => req.header("content-type", ct).body(t),
        }
    }

    /// One call; the reply whatever its status.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Body,
    ) -> Result<Reply> {
        let url = self.url(path);
        let resp = self
            .request(method, path, query, body)
            .send()
            .await
            .map_err(|e| Fail::Network(format!("{url}: {}", network_text(&e))))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp
            .text()
            .await
            .map_err(|e| Fail::Network(format!("{url}: {e}")))?;
        let json = if content_type.contains("json") || text.trim_start().starts_with(['{', '[']) {
            serde_json::from_str(&text).ok()
        } else {
            None
        };
        Ok(Reply {
            status,
            text,
            json,
            content_type,
        })
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Reply> {
        self.send(Method::GET, path, query, Body::None).await?.ok()
    }

    pub async fn post(&self, path: &str, body: Body) -> Result<Reply> {
        self.send(Method::POST, path, &[], body).await?.ok()
    }

    pub async fn put(&self, path: &str, body: Body) -> Result<Reply> {
        self.send(Method::PUT, path, &[], body).await?.ok()
    }

    pub async fn patch(&self, path: &str, body: Body) -> Result<Reply> {
        self.send(Method::PATCH, path, &[], body).await?.ok()
    }

    pub async fn delete(&self, path: &str, query: &[(&str, String)]) -> Result<Reply> {
        self.send(Method::DELETE, path, query, Body::None)
            .await?
            .ok()
    }

    /// The raw bytes of an answer (a bundle).
    pub async fn bytes(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>> {
        let url = self.url(path);
        let resp = self
            .request(Method::GET, path, query, Body::None)
            .send()
            .await
            .map_err(|e| Fail::Network(format!("{url}: {}", network_text(&e))))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Fail::Network(format!("{url}: {e}")))?;
        if !(200..=299).contains(&status) {
            let text = String::from_utf8_lossy(&bytes).to_string();
            return Reply {
                status,
                json: serde_json::from_str(&text).ok(),
                text,
                content_type: String::new(),
            }
            .ok()
            .map(|_| Vec::new());
        }
        Ok(bytes.to_vec())
    }

    /// Read a Server-Sent Events answer: `on(event, data)` per event; stop
    /// after `max` events or `timeout`, or when `on` returns false.
    pub async fn sse(
        &self,
        method: Method,
        path: &str,
        body: Body,
        max: Option<usize>,
        timeout: Duration,
        on: &mut dyn FnMut(&str, &str) -> bool,
    ) -> Result<()> {
        let url = self.url(path);
        let resp = self
            .request(method, path, &[], body)
            .header("accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| Fail::Network(format!("{url}: {}", network_text(&e))))?;
        let status = resp.status().as_u16();
        if !(200..=299).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Reply {
                status,
                json: serde_json::from_str(&text).ok(),
                text,
                content_type: String::new(),
            }
            .ok()
            .map(|_| ());
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut seen = 0usize;
        let mut event = String::new();
        let mut data = String::new();
        use futures_util::StreamExt;
        loop {
            let next = tokio::time::timeout_at(deadline, stream.next()).await;
            let chunk = match next {
                Err(_) => return Ok(()),
                Ok(None) => return Ok(()),
                Ok(Some(Err(e))) => return Err(Fail::Network(format!("{url}: {e}"))),
                Ok(Some(Ok(c))) => c,
            };
            buffer.push_str(&String::from_utf8_lossy(&c_slice(&chunk)));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                if line.is_empty() {
                    if !data.is_empty() || !event.is_empty() {
                        let kind = if event.is_empty() {
                            "message"
                        } else {
                            event.as_str()
                        };
                        let keep = on(kind, data.trim_end_matches('\n'));
                        seen += 1;
                        event.clear();
                        data.clear();
                        if !keep || max.is_some_and(|m| seen >= m) {
                            return Ok(());
                        }
                    }
                } else if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.strip_prefix(' ').unwrap_or(v));
                    data.push('\n');
                }
            }
        }
    }
}

fn c_slice(b: &bytes::Bytes) -> Vec<u8> {
    b.to_vec()
}

fn network_text(e: &reqwest::Error) -> String {
    if e.is_connect() {
        format!("cannot connect ({e})")
    } else if e.is_timeout() {
        "timed out".to_string()
    } else {
        e.to_string()
    }
}
