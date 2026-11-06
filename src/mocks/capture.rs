//! Bounded log of the requests a mock received (ring + byte budget) with a
//! live broadcast feed, like a request bin but with the mock's decision:
//! matched operation, validation issues, auth result and the response sent.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::broadcast;

use super::schema::Issue;
use crate::request_bin::BinHeader;

/// Bytes of a response body kept in a capture.
pub const MAX_RESPONSE_PREVIEW: usize = 8 * 1024;

/// Who sent a captured request, as far as a public instance can tell. Two
/// variants, never one prefixed string: a session name may hold a `:`, so
/// a client could otherwise name itself after a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sender {
    /// The session key: `X-Rustybin-Session`, else `ip:<client address>`.
    Session(String),
    /// The API key's id in shared mode.
    Key(String),
}

/// Which of a mock's captures a caller reads. Every reader of a capture
/// (the routes, the stream, the rule tester, the tools) goes through this:
/// a capture holds the sender's credentials as a gateway forwarded them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrafficScope {
    All,
    Own(Sender),
}

impl TrafficScope {
    pub fn sees(&self, c: &MockCapture) -> bool {
        match self {
            TrafficScope::All => true,
            TrafficScope::Own(sender) => c.sender.as_ref() == Some(sender),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MockCapture {
    /// Sequence number in the mock, from 1.
    pub seq: u64,
    pub id: String,
    pub timestamp: String,
    pub method: String,
    /// Path below the mock root.
    pub path: String,
    pub full_path: String,
    pub query: Option<String>,
    pub headers: Vec<BinHeader>,
    pub content_type: Option<String>,
    pub body: Option<String>,
    pub body_base64: Option<String>,
    pub body_size: usize,
    pub body_truncated: bool,
    pub client_ip: Option<String>,
    /// `mocked`, `not_found`, `method_not_allowed`, `unauthorized`,
    /// `forbidden`, `invalid_request`, `not_acceptable`, `disabled` or
    /// `not_deployed`.
    pub outcome: &'static str,
    pub operation: Option<String>,
    /// Path parameters of the matched operation (decoded).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<BinHeader>,
    /// Revision that answered.
    pub revision: u64,
    pub status: u16,
    /// How the response was chosen.
    pub source: Option<String>,
    /// Request validation issues (empty when valid or validation is off).
    pub validation: Vec<Issue>,
    pub auth: Option<String>,
    pub response_headers: Vec<BinHeader>,
    pub response_body: Option<String>,
    pub response_truncated: bool,
    pub duration_ms: u64,
    /// `mcp` when the mock's MCP server sent the request (a tool call).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'static str>,
    /// The MCP tool that sent it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Who sent it, for [`TrafficScope`]; never in an answer. `None` when
    /// nothing identifies the sender: then only `All` reads it.
    #[serde(skip)]
    pub sender: Option<Sender>,
}

impl MockCapture {
    fn stored_bytes(&self) -> usize {
        let headers: usize = self
            .headers
            .iter()
            .chain(self.response_headers.iter())
            .map(|h| h.name.len() + h.value.len())
            .sum();
        headers
            + self.body.as_ref().map_or(0, String::len)
            + self.body_base64.as_ref().map_or(0, String::len)
            + self.response_body.as_ref().map_or(0, String::len)
            + self.full_path.len()
            + self.tool.as_ref().map_or(0, String::len)
            + self.query.as_ref().map_or(0, String::len)
            + self
                .params
                .iter()
                .map(|p| p.name.len() + p.value.len())
                .sum::<usize>()
            + self
                .validation
                .iter()
                .map(|i| i.pointer.len() + i.message.len())
                .sum::<usize>()
    }
}

/// Most operations tracked per mock (more are counted under none).
const MAX_TRACKED_OPERATIONS: usize = 1000;
/// Minutes of per-minute counts kept (the last hour).
const MINUTES: usize = 60;

/// Calls and last hit of one operation.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct OpStats {
    pub hits: u64,
    pub last_hit: Option<String>,
}

struct LogInner {
    entries: VecDeque<Arc<MockCapture>>,
    bytes: usize,
    next_seq: u64,
    last_hit: Option<String>,
    operations: HashMap<String, OpStats>,
    /// (minute since the epoch, calls), oldest first.
    minutes: VecDeque<(i64, u64)>,
}

/// Cheap to clone; shared by the store entry and request handlers.
#[derive(Clone)]
pub struct CaptureLog {
    inner: Arc<Mutex<LogInner>>,
    tx: broadcast::Sender<Arc<MockCapture>>,
    max_entries: usize,
    max_bytes: usize,
}

impl CaptureLog {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(Mutex::new(LogInner {
                entries: VecDeque::new(),
                bytes: 0,
                next_seq: 1,
                last_hit: None,
                operations: HashMap::new(),
                minutes: VecDeque::new(),
            })),
            tx,
            max_entries,
            max_bytes,
        }
    }

    fn lock(&self) -> MutexGuard<'_, LogInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Store a capture (assigns `seq`) and publish it.
    pub fn record(&self, entry: MockCapture) -> Arc<MockCapture> {
        self.record_at(entry, chrono::Utc::now().timestamp() / 60)
    }

