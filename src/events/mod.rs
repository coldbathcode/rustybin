//! Event mocks: the channels of an AsyncAPI mock, served.
//!
//! A mock made from an AsyncAPI document ([`crate::mocks::asyncapi`]) has
//! channels in place of operations. Each is served at
//! `/events/{mock}/{channel}` (and below the mock's own root, at
//! `/mock/{mock}/_rustybin/events/{channel}`): a WebSocket, server-sent
//! events for `Accept: text/event-stream`, and a `POST` that publishes a
//! message as a client. Rustybin publishes the document's `send` messages
//! on a pattern (`every`, `burst`, `on_receive`, `off`) and validates what
//! clients publish against the `receive` messages. A webhook subscriber is
//! a URL the messages are posted to, through the outbound policy under
//! [`Purpose::Events`], which has no default.
//!
//! Every subscriber of a channel gets the same messages: one hub per
//! channel, one publishing task per channel with subscribers, stopped with
//! the last of them or with the mock. Messages are kept in the mock's
//! capture log ([`crate::mocks::capture::EventRecord`]), so they are
//! bounded and read like its requests.

mod serve;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::routing::{any, delete, get, post};
use axum::Router;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::ai::realtime::{Sessions, Slot};
use crate::catalog::{category, Endpoint, Example};
use crate::egress::Purpose;
use crate::mocks::asyncapi::{self, Channel};
use crate::mocks::capture::{CaptureLog, Delivered, EventRecord, Sender, MAX_EVENT_PREVIEW};
use crate::mocks::compile::Compiled;
use crate::mocks::model::{ChannelPattern, ChannelSettings};
use crate::mocks::schema::Issue;
use crate::mocks::service::{ServiceError, Svc};
use crate::mocks::store::Lookup;
use crate::rbac::Permission;
use crate::state::AppState;

pub const MAX_CONNECTIONS_KEY: &str = "asyncapi.max_connections";
pub const SESSION_MINUTES_KEY: &str = "asyncapi.session_minutes";
pub const MAX_SUBSCRIBERS_KEY: &str = "asyncapi.max_subscribers";
pub const DEFAULT_MAX_CONNECTIONS: usize = 128;
pub const DEFAULT_SESSION_MINUTES: usize = 60;
pub const DEFAULT_MAX_SUBSCRIBERS: usize = 50;

/// Most channels with a hub at once; one more is refused.
const MAX_HUBS: usize = 1024;
/// Most webhook subscribers of one mock.
pub const MAX_SUBSCRIBERS_PER_MOCK: usize = 10;
/// How long a webhook subscriber lives by default, and at most.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);
const MAX_TTL: Duration = Duration::from_secs(24 * 3600);
/// A subscriber that failed this often in a row is removed.
const MAX_FAILURES: u32 = 10;
/// Webhook deliveries under way at once; one more is counted as failed.
const MAX_DELIVERIES: usize = 16;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a channel's task looks at its mock and its subscribers.
const TICK: Duration = Duration::from_secs(1);
/// The largest message a client may publish over a WebSocket.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// The caps of the event mocks as instance settings.
pub fn specs() -> Vec<crate::settings::Spec> {
    use crate::settings::{Kind, Spec};
    let int = |key: &'static str,
               title: &'static str,
               help: &'static str,
               min: i64,
               max: i64,
               default: usize| {
        Spec::new(
            key,
            "asyncapi",
            title,
            help,
            Kind::Integer { min, max },
            json!(default),
        )
    };
    vec![
        int(
            MAX_CONNECTIONS_KEY,
            "Event connections at once",
            "How many WebSocket and server-sent event connections to the channels of AsyncAPI mocks (/events/{mock}/{channel}) may be open on the instance; one more is refused with 429.",
            1,
            1024,
            DEFAULT_MAX_CONNECTIONS,
        ),
        int(
            SESSION_MINUTES_KEY,
            "Longest event connection (minutes)",
            "A connection to a channel is closed after this long.",
            1,
            720,
            DEFAULT_SESSION_MINUTES,
        ),
        int(
            MAX_SUBSCRIBERS_KEY,
            "Webhook subscribers",
            "How many webhook subscribers the instance keeps for the channels of AsyncAPI mocks (at most 10 a mock).",
            0,
            500,
            DEFAULT_MAX_SUBSCRIBERS,
        ),
    ]
}

/// What a channel's hub sends its subscribers.
#[derive(Debug)]
pub enum Frame {
    Message {
        /// The message's number on the channel, from 1.
        id: u64,
        name: String,
        /// The payload as sent (a malformed one is cut short).
        text: String,
        headers: Vec<(String, String)>,
    },
    /// The mock was deleted, turned off or lost the channel.
    Closed,
}

/// One channel's hub.
struct Hub {
    tx: broadcast::Sender<Arc<Frame>>,
    /// Messages published on the channel.
    seq: AtomicU64,
    ws: AtomicUsize,
    sse: AtomicUsize,
    /// The publishing task is running.
    running: AtomicBool,
    /// The log of the mock the hub was made for: a mock deleted and made
    /// again under the same name gets a hub of its own.
    log: CaptureLog,
}

/// How a subscriber listens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ws,
    Sse,
}

