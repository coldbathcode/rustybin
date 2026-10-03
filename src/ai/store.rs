//! Bounded in-memory state of the mock LLM:
//! - [`RequestStore`]: what each AI request looked like upstream (for
//!   `GET /ai/requests/{id}`), capped and expiring;
//! - [`PromptCache`]: prompt-prefix hashes seen recently, so Anthropic
//!   `cache_control` requests report cache creation then cache reads.

use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// One AI exchange as the upstream saw it.
#[derive(Clone, Debug, Serialize)]
pub struct AiRecord {
    pub request_id: String,
    pub timestamp: String,
    #[serde(skip)]
    pub session: String,
    pub provider: &'static str,
    pub method: String,
    pub endpoint: String,
    pub query: Option<String>,
    pub model: String,
    pub mode: String,
    pub stream: bool,
    /// Credential seen (redacted), if any.
    pub credential: Option<String>,
    /// Request headers (credential values redacted).
    pub headers: Vec<(String, String)>,
    /// Request body (JSON when parseable, else text; truncated).
    pub body: Value,
    /// The normalised prompt (see `engine::render`).
    pub normalized_prompt: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub finish_reason: String,
    pub reply_preview: String,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Ring buffer of [`AiRecord`]s with a TTL.
pub struct RequestStore {
    capacity: usize,
    ttl: Duration,
    entries: Mutex<VecDeque<(Instant, Arc<AiRecord>)>>,
}

impl RequestStore {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            ttl,
            entries: Mutex::new(VecDeque::new()),
        }
    }

    pub fn insert(&self, rec: AiRecord) {
        let mut e = lock(&self.entries);
        let now = Instant::now();
        while e
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > self.ttl)
        {
            e.pop_front();
        }
        while e.len() >= self.capacity {
            e.pop_front();
        }
        e.push_back((now, Arc::new(rec)));
    }

    pub fn get(&self, request_id: &str) -> Option<Arc<AiRecord>> {
        let now = Instant::now();
        lock(&self.entries)
            .iter()
            .rev()
            .find(|(t, r)| r.request_id == request_id && now.duration_since(*t) <= self.ttl)
            .map(|(_, r)| r.clone())
    }

    /// Newest first; `session` filters when given.
    pub fn list(&self, session: Option<&str>, limit: usize) -> Vec<Arc<AiRecord>> {
        let now = Instant::now();
        lock(&self.entries)
            .iter()
            .rev()
            .filter(|(t, _)| now.duration_since(*t) <= self.ttl)
            .filter(|(_, r)| session.is_none_or(|s| r.session == s))
            .take(limit)
            .map(|(_, r)| r.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Recently seen prompt prefixes (capacity + TTL, like a 5 minute cache).
pub struct PromptCache {
    capacity: usize,
    ttl: Duration,
    seen: Mutex<HashMap<[u8; 32], Instant>>,
}

impl PromptCache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            ttl,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// True when `key` was seen within the TTL. Records/refreshes it.
    pub fn hit(&self, key: [u8; 32]) -> bool {
        let now = Instant::now();
        let mut m = lock(&self.seen);
        let was = m
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) <= self.ttl);
        if !m.contains_key(&key) && m.len() >= self.capacity {
            m.retain(|_, t| now.duration_since(*t) <= self.ttl);
            if m.len() >= self.capacity {
                if let Some(oldest) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| *k) {
                    m.remove(&oldest);
                }
            }
        }
        m.insert(key, now);
        was
    }

    pub fn len(&self) -> usize {
        lock(&self.seen).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, session: &str) -> AiRecord {
        AiRecord {
            request_id: id.into(),
            timestamp: String::new(),
            session: session.into(),
            provider: "openai",
            method: "POST".into(),
            endpoint: "/x".into(),
            query: None,
            model: "m".into(),
            mode: "canned".into(),
            stream: false,
            credential: None,
            headers: vec![],
            body: Value::Null,
            normalized_prompt: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            finish_reason: "stop".into(),
            reply_preview: String::new(),
        }
    }

    #[test]
    fn store_is_bounded() {
        let s = RequestStore::new(3, Duration::from_secs(60));
        for i in 0..10 {
            s.insert(rec(&i.to_string(), if i % 2 == 0 { "a" } else { "b" }));
        }
        assert_eq!(s.len(), 3);
        assert!(s.get("9").is_some());
        assert!(s.get("1").is_none());
        assert_eq!(s.list(Some("b"), 10).len(), 2);
    }

    #[test]
    fn cache_is_bounded_and_hits() {
        let c = PromptCache::new(2, Duration::from_secs(60));
        assert!(!c.hit([1; 32]));
        assert!(c.hit([1; 32]));
        assert!(!c.hit([2; 32]));
        assert!(!c.hit([3; 32]));
        assert_eq!(c.len(), 2);
    }
}