    fn record_at(&self, mut entry: MockCapture, minute: i64) -> Arc<MockCapture> {
        let mut inner = self.lock();
        entry.seq = inner.next_seq;
        inner.next_seq += 1;
        inner.last_hit = Some(entry.timestamp.clone());
        if let Some(op) = &entry.operation {
            let tracked = inner.operations.len() < MAX_TRACKED_OPERATIONS
                || inner.operations.contains_key(op);
            if tracked {
                let stats = inner.operations.entry(op.clone()).or_default();
                stats.hits += 1;
                stats.last_hit = Some(entry.timestamp.clone());
            }
        }
        match inner.minutes.back_mut() {
            Some((m, n)) if *m == minute => *n += 1,
            _ => inner.minutes.push_back((minute, 1)),
        }
        while inner
            .minutes
            .front()
            .is_some_and(|(m, _)| *m <= minute - MINUTES as i64)
        {
            inner.minutes.pop_front();
        }
        let entry = Arc::new(entry);
        let size = entry.stored_bytes();
        while !inner.entries.is_empty()
            && (inner.entries.len() >= self.max_entries || inner.bytes + size > self.max_bytes)
        {
            if let Some(old) = inner.entries.pop_front() {
                inner.bytes = inner.bytes.saturating_sub(old.stored_bytes());
            }
        }
        inner.bytes += size;
        inner.entries.push_back(entry.clone());
        let _ = self.tx.send(entry.clone());
        entry
    }

    /// Newest first, at most `limit`.
    pub fn entries(&self, limit: usize) -> Vec<Arc<MockCapture>> {
        self.lock()
            .entries
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Newest first, at most `limit` of what `scope` sees, and how many it
    /// sees in all (`All`: every request since the mock started; `Own`: the
    /// caller's still in the ring, so other people's calls are not counted).
    pub fn visible(&self, scope: &TrafficScope, limit: usize) -> (Vec<Arc<MockCapture>>, u64) {
        if *scope == TrafficScope::All {
            return (self.entries(limit), self.total());
        }
        let inner = self.lock();
        let own: Vec<_> = inner
            .entries
            .iter()
            .rev()
            .filter(|e| scope.sees(e))
            .collect();
        let total = own.len() as u64;
        (own.into_iter().take(limit).cloned().collect(), total)
    }

    pub fn entry(&self, seq: u64) -> Option<Arc<MockCapture>> {
        self.lock().entries.iter().find(|e| e.seq == seq).cloned()
    }

    pub fn total(&self) -> u64 {
        self.lock().next_seq.saturating_sub(1)
    }

    pub fn stored(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn last_hit(&self) -> Option<String> {
        self.lock().last_hit.clone()
    }

    /// Calls and last hit per operation id.
    pub fn operation_stats(&self) -> HashMap<String, OpStats> {
        self.lock().operations.clone()
    }

    /// Calls in the last hour.
    pub fn last_hour(&self) -> u64 {
        self.last_hour_at(chrono::Utc::now().timestamp() / 60)
    }

    fn last_hour_at(&self, minute: i64) -> u64 {
        self.lock()
            .minutes
            .iter()
            .filter(|(m, _)| *m > minute - MINUTES as i64)
            .map(|(_, n)| n)
            .sum()
    }

    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.bytes = 0;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<MockCapture>> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn capture(body: &str) -> MockCapture {
        MockCapture {
            seq: 0,
            id: String::new(),
            timestamp: "t".into(),
            method: "GET".into(),
            path: "/".into(),
            full_path: "/mock/x".into(),
            query: None,
            headers: Vec::new(),
            content_type: None,
            body: Some(body.into()),
            body_base64: None,
            body_size: body.len(),
            body_truncated: false,
            client_ip: None,
            outcome: "mocked",
            operation: None,
            params: Vec::new(),
            revision: 1,
            status: 200,
            source: None,
            validation: Vec::new(),
            auth: None,
            response_headers: Vec::new(),
            response_body: None,
            response_truncated: false,
            duration_ms: 0,
            channel: None,
            tool: None,
            sender: None,
        }
    }

    #[test]
    fn ring_and_byte_budget() {
        let log = CaptureLog::new(3, 200);
        for _ in 0..5 {
            log.record(capture("x"));
        }
        let seqs: Vec<u64> = log.entries(10).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![5, 4, 3]);
        log.record(capture(&"y".repeat(190)));
        assert_eq!(log.stored(), 1);
        assert_eq!(log.total(), 6);
        assert!(log.entry(6).is_some());
        assert_eq!(log.last_hit().as_deref(), Some("t"));
        log.clear();
        assert_eq!(log.stored(), 0);
        assert_eq!(log.total(), 6);
    }

    #[test]
    fn operation_and_hourly_stats() {
        let log = CaptureLog::new(3, 10_000);
        let with_op = |op: &str| {
            let mut c = capture("");
            c.operation = Some(op.into());
            c
        };
        log.record_at(with_op("a"), 100);
        log.record_at(with_op("a"), 100);
        log.record_at(with_op("b"), 130);
        log.record_at(capture(""), 159);
        let stats = log.operation_stats();
        assert_eq!(stats["a"].hits, 2);
        assert_eq!(stats["b"].hits, 1);
        assert_eq!(stats.len(), 2);
        assert_eq!(log.last_hour_at(159), 4);
        assert_eq!(log.last_hour_at(160), 2);
        assert_eq!(log.last_hour_at(300), 0);
        log.record_at(capture(""), 200);
        assert_eq!(log.lock().minutes.len(), 2);
    }
}