/// Held by a connection while it listens to a channel.
pub struct Listener {
    pub rx: broadcast::Receiver<Arc<Frame>>,
    hub: Arc<Hub>,
    kind: Kind,
    _slot: Slot,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let n = match self.kind {
            Kind::Ws => &self.hub.ws,
            Kind::Sse => &self.hub.sse,
        };
        n.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A URL a channel's messages are posted to.
struct Subscriber {
    id: String,
    mock: String,
    channel: String,
    url: String,
    created_at: String,
    expires: Instant,
    expires_at: String,
    delivered: u64,
    failed: u64,
    /// Failures in a row.
    failing: u32,
    last_status: Option<u16>,
    last_error: Option<&'static str>,
}

impl Subscriber {
    fn json(&self) -> Value {
        // A query may hold the receiver's credential: never answered.
        let (shown, query) = match self.url.split_once('?') {
            Some((base, _)) => (base, true),
            None => (self.url.as_str(), false),
        };
        json!({
            "id": self.id,
            "channel": self.channel,
            "url": shown,
            "query_hidden": query,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "delivered": self.delivered,
            "failed": self.failed,
            "last_status": self.last_status,
            "last_error": self.last_error,
        })
    }
}

#[derive(Default)]
struct Inner {
    hubs: HashMap<(String, String), Arc<Hub>>,
    subscribers: Vec<Subscriber>,
}

/// The event mocks' shared state (`AppState.events`).
pub struct Events {
    settings: Arc<crate::settings::Settings>,
    inner: Mutex<Inner>,
    connections: Arc<Sessions>,
    deliveries: Arc<tokio::sync::Semaphore>,
}

fn now_text() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A deterministic draw for a fault: is message `n` one of `percent` in a
/// hundred?
fn drawn(seed: u64, salt: u64, n: u64, percent: u8) -> bool {
    if percent == 0 {
        return false;
    }
    let h =
        (seed ^ salt ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    ((h >> 33) % 100) < u64::from(percent)
}

/// A channel's settings and the pattern it publishes on: the one set,
/// else `on_receive` when the document declares a reply, `off` when the
/// channel publishes nothing, `every` otherwise.
pub fn settings_of(c: &Compiled, ch: &Channel) -> (ChannelPattern, ChannelSettings) {
    let cfg = c.settings.channels.get(&ch.id).cloned().unwrap_or_default();
    let pattern = cfg.pattern.unwrap_or(if ch.send.is_empty() {
        ChannelPattern::Off
    } else if ch.reply {
        ChannelPattern::OnReceive
    } else {
        ChannelPattern::Every
    });
    (pattern, cfg)
}

/// A live, enabled AsyncAPI mock and its capture log.
fn live_mock(state: &AppState, mock: &str) -> Option<(Arc<Compiled>, CaptureLog)> {
    match state.mocks.lookup(mock) {
        Lookup::Ready(live) if live.enabled && live.compiled.spec.events.is_some() => {
            Some((live.compiled, live.capture))
        }
        _ => None,
    }
}

fn channel_of<'a>(c: &'a Compiled, id: &str) -> Option<&'a Channel> {
    c.spec.events.as_deref()?.channel(id)
}

/// A payload for a record: the JSON when small, else its text, cut.
fn preview(payload: Option<&Value>, text: &str) -> (Option<Value>, Option<String>, bool) {
    if text.len() > MAX_EVENT_PREVIEW {
        let mut end = MAX_EVENT_PREVIEW;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        return (None, Some(text[..end].to_string()), true);
    }
    match payload {
        Some(v) => (Some(v.clone()), None, false),
        None => (None, Some(text.to_string()), false),
    }
}

/// What publishing a message was asked to do.
pub struct Publish {
    /// The payload to publish in place of the channel's next one.
    pub given: Option<Value>,
    /// Apply the channel's faults (the pattern's own messages do; one a
    /// person publishes does not).
    pub faults: bool,
}

impl Events {
    pub fn new(settings: Arc<crate::settings::Settings>) -> Self {
        Self {
            settings,
            inner: Mutex::default(),
            connections: Arc::default(),
            deliveries: Arc::new(tokio::sync::Semaphore::new(MAX_DELIVERIES)),
        }
    }

