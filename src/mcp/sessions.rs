//! Bounded session store for the legacy (handshake era) transports.
//!
//! Sessions are created by `initialize` (Streamable HTTP, 2025-xx) or by
//! opening `GET /mcp/sse` (HTTP+SSE, 2024-11-05). The store has a capacity
//! cap (least recently used session evicted first) and an idle TTL; every
//! access purges expired entries. Per-session maps (pending server-to-client
//! requests, in-flight requests, subscriptions) are capped as well.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Notify};

use super::protocol::Version;

/// Per-session caps.
pub const MAX_PENDING: usize = 32;
pub const MAX_INFLIGHT: usize = 64;
pub const MAX_SUBSCRIPTIONS: usize = 32;

/// A one-shot cancellation signal shared by a request and whoever may cancel it.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }

    /// Resolves once [`cancel`](Self::cancel) has been called.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// A guard that cancels the token when dropped (client went away).
    pub fn drop_guard(&self) -> CancelOnDrop {
        CancelOnDrop(self.clone())
    }
}

pub struct CancelOnDrop(CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Which legacy transport owns the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    StreamableHttp,
    HttpSse,
}

pub struct Session {
    pub id: String,
    /// Profile (server variant) the session was created on; requests on
    /// another variant's endpoint do not see it.
    pub profile: String,
    pub transport: TransportKind,
    /// Authenticated principal (`sub`) bound at creation, if any.
    pub principal: Option<String>,
    /// Cancelled when the session is terminated or evicted.
    pub closed: CancelToken,
    state: Mutex<SessionState>,
    next_request: AtomicU64,
}

pub struct SessionState {
    pub version: Version,
    pub client_capabilities: Value,
    pub client_info: Value,
    pub initialized: bool,
    pub log_level: Option<String>,
    pub subscriptions: HashSet<String>,
    /// Server-to-client requests waiting for the client's response.
    pending: HashMap<String, oneshot::Sender<Value>>,
    /// Client requests being processed (for `notifications/cancelled`).
    inflight: HashMap<String, CancelToken>,
    /// Standalone stream (GET /mcp, or the HTTP+SSE stream).
    pub outbound: Option<mpsc::Sender<Value>>,
    last_seen: Instant,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// JSON-RPC ids compare by their JSON text (`1` and `"1"` differ).
pub fn id_key(id: &Value) -> String {
    id.to_string()
}

impl Session {
    pub fn new(
        profile: &str,
        transport: TransportKind,
        version: Version,
        principal: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            profile: profile.to_string(),
            transport,
            principal,
            closed: CancelToken::new(),
            state: Mutex::new(SessionState {
                version,
                client_capabilities: Value::Object(Default::default()),
                client_info: Value::Null,
                initialized: false,
                log_level: None,
                subscriptions: HashSet::new(),
                pending: HashMap::new(),
                inflight: HashMap::new(),
                outbound: None,
                last_seen: Instant::now(),
            }),
            next_request: AtomicU64::new(1),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, SessionState> {
        lock(&self.state)
    }

    pub fn version(&self) -> Version {
        self.state().version
    }

    pub fn touch(&self) {
        self.state().last_seen = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.state().last_seen.elapsed()
    }

    /// Register a server-to-client request; returns its id and the receiver
    /// for the client's response. `None` when too many are pending.
    pub fn register_pending(&self) -> Option<(String, oneshot::Receiver<Value>)> {
        let n = self.next_request.fetch_add(1, Ordering::Relaxed);
        let id = format!("rustybin-{n}");
        let (tx, rx) = oneshot::channel();
        let mut st = self.state();
        st.pending.retain(|_, tx| !tx.is_closed());
        if st.pending.len() >= MAX_PENDING {
            return None;
        }
        st.pending.insert(id_key(&Value::String(id.clone())), tx);
        Some((id, rx))
    }

    pub fn forget_pending(&self, id: &str) {
        self.state()
            .pending
            .remove(&id_key(&Value::String(id.to_string())));
    }

    /// Deliver a client response to a waiting server-to-client request.
    pub fn resolve_pending(&self, id: &Value, body: Value) -> bool {
        let tx = self.state().pending.remove(&id_key(id));
        match tx {
            Some(tx) => tx.send(body).is_ok(),
            None => false,
        }
    }

    /// Track an in-flight client request so it can be cancelled.
    pub fn track_inflight(&self, id: &Value, token: CancelToken) {
        let mut st = self.state();
        if st.inflight.len() >= MAX_INFLIGHT {
            st.inflight.retain(|_, t| !t.is_cancelled());
            if st.inflight.len() >= MAX_INFLIGHT {
                return;
            }
        }
        st.inflight.insert(id_key(id), token);
    }

    pub fn finish_inflight(&self, id: &Value) {
        self.state().inflight.remove(&id_key(id));
    }

    /// `notifications/cancelled`: cancel the in-flight request, if any.
    pub fn cancel_inflight(&self, id: &Value) -> bool {
        match self.state().inflight.remove(&id_key(id)) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Send a message on the standalone stream (best effort, never blocks).
    pub fn send_outbound(&self, msg: Value) -> bool {
        let tx = self.state().outbound.clone();
        tx.is_some_and(|tx| tx.try_send(msg).is_ok())
    }
}

/// The session map with capacity and idle TTL bounds.
pub struct SessionStore {
    map: Mutex<HashMap<String, Arc<Session>>>,
    capacity: usize,
    ttl: Duration,
}

impl SessionStore {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            ttl,
        }
    }

