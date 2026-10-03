//! A small bounded key/value store with per-entry expiry, used for
//! authorization codes, refresh tokens, registered clients and the token
//! denylist. Expired entries are purged on every write; when the store is
//! full the entry closest to expiry is evicted.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

struct Entry<V> {
    value: V,
    expires_at: i64,
    seq: u64,
}

pub struct BoundedStore<V> {
    entries: Mutex<HashMap<String, Entry<V>>>,
    next_seq: std::sync::atomic::AtomicU64,
    capacity: usize,
    ttl_secs: i64,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl<V: Clone> BoundedStore<V> {
    pub fn new(capacity: usize, ttl_secs: i64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            next_seq: std::sync::atomic::AtomicU64::new(0),
            capacity: capacity.max(1),
            ttl_secs,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry<V>>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Default lifetime of an entry in seconds.
    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    /// Insert with the default TTL; returns the expiry timestamp.
    pub fn insert(&self, key: String, value: V) -> i64 {
        let expires_at = now() + self.ttl_secs;
        self.insert_until(key, value, expires_at);
        expires_at
    }

    /// Insert with an explicit expiry (unix seconds).
    pub fn insert_until(&self, key: String, value: V, expires_at: i64) {
        let now = now();
        let mut map = self.lock();
        map.retain(|_, e| e.expires_at > now);
        if expires_at <= now {
            // Already expired: storing it would only evict a live entry.
            map.remove(&key);
            return;
        }
        let seq = self
            .next_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        while map.len() >= self.capacity && !map.contains_key(&key) {
            let oldest = map
                .iter()
                .min_by_key(|(_, e)| (e.expires_at, e.seq))
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    map.remove(&k);
                }
                None => break,
            }
        }
        map.insert(
            key,
            Entry {
                value,
                expires_at,
                seq,
            },
        );
    }

    /// The live value for `key`.
    pub fn get(&self, key: &str) -> Option<V> {
        let now = now();
        self.lock()
            .get(key)
            .filter(|e| e.expires_at > now)
            .map(|e| e.value.clone())
    }

    /// The live value and its expiry.
    pub fn get_with_expiry(&self, key: &str) -> Option<(V, i64)> {
        let now = now();
        self.lock()
            .get(key)
            .filter(|e| e.expires_at > now)
            .map(|e| (e.value.clone(), e.expires_at))
    }

    /// Remove and return the live value (single use).
    pub fn take(&self, key: &str) -> Option<V> {
        let now = now();
        self.lock()
            .remove(key)
            .filter(|e| e.expires_at > now)
            .map(|e| e.value)
    }

    pub fn remove(&self, key: &str) {
        self.lock().remove(key);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_and_expiry() {
        let store = BoundedStore::new(2, 60);
        store.insert("a".into(), 1);
        store.insert("b".into(), 2);
        store.insert("c".into(), 3);
        assert_eq!(store.len(), 2);
        store.insert_until("expired".into(), 4, now() - 1);
        assert_eq!(store.get("expired"), None);
        assert_eq!(store.take("expired"), None);
        assert_eq!(store.take("c"), Some(3));
        assert_eq!(store.take("c"), None);
        // Expired entries are purged on the next write.
        store.insert_until("x".into(), 5, now() - 1);
        store.insert("y".into(), 6);
        assert!(store.len() <= 2);
        assert!(store.contains("y"));
    }
}