    /// The state, without the subscribers that expired.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        inner.subscribers.retain(|s| s.expires > now);
        inner
    }

    fn setting(&self, key: &str, default: usize) -> usize {
        self.settings
            .get(key)
            .0
            .as_u64()
            .map_or(default, |v| v as usize)
    }

    /// Connections open on the instance.
    pub fn open_connections(&self) -> usize {
        self.connections.open()
    }

    /// The channel's hub, made on first use. `None` when [`MAX_HUBS`]
    /// channels have one and none is idle.
    fn hub(&self, mock: &str, channel: &str, log: &CaptureLog) -> Option<Arc<Hub>> {
        let mut inner = self.lock();
        let key = (mock.to_string(), channel.to_string());
        if let Some(h) = inner.hubs.get(&key) {
            if h.log.same(log) {
                return Some(h.clone());
            }
            // The mock this hub served is gone: its listeners are let go.
            let _ = h.tx.send(Arc::new(Frame::Closed));
            inner.hubs.remove(&key);
        }
        if inner.hubs.len() >= MAX_HUBS {
            inner.hubs.retain(|_, h| h.running.load(Ordering::Acquire));
            if inner.hubs.len() >= MAX_HUBS {
                return None;
            }
        }
        let (tx, _) = broadcast::channel(256);
        let hub = Arc::new(Hub {
            tx,
            seq: AtomicU64::new(0),
            ws: AtomicUsize::new(0),
            sse: AtomicUsize::new(0),
            running: AtomicBool::new(false),
            log: log.clone(),
        });
        inner.hubs.insert(key, hub.clone());
        Some(hub)
    }

    fn webhooks_of(inner: &Inner, mock: &str, channel: &str) -> Vec<(String, String)> {
        inner
            .subscribers
            .iter()
            .filter(|s| s.mock == mock && s.channel == channel)
            .map(|s| (s.id.clone(), s.url.clone()))
            .collect()
    }

    /// Publish the channel's next message (or `given`) to every subscriber
    /// and keep it in the mock's log. `Err`: the channel publishes nothing
    /// and nothing was given, or no hub is free.
    pub fn publish(
        self: &Arc<Self>,
        state: &AppState,
        mock: &str,
        c: &Compiled,
        log: &CaptureLog,
        ch: &Channel,
        what: Publish,
    ) -> Result<Arc<EventRecord>, &'static str> {
        let hub = self
            .hub(mock, &ch.id, log)
            .ok_or("too many channels are in use on this instance: try again later")?;
        let (_, cfg) = settings_of(c, ch);
        let seed = c.settings.responses.seed;
        let out = match what.given {
            Some(payload) => asyncapi::Outgoing {
                message: ch
                    .send
                    .first()
                    .map_or_else(|| "message".to_string(), |m| m.name.clone()),
                payload,
                source: "given",
                headers: Vec::new(),
            },
            None => {
                if ch.send.is_empty() {
                    return Err("the channel publishes no messages (the document has no operation that sends on it): give a payload");
                }
                let n = hub.seq.load(Ordering::Acquire) + 1;
                asyncapi::outgoing(&c.spec.root, ch, &cfg.examples, seed, n)
                    .ok_or("the channel publishes no messages")?
            }
        };
        let n = hub.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let mut text = out.payload.to_string();
        let dropped = what.faults && drawn(seed, 0xd409, n, cfg.drop_percent);
        let malformed = what.faults && !dropped && drawn(seed, 0x3a1f, n, cfg.malformed_percent);
        if malformed {
            let mut end = (text.len() / 2).max(1).min(text.len().saturating_sub(1));
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        let webhooks = if dropped {
            Vec::new()
        } else {
            Self::webhooks_of(&self.lock(), mock, &ch.id)
        };
        let delivered = if dropped {
            Delivered::default()
        } else {
            Delivered {
                ws: hub.ws.load(Ordering::Acquire),
                sse: hub.sse.load(Ordering::Acquire),
                webhook: webhooks.len(),
            }
        };
        let (payload, cut, truncated) = preview((!malformed).then_some(&out.payload), &text);
        let record = log.record_event(EventRecord {
            seq: 0,
            timestamp: now_text(),
            channel: ch.id.clone(),
            direction: "sent",
            transport: None,
            message: Some(out.message.clone()),
            payload,
            text: cut,
            size: text.len(),
            truncated,
            valid: !malformed,
            issues: Vec::new(),
            source: Some(out.source),
            fault: if dropped {
                Some("dropped")
            } else if malformed {
                Some("malformed")
            } else {
                None
            },
            delivered: Some(delivered),
            revision: c.revision,
            sender: None,
        });
        if dropped {
            return Ok(record);
        }
        let frame = Arc::new(Frame::Message {
            id: n,
            name: out.message,
            text,
            headers: out.headers,
        });
        let _ = hub.tx.send(frame.clone());
        for (id, url) in webhooks {
            self.deliver(state, mock, &ch.id, id, url, frame.clone());
        }
        Ok(record)
    }

    /// Check a message a client published, keep it, and answer it when the
    /// channel's pattern is `on_receive`.
    #[allow(clippy::too_many_arguments)]
    pub async fn receive(
        self: &Arc<Self>,
        state: &AppState,
        mock: &str,
        c: &Compiled,
        log: &CaptureLog,
        ch: &Channel,
        text: &str,
        transport: &'static str,
        sender: Option<Sender>,
    ) -> (asyncapi::Verdict, Option<Arc<EventRecord>>) {
        let parsed: Result<Value, _> = serde_json::from_str(text);
        let verdict = match &parsed {
            Ok(v) => asyncapi::check_incoming(&c.spec.root, ch, v),
            Err(e) => asyncapi::Verdict {
                message: None,
                issues: vec![Issue::new("", format!("the message is not JSON: {e}"))],
            },
        };
        let (payload, cut, truncated) = preview(parsed.as_ref().ok(), text);
        log.record_event(EventRecord {
            seq: 0,
            timestamp: now_text(),
            channel: ch.id.clone(),
            direction: "received",
            transport: Some(transport),
            message: verdict.message.clone(),
            payload,
            text: cut,
            size: text.len(),
            truncated,
            valid: verdict.valid(),
            issues: verdict.issues.clone(),
            source: None,
            fault: None,
            delivered: None,
            revision: c.revision,
            sender,
        });
        let (pattern, cfg) = settings_of(c, ch);
        let mut reply = None;
        if verdict.valid() && pattern == ChannelPattern::OnReceive {
            if cfg.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(cfg.delay_ms)).await;
            }
            let what = Publish {
                given: None,
                faults: true,
            };
            reply = self.publish(state, mock, c, log, ch, what).ok();
        }
        (verdict, reply)
    }

    /// Start listening to a channel: a connection slot, the hub, and its
    /// publishing task if it is not running.
    pub fn listen(
        self: &Arc<Self>,
        state: &AppState,
        mock: &str,
        channel: &str,
        log: &CaptureLog,
        kind: Kind,
    ) -> Result<Listener, (StatusCode, String)> {
        let max = self.setting(MAX_CONNECTIONS_KEY, DEFAULT_MAX_CONNECTIONS);
        let slot = self.connections.take(max).ok_or_else(|| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                format!("too many event connections are open on this instance (at most {max}): close one and try again"),
            )
        })?;
        let hub = self.hub(mock, channel, log).ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "too many channels are in use on this instance: try again later".to_string(),
            )
        })?;
        let rx = hub.tx.subscribe();
        {
            // Counted under the lock the task stops under, so a listener
            // that arrives as the task ends starts the next one.
            let _inner = self.lock();
            match kind {
                Kind::Ws => &hub.ws,
                Kind::Sse => &hub.sse,
            }
            .fetch_add(1, Ordering::AcqRel);
        }
        self.ensure_task(state, mock, channel, &hub);
        Ok(Listener {
            rx,
            hub,
            kind,
            _slot: slot,
        })
    }

    /// How long a connection may stay open.
    pub fn session(&self) -> Duration {
        Duration::from_secs(self.setting(SESSION_MINUTES_KEY, DEFAULT_SESSION_MINUTES) as u64 * 60)
    }

    /// The channel's one publishing task: while somebody listens (a
    /// connection or a webhook subscriber) it publishes on the pattern and
    /// watches the mock; it ends with the last listener, and closes the hub
    /// when the mock is deleted, turned off or loses the channel.
    fn ensure_task(self: &Arc<Self>, state: &AppState, mock: &str, channel: &str, hub: &Arc<Hub>) {
        if hub.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let events = self.clone();
        let state = state.clone();
        let hub = hub.clone();
        let (mock, channel) = (mock.to_string(), channel.to_string());
        tokio::spawn(async move {
            let mut due = Instant::now();
            let mut first = true;
            loop {
                let Some((c, log)) = live_mock(&state, &mock) else {
                    events.close(&mock, &channel, &hub);
                    return;
                };
                let Some(ch) = channel_of(&c, &channel) else {
                    events.close(&mock, &channel, &hub);
                    return;
                };
                if !hub.log.same(&log) {
                    // Another mock has the name now, with a hub of its own.
                    let _ = hub.tx.send(Arc::new(Frame::Closed));
                    hub.running.store(false, Ordering::Release);
                    return;
                }
                if events.idle(&mock, &channel, &hub) {
                    return;
                }
                let (pattern, cfg) = settings_of(&c, ch);
                let every = Duration::from_millis(cfg.every_ms);
                if first {
                    due = Instant::now() + every;
                    first = false;
                }
                let timed = matches!(pattern, ChannelPattern::Every | ChannelPattern::Burst);
                if timed && Instant::now() >= due {
                    due += every;
                    if due < Instant::now() {
                        due = Instant::now() + every;
                    }
                    if cfg.delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(cfg.delay_ms)).await;
                    }
                    let count = if pattern == ChannelPattern::Burst {
                        cfg.burst
                    } else {
                        1
                    };
                    for _ in 0..count {
                        let what = Publish {
                            given: None,
                            faults: true,
                        };
                        let _ = events.publish(&state, &mock, &c, &log, ch, what);
                    }
                }
                let wait = if timed {
                    due.saturating_duration_since(Instant::now()).min(TICK)
                } else {
                    TICK
                };
                tokio::time::sleep(wait.max(Duration::from_millis(10))).await;
            }
        });
    }

    /// Nobody listens: the task ends (decided under the lock a listener is
    /// counted under).
    fn idle(&self, mock: &str, channel: &str, hub: &Hub) -> bool {
        let inner = self.lock();
        let listening = hub.ws.load(Ordering::Acquire) + hub.sse.load(Ordering::Acquire) > 0
            || inner
                .subscribers
                .iter()
                .any(|s| s.mock == mock && s.channel == channel);
        if !listening {
            hub.running.store(false, Ordering::Release);
        }
        !listening
    }

    /// The channel is gone: its listeners are told, its hub and its webhook
    /// subscribers forgotten.
    fn close(&self, mock: &str, channel: &str, hub: &Hub) {
        let mut inner = self.lock();
        let _ = hub.tx.send(Arc::new(Frame::Closed));
        hub.running.store(false, Ordering::Release);
        let key = (mock.to_string(), channel.to_string());
        if inner.hubs.get(&key).is_some_and(|h| h.log.same(&hub.log)) {
            inner.hubs.remove(&key);
        }
        inner
            .subscribers
            .retain(|s| !(s.mock == mock && s.channel == channel));
    }

    /// Post one message to a webhook subscriber, through the outbound
    /// policy, without holding up the channel.
    fn deliver(
        self: &Arc<Self>,
        state: &AppState,
        mock: &str,
        channel: &str,
        id: String,
        url: String,
        frame: Arc<Frame>,
    ) {
        let Ok(permit) = self.deliveries.clone().try_acquire_owned() else {
            self.delivered(&id, Err("busy"));
            return;
        };
        let events = self.clone();
        let egress = state.egress.clone();
        let (mock, channel) = (mock.to_string(), channel.to_string());
        tokio::spawn(async move {
            let _permit = permit;
            let Frame::Message {
                id: n,
                name,
                text,
                headers,
            } = &*frame
            else {
                return;
            };
            let pinned = match egress.resolve(&url, Purpose::Events).await {
                Ok(p) => p,
                Err(_) => return events.delivered(&id, Err("egress_refused")),
            };
            let Ok(client) = egress.client(&pinned, DELIVERY_TIMEOUT) else {
                return events.delivered(&id, Err("client"));
            };
            let mut req = client
                .post(&pinned.url)
                .header("content-type", "application/json")
                .header("x-rustybin-mock", &mock)
                .header("x-rustybin-channel", &channel)
                .header("x-rustybin-event", name)
                .header("x-rustybin-event-id", n.to_string())
                .body(text.clone());
            for (k, v) in headers {
                let lower = k.to_ascii_lowercase();
                let reserved = matches!(
                    lower.as_str(),
                    "host"
                        | "content-length"
                        | "content-type"
                        | "transfer-encoding"
                        | "connection"
                        | "upgrade"
                ) || lower.starts_with("x-rustybin-");
                if !reserved {
                    req = req.header(k, v);
                }
            }
            match req.send().await {
                Ok(resp) => events.delivered(&id, Ok(resp.status().as_u16())),
                Err(e) if e.is_timeout() => events.delivered(&id, Err("timeout")),
                Err(_) => events.delivered(&id, Err("unreachable")),
            }
        });
    }

    /// Note a delivery's outcome; a subscriber that keeps failing goes.
    fn delivered(&self, id: &str, outcome: Result<u16, &'static str>) {
        let mut inner = self.lock();
        let Some(s) = inner.subscribers.iter_mut().find(|s| s.id == id) else {
            return;
        };
        match outcome {
            Ok(status) if (200..300).contains(&status) => {
                s.delivered += 1;
                s.failing = 0;
                s.last_status = Some(status);
                s.last_error = None;
            }
            Ok(status) => {
                s.failed += 1;
                s.failing += 1;
                s.last_status = Some(status);
                s.last_error = Some("status");
            }
            Err(e) => {
                s.failed += 1;
                s.failing += 1;
                s.last_error = Some(e);
            }
        }
        if s.failing >= MAX_FAILURES {
            inner.subscribers.retain(|s| s.id != id);
        }
    }

    /// A channel's live numbers.
    fn status(&self, mock: &str, channel: &str) -> Value {
        let inner = self.lock();
        let hub = inner.hubs.get(&(mock.to_string(), channel.to_string()));
        json!({
            "published": hub.map_or(0, |h| h.seq.load(Ordering::Acquire)),
            "ws": hub.map_or(0, |h| h.ws.load(Ordering::Acquire)),
            "sse": hub.map_or(0, |h| h.sse.load(Ordering::Acquire)),
            "webhooks": inner.subscribers.iter().filter(|s| s.mock == mock && s.channel == channel).count(),
        })
    }
}