    fn purge(&self, map: &mut HashMap<String, Arc<Session>>) {
        let ttl = self.ttl;
        map.retain(|_, s| {
            let keep = s.idle_for() < ttl && !s.closed.is_cancelled();
            if !keep {
                s.closed.cancel();
            }
            keep
        });
    }

    /// Insert a new session, evicting expired ones and then the least
    /// recently used one if the store is full.
    pub fn insert(&self, session: Arc<Session>) {
        let mut map = lock(&self.map);
        self.purge(&mut map);
        while map.len() >= self.capacity {
            let oldest = map
                .iter()
                .max_by_key(|(_, s)| s.idle_for())
                .map(|(k, _)| k.clone());
            match oldest.and_then(|k| map.remove(&k)) {
                Some(evicted) => evicted.closed.cancel(),
                None => break,
            }
        }
        map.insert(session.id.clone(), session);
    }

    /// Look up a live session (and mark it used).
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        let mut map = lock(&self.map);
        self.purge(&mut map);
        let s = map.get(id).cloned()?;
        s.touch();
        Some(s)
    }

    /// Terminate a session.
    pub fn remove(&self, id: &str) -> Option<Arc<Session>> {
        let s = lock(&self.map).remove(id)?;
        s.closed.cancel();
        Some(s)
    }

    pub fn len(&self) -> usize {
        lock(&self.map).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Arc<Session> {
        Arc::new(Session::new(
            "default",
            TransportKind::StreamableHttp,
            Version::V2025_11_25,
            None,
        ))
    }

    #[test]
    fn store_is_bounded_lru() {
        let store = SessionStore::new(2, Duration::from_secs(60));
        let a = session();
        let b = session();
        let c = session();
        store.insert(a.clone());
        std::thread::sleep(Duration::from_millis(5));
        store.insert(b.clone());
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.get(&b.id).is_some());
        store.insert(c.clone());
        assert_eq!(store.len(), 2);
        assert!(store.get(&a.id).is_none(), "oldest evicted");
        assert!(a.closed.is_cancelled());
        assert!(store.get(&c.id).is_some());
    }

    #[test]
    fn store_expires_idle_sessions() {
        let store = SessionStore::new(10, Duration::from_millis(1));
        let a = session();
        store.insert(a.clone());
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.get(&a.id).is_none());
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn pending_requests_resolve() {
        let s = session();
        let (id, rx) = s.register_pending().expect("slot");
        assert!(s.resolve_pending(&json!(id), json!({"result": {}})));
        assert_eq!(rx.await.expect("value"), json!({"result": {}}));
        assert!(!s.resolve_pending(&json!("unknown"), json!({})));
    }

    #[tokio::test]
    async fn cancel_token_wakes_waiters() {
        let t = CancelToken::new();
        let t2 = t.clone();
        let h = tokio::spawn(async move { t2.cancelled().await });
        tokio::task::yield_now().await;
        {
            let _g = t.drop_guard();
        }
        h.await.expect("joined");
        assert!(t.is_cancelled());
    }
}