// ── The service the routes and the tools share ──────────────────────

type SvcResult<T> = Result<T, ServiceError>;

fn not_asyncapi(name: &str) -> ServiceError {
    ServiceError::with_body(
        StatusCode::CONFLICT,
        json!({
            "error": format!("mock {name:?} is not an AsyncAPI mock: it has operations, not channels"),
            "reason": "not_asyncapi",
        }),
    )
}

fn no_channel(name: &str, channel: &str, c: &Compiled) -> ServiceError {
    let known: Vec<&str> = c
        .spec
        .events
        .as_deref()
        .map(|e| e.channels.iter().map(|c| c.id.as_str()).collect())
        .unwrap_or_default();
    ServiceError::with_body(
        StatusCode::NOT_FOUND,
        json!({
            "error": format!("mock {name:?} has no channel {channel:?}"),
            "channels": known,
        }),
    )
}

/// The data plane path of a channel.
pub fn channel_path(mock: &str, channel: &str) -> String {
    format!("/events/{mock}/{channel}")
}

/// `GET /_rustybin/mocks/{name}/channels`: the draft's channels, each with
/// its settings, a valid message to publish and its live numbers.
pub fn channels(svc: &Svc, name: &str) -> SvcResult<Value> {
    let s = svc.readable(name)?;
    let c = &s.draft;
    let events = c.spec.events.as_deref().ok_or_else(|| not_asyncapi(name))?;
    let base = svc.links.base_url();
    let seed = c.settings.responses.seed;
    let list: Vec<Value> = events
        .channels
        .iter()
        .map(|ch| {
            let (pattern, cfg) = settings_of(c, ch);
            let mut v = asyncapi::channel_json(ch);
            if let Value::Object(m) = &mut v {
                let path = channel_path(name, &ch.id);
                m.insert("url".into(), json!(format!("{base}{path}")));
                m.insert("path".into(), json!(path));
                m.insert("pattern".into(), json!(pattern.as_str()));
                m.insert(
                    "settings".into(),
                    serde_json::to_value(&cfg).unwrap_or(Value::Null),
                );
                if let Some((message, payload)) = asyncapi::sample_incoming(&c.spec.root, ch, seed)
                {
                    m.insert(
                        "sample".into(),
                        json!({ "message": message, "payload": payload }),
                    );
                }
                m.insert("live".into(), svc.state.events.status(name, &ch.id));
            }
            v
        })
        .collect();
    Ok(json!({
        "mock": name,
        "asyncapi": events.asyncapi,
        "state": s.state(),
        "count": list.len(),
        "channels": list,
    }))
}

/// `GET /_rustybin/mocks/{name}/channels/{channel}/messages`: what was
/// published and received, newest first. A caller who reads only their own
/// traffic sees what Rustybin published and what they published.
pub fn messages(
    svc: &Svc,
    name: &str,
    channel: &str,
    limit: Option<usize>,
    after: Option<u64>,
) -> SvcResult<Value> {
    let s = svc.readable(name)?;
    let c = &s.draft;
    if c.spec.events.is_none() {
        return Err(not_asyncapi(name));
    }
    let live = s.live.as_ref().unwrap_or(c);
    if channel_of(c, channel).is_none() && channel_of(live, channel).is_none() {
        return Err(no_channel(name, channel, c));
    }
    let limit = limit.unwrap_or(50).clamp(1, 500);
    let scope = svc.traffic_scope(&s);
    let (list, total) = s
        .capture
        .events(&scope, Some(channel), after.unwrap_or(0), limit);
    Ok(json!({
        "mock": name,
        "channel": channel,
        "total": total,
        "count": list.len(),
        "last": list.first().map_or(after.unwrap_or(0), |r| r.seq),
        "messages": list,
    }))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishBody {
    /// The payload to publish in place of the channel's next message.
    #[serde(default)]
    pub payload: Option<Value>,
    /// How many of the channel's next messages to publish (1 to 20).
    #[serde(default)]
    pub count: Option<u32>,
}

/// `POST /_rustybin/mocks/{name}/channels/{channel}/publish`: Rustybin
/// publishes the channel's next message (or the payload given) to every
/// subscriber now.
pub fn publish(svc: &Svc, name: &str, channel: &str, body: PublishBody) -> SvcResult<Value> {
    svc.manageable(name)?;
    let Some((c, log)) = live_mock(&svc.state, name) else {
        return Err(match svc.state.mocks.lookup(name) {
            Lookup::Ready(live) if live.compiled.spec.events.is_none() => not_asyncapi(name),
            _ => ServiceError::new(
                StatusCode::CONFLICT,
                format!("mock {name:?} is not live: deploy and enable it to publish"),
            ),
        });
    };
    let ch = channel_of(&c, channel).ok_or_else(|| no_channel(name, channel, &c))?;
    let count = body.count.unwrap_or(1);
    if !(1..=crate::mocks::model::MAX_BURST).contains(&count) {
        return Err(ServiceError::bad_request(format!(
            "count must be between 1 and {}",
            crate::mocks::model::MAX_BURST
        )));
    }
    if body.payload.is_some() && count != 1 {
        return Err(ServiceError::bad_request(
            "a payload is published once: leave count out",
        ));
    }
    if let Some(p) = &body.payload {
        if p.to_string().len() > MAX_MESSAGE_BYTES {
            return Err(ServiceError::bad_request(format!(
                "the payload is larger than {MAX_MESSAGE_BYTES} bytes"
            )));
        }
    }
    let mut published = Vec::new();
    let mut given = body.payload;
    for _ in 0..count {
        let what = Publish {
            given: given.take(),
            faults: false,
        };
        let record = svc
            .state
            .events
            .publish(&svc.state, name, &c, &log, ch, what)
            .map_err(|e| ServiceError::new(StatusCode::CONFLICT, e))?;
        published.push(record);
    }
    crate::audit::target("mock", name);
    Ok(json!({
        "mock": name,
        "channel": channel,
        "published": published.len(),
        "messages": published,
    }))
}

/// `GET /_rustybin/mocks/{name}/subscribers`.
pub fn subscribers(svc: &Svc, name: &str) -> SvcResult<Value> {
    svc.readable(name)?;
    let inner = svc.state.events.lock();
    let list: Vec<Value> = inner
        .subscribers
        .iter()
        .filter(|s| s.mock == name)
        .map(Subscriber::json)
        .collect();
    Ok(json!({ "mock": name, "count": list.len(), "subscribers": list }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeBody {
    pub channel: String,
    /// Where the channel's messages are posted.
    pub url: String,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

/// `POST /_rustybin/mocks/{name}/subscribers`: a webhook subscriber. The
/// URL's host must be allowed for the outbound purpose `events`; a refused
/// one is answered before anything is sent. Never on a visitor's mock.
pub fn subscribe(svc: &Svc, name: &str, body: SubscribeBody) -> SvcResult<Value> {
    let s = svc.manageable(name)?;
    if s.owner.is_visitor() {
        return Err(ServiceError::forbidden(
            "webhook subscribers are not available for self-service mocks",
        ));
    }
    let Some((c, log)) = live_mock(&svc.state, name) else {
        return Err(if s.draft.spec.events.is_none() {
            not_asyncapi(name)
        } else {
            ServiceError::new(
                StatusCode::CONFLICT,
                format!("mock {name:?} is not live: deploy and enable it first"),
            )
        });
    };
    if channel_of(&c, &body.channel).is_none() {
        return Err(no_channel(name, &body.channel, &c));
    }
    let url = body.url.trim().to_string();
    crate::egress::parse_url(&url, true)
        .map_err(|e| ServiceError::bad_request(format!("url: {e}")))?;
    let check = svc.state.egress.dry_check(&url, Purpose::Events);
    if check["allowed"] != json!(true) {
        return Err(ServiceError::with_body(
            StatusCode::FORBIDDEN,
            json!({
                "error": "the subscriber's host is not allowed for the outbound purpose events: add it to RUSTYBIN_EGRESS_ALLOW",
                "reason": "egress_refused",
                "purpose": "events",
                "reasons": check["reasons"],
            }),
        ));
    }
    let ttl = match body.ttl_seconds {
        None => DEFAULT_TTL,
        Some(t) if (1..=MAX_TTL.as_secs()).contains(&t) => Duration::from_secs(t),
        Some(_) => {
            return Err(ServiceError::bad_request(format!(
                "ttl_seconds must be between 1 and {}",
                MAX_TTL.as_secs()
            )))
        }
    };
    let events = &svc.state.events;
    let max = events.setting(MAX_SUBSCRIBERS_KEY, DEFAULT_MAX_SUBSCRIBERS);
    let now = chrono::Utc::now();
    let sub = Subscriber {
        id: random_hex(8),
        mock: name.to_string(),
        channel: body.channel.clone(),
        url,
        created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires: Instant::now() + ttl,
        expires_at: (now + chrono::TimeDelta::from_std(ttl).unwrap_or_default())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        delivered: 0,
        failed: 0,
        failing: 0,
        last_status: None,
        last_error: None,
    };
    let answer = sub.json();
    {
        let mut inner = events.lock();
        if inner.subscribers.len() >= max {
            return Err(ServiceError::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("the instance keeps at most {max} webhook subscribers ({MAX_SUBSCRIBERS_KEY}): remove one"),
            ));
        }
        if inner.subscribers.iter().filter(|s| s.mock == name).count() >= MAX_SUBSCRIBERS_PER_MOCK {
            return Err(ServiceError::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "a mock has at most {MAX_SUBSCRIBERS_PER_MOCK} webhook subscribers: remove one"
                ),
            ));
        }
        inner.subscribers.push(sub);
    }
    if let Some(hub) = events.hub(name, &body.channel, &log) {
        events.ensure_task(&svc.state, name, &body.channel, &hub);
    }
    crate::audit::target("mock", name);
    Ok(answer)
}

/// `DELETE /_rustybin/mocks/{name}/subscribers/{id}`.
pub fn unsubscribe(svc: &Svc, name: &str, id: &str) -> SvcResult<Value> {
    svc.manageable(name)?;
    let mut inner = svc.state.events.lock();
    let before = inner.subscribers.len();
    inner
        .subscribers
        .retain(|s| !(s.mock == name && s.id == id));
    if inner.subscribers.len() == before {
        return Err(ServiceError::not_found(format!(
            "mock {name:?} has no subscriber {id:?} (removed, expired, or dropped after {MAX_FAILURES} failed deliveries)"
        )));
    }
    crate::audit::target("mock", name);
    Ok(json!({ "removed": id, "mock": name }))
}

// ── Routes, catalogue, OpenAPI ──────────────────────────────────────

pub const CHANNELS_PATH: &str = "/_rustybin/mocks/{name}/channels";
pub const MESSAGES_PATH: &str = "/_rustybin/mocks/{name}/channels/{channel}/messages";
pub const PUBLISH_PATH: &str = "/_rustybin/mocks/{name}/channels/{channel}/publish";
pub const SUBSCRIBERS_PATH: &str = "/_rustybin/mocks/{name}/subscribers";
pub const SUBSCRIBER_PATH: &str = "/_rustybin/mocks/{name}/subscribers/{id}";
pub const EVENTS_PATH: &str = "/events/{name}/{channel}";
pub const MOCK_EVENTS_PATH: &str = "/mock/{name}/_rustybin/events/{channel}";

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(EVENTS_PATH, any(serve::channel))
        .route(MOCK_EVENTS_PATH, any(serve::channel))
        .route(CHANNELS_PATH, get(serve::channels))
        .route(MESSAGES_PATH, get(serve::messages))
        .route(PUBLISH_PATH, post(serve::publish))
        .route(
            SUBSCRIBERS_PATH,
            get(serve::subscribers).post(serve::subscribe),
        )
        .route(SUBSCRIBER_PATH, delete(serve::unsubscribe))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            EVENTS_PATH,
            &["ANY"],
            category::MOCKS,
            "A channel of an AsyncAPI mock: a WebSocket, server-sent events (Accept: text/event-stream), or POST a message as a client",
        )
        .description(
            "Rustybin publishes the document's send messages on the channel's pattern (every, burst, on_receive, off) and \
             validates what a client publishes against its receive messages: an invalid one is answered with an error event \
             (WebSocket) or 422 (POST) and kept with its validation report. A plain GET describes the channel.",
        )
        .websocket()
        .example(Example::get("A channel of a mock that does not exist", "/events/missing/orders").expect_status(404)),
        Endpoint::new(
            MOCK_EVENTS_PATH,
            &["ANY"],
            category::MOCKS,
            "The same channel below the mock's own root (a self-service mock answers on its own domain)",
        )
        .websocket()
        .example(Example::get("A channel of a mock that does not exist", "/mock/missing/_rustybin/events/orders").expect_status(404)),
        Endpoint::new(
            CHANNELS_PATH,
            &["GET"],
            category::MOCKS,
            "An AsyncAPI mock's channels: messages, pattern, settings, a valid message to publish and the subscribers connected",
        )
        .permission(Permission::MocksRead)
        .self_service()
        .project_scoped()
        .example(Example::get("Channels of a mock that does not exist", "/_rustybin/mocks/missing/channels").expect_status(404)),
        Endpoint::new(
            MESSAGES_PATH,
            &["GET"],
            category::MOCKS,
            "The messages published and received on a channel, newest first (?limit=, ?after= a sequence number)",
        )
        .permission(Permission::TrafficRead)
        .self_service()
        .project_scoped()
        .example(Example::get("Messages of a mock that does not exist", "/_rustybin/mocks/missing/channels/orders/messages").expect_status(404)),
        Endpoint::new(
            PUBLISH_PATH,
            &["POST"],
            category::MOCKS,
            "Publish the channel's next message (or a payload given) to every subscriber now",
        )
        .permission(Permission::MocksData)
        .self_service()
        .project_scoped()
        .example(
            Example::post("Publish on a mock that does not exist", "/_rustybin/mocks/missing/channels/orders/publish")
                .json("{}")
                .expect_status(404),
        ),
        Endpoint::new(
            SUBSCRIBERS_PATH,
            &["GET", "POST"],
            category::MOCKS,
            "Webhook subscribers of an AsyncAPI mock: POST registers a URL a channel's messages are posted to (egress purpose events, no default)",
        )
        .permission_for(&["GET"], Permission::MocksRead)
        .permission_for(&["POST"], Permission::MocksWrite)
        .project_scoped()
        .example(Example::get("Subscribers of a mock that does not exist", "/_rustybin/mocks/missing/subscribers").expect_status(404)),
        Endpoint::new(
            SUBSCRIBER_PATH,
            &["DELETE"],
            category::MOCKS,
            "Remove a webhook subscriber",
        )
        .permission(Permission::MocksWrite)
        .project_scoped()
        .example(Example::delete("A subscriber of a mock that does not exist", "/_rustybin/mocks/missing/subscribers/abc").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let tags = json!(["Mocks"]);
    let name = json!({ "name": "name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Mock name" });
    let channel = json!({ "name": "channel", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Channel id" });
    let not_found = json!({ "description": "Unknown mock or channel" });
    let data = |summary: &str, id: &str| {
        json!({
            "tags": tags, "summary": summary, "operationId": id,
            "parameters": [name, channel],
            "responses": {
                "101": { "description": "Switching protocols (WebSocket)" },
                "200": { "description": "The channel, or text/event-stream of its messages" },
                "202": { "description": "A published message was valid" },
                "404": not_found,
                "422": { "description": "A published message was not valid: the issues" },
                "429": { "description": "Too many event connections" }
            }
        })
    };
    let methods = |summary: &str, id: &str| {
        let mut m = serde_json::Map::new();
        for method in ["get", "post", "put", "patch", "delete"] {
            let name = format!("{id}{}{}", method[..1].to_uppercase(), &method[1..]);
            m.insert(method.into(), data(summary, &name));
        }
        Value::Object(m)
    };
    json!({
        EVENTS_PATH: methods("A channel of an AsyncAPI mock (WebSocket, SSE, POST)", "mockEventChannel"),
        MOCK_EVENTS_PATH: methods("A channel below the mock's root", "mockOwnEventChannel"),
        CHANNELS_PATH: { "get": {
            "tags": tags, "summary": "Channels of an AsyncAPI mock", "operationId": "listMockChannels",
            "parameters": [name],
            "responses": { "200": { "description": "The channels" }, "404": not_found, "409": { "description": "Not an AsyncAPI mock" } }
        } },
        MESSAGES_PATH: { "get": {
            "tags": tags, "summary": "Messages of a channel", "operationId": "listChannelMessages",
            "parameters": [name, channel,
                { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 500 } },
                { "name": "after", "in": "query", "schema": { "type": "integer" }, "description": "Only messages after this sequence number" }],
            "responses": { "200": { "description": "The messages, newest first" }, "404": not_found }
        } },
        PUBLISH_PATH: { "post": {
            "tags": tags, "summary": "Publish a message on a channel", "operationId": "publishChannelMessage",
            "parameters": [name, channel],
            "requestBody": { "content": { "application/json": { "schema": { "type": "object", "additionalProperties": false, "properties": {
                "payload": { "description": "Published in place of the channel's next message" },
                "count": { "type": "integer", "minimum": 1, "maximum": 20 }
            } } } } },
            "responses": { "200": { "description": "What was published" }, "404": not_found, "409": { "description": "Not live, not an AsyncAPI mock, or the channel publishes nothing" } }
        } },
        SUBSCRIBERS_PATH: {
            "get": {
                "tags": tags, "summary": "Webhook subscribers", "operationId": "listMockSubscribers",
                "parameters": [name],
                "responses": { "200": { "description": "The subscribers" }, "404": not_found }
            },
            "post": {
                "tags": tags, "summary": "Add a webhook subscriber", "operationId": "addMockSubscriber",
                "parameters": [name],
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "additionalProperties": false, "required": ["channel", "url"], "properties": {
                    "channel": { "type": "string" },
                    "url": { "type": "string", "description": "Its host must be allowed for the outbound purpose events" },
                    "ttl_seconds": { "type": "integer", "minimum": 1, "maximum": 86400, "default": 3600 }
                } } } } },
                "responses": { "201": { "description": "The subscriber" }, "403": { "description": "The host is not allowed (reason egress_refused), or a self-service mock" }, "404": not_found, "429": { "description": "Too many subscribers" } }
            }
        },
        SUBSCRIBER_PATH: { "delete": {
            "tags": tags, "summary": "Remove a webhook subscriber", "operationId": "removeMockSubscriber",
            "parameters": [name, { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
            "responses": { "200": { "description": "Removed" }, "404": not_found }
        } }
    })
}
