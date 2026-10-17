//! Per-key daily counters (requests and response bytes per UTC day).
//!
//! Each replica counts locally. With a quota store (`RUSTYBIN_QUOTA_STORE`)
//! the local deltas are flushed periodically and the store answers with the
//! day's totals across every replica; the replica's view of a key's day is
//! then `store total + local count not yet flushed`. Without a store the
//! view is the local count.
//!
//! Bounded: entries exist only for valid keys, only today and the two
//! previous days are kept, and the number of entries is capped.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::NaiveDate;

/// Days kept besides today (so yesterday is complete for a whole day).
pub const DAYS_KEPT: i64 = 2;

/// Which daily quota rejected a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaKind {
    Requests,
    Egress,
}

impl QuotaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaKind::Requests => "requests",
            QuotaKind::Egress => "egress",
        }
    }
}

/// Local counts not yet flushed to the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    pub day: NaiveDate,
    pub key_id: Arc<str>,
    pub requests: u64,
    pub egress_bytes: u64,
}

/// A day's totals of one key (from the store, or local).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Total {
    pub day: NaiveDate,
    pub key_id: String,
    pub requests: u64,
    pub egress_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    /// Counted by this replica.
    requests: u64,
    egress: u64,
    /// Part of the local counts already sent to the store.
    flushed_requests: u64,
    flushed_egress: u64,
    /// Totals the store reported at the last flush.
    store_requests: u64,
    store_egress: u64,
}

impl Entry {
    fn requests(&self) -> u64 {
        self.store_requests
            .saturating_add(self.requests.saturating_sub(self.flushed_requests))
    }

    fn egress(&self) -> u64 {
        self.store_egress
            .saturating_add(self.egress.saturating_sub(self.flushed_egress))
    }
}

#[derive(Default)]
struct Inner {
    days: BTreeMap<NaiveDate, HashMap<Arc<str>, Entry>>,
    entries: usize,
    /// Warned about the entry cap (once per day).
    warned: Option<NaiveDate>,
}

impl Inner {
    fn prune(&mut self, today: NaiveDate) {
        let oldest = today - chrono::Duration::days(DAYS_KEPT);
        while let Some((&day, _)) = self.days.first_key_value() {
            if day >= oldest {
                break;
            }
            if let Some(map) = self.days.remove(&day) {
                self.entries = self.entries.saturating_sub(map.len());
            }
        }
    }

    /// The entry of a key on a day, created when there is room.
    fn entry(&mut self, day: NaiveDate, key: &Arc<str>, max: usize) -> Option<&mut Entry> {
        let exists = self.days.get(&day).is_some_and(|m| m.contains_key(key));
        if !exists {
            if self.entries >= max {
                self.prune(day);
            }
            if self.entries >= max {
                if self.warned != Some(day) {
                    self.warned = Some(day);
                    tracing::warn!(
                        "per-key usage counters are full ({max} entries): new keys are not counted today"
                    );
                }
                return None;
            }
            self.entries += 1;
        }
        Some(
            self.days
                .entry(day)
                .or_default()
                .entry(key.clone())
                .or_default(),
        )
    }
}

/// The per-key daily counters of this replica.
pub struct Ledger {
    inner: Mutex<Inner>,
    max_entries: usize,
}

impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledger")
            .field("max_entries", &self.max_entries)
            .finish_non_exhaustive()
    }
}

impl Ledger {
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_entries: max_entries.max(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Count one request unless a daily quota is used up (0 = unlimited).
    /// Returns the requests left today after this one (`None` without a
    /// request quota). A key that cannot be tracked (counter cap reached) is
    /// let through uncounted.
    pub fn admit(
        &self,
        day: NaiveDate,
        key: &Arc<str>,
        max_requests: u64,
        max_egress: u64,
    ) -> Result<Option<u64>, QuotaKind> {
        let mut inner = self.lock();
        if inner.days.last_key_value().is_some_and(|(d, _)| *d < day) {
            inner.prune(day);
        }
        let Some(e) = inner.entry(day, key, self.max_entries) else {
            return Ok(None);
        };
        if max_requests > 0 && e.requests() >= max_requests {
            return Err(QuotaKind::Requests);
        }
        if max_egress > 0 && e.egress() >= max_egress {
            return Err(QuotaKind::Egress);
        }
        e.requests = e.requests.saturating_add(1);
        Ok((max_requests > 0).then(|| max_requests.saturating_sub(e.requests())))
    }

    /// Give back the count of an admitted request that was rejected later
    /// (by the rate limiter).
    pub fn uncount(&self, day: NaiveDate, key: &str) {
        let mut inner = self.lock();
        if let Some(e) = inner.days.get_mut(&day).and_then(|m| m.get_mut(key)) {
            // Never below what was already flushed.
            if e.requests > e.flushed_requests {
                e.requests -= 1;
            }
        }
    }

    /// Count response bytes (only for keys already counted that day).
    pub fn add_egress(&self, day: NaiveDate, key: &str, bytes: u64) {
        let mut inner = self.lock();
        if let Some(e) = inner.days.get_mut(&day).and_then(|m| m.get_mut(key)) {
            e.egress = e.egress.saturating_add(bytes);
        }
    }

    /// This replica's view of a key's day: (requests, egress bytes).
    pub fn totals(&self, day: NaiveDate, key: &str) -> (u64, u64) {
        self.lock()
            .days
            .get(&day)
            .and_then(|m| m.get(key))
            .map(|e| (e.requests(), e.egress()))
            .unwrap_or((0, 0))
    }

    /// One page of a day's totals, sorted by key id, starting after `after`.
    /// Returns the rows and the key id to continue from (when more exist).
    pub fn page(
        &self,
        day: NaiveDate,
        after: Option<&str>,
        limit: usize,
    ) -> (Vec<Total>, Option<String>) {
        let inner = self.lock();
        let Some(map) = inner.days.get(&day) else {
            return (Vec::new(), None);
        };
        let mut ids: Vec<&Arc<str>> = map
            .keys()
            .filter(|k| after.is_none_or(|a| k.as_ref() > a))
            .collect();
        ids.sort_unstable();
        let more = ids.len() > limit;
        let rows: Vec<Total> = ids
            .into_iter()
            .take(limit)
            .filter_map(|k| {
                map.get(k).map(|e| Total {
                    day,
                    key_id: k.to_string(),
                    requests: e.requests(),
                    egress_bytes: e.egress(),
                })
            })
            .collect();
        let next = if more {
            rows.last().map(|r| r.key_id.clone())
        } else {
            None
        };
        (rows, next)
    }

    /// Local counts not yet flushed (every key and day with a change).
    pub fn pending(&self) -> Vec<Delta> {
        let inner = self.lock();
        let mut out = Vec::new();
        for (day, map) in &inner.days {
            for (key, e) in map {
                let requests = e.requests.saturating_sub(e.flushed_requests);
                let egress_bytes = e.egress.saturating_sub(e.flushed_egress);
                if requests > 0 || egress_bytes > 0 {
                    out.push(Delta {
                        day: *day,
                        key_id: key.clone(),
                        requests,
                        egress_bytes,
                    });
                }
            }
        }
        out
    }

    /// Record a successful flush: `sent` is now in the store, whose totals
    /// after the flush are `totals`.
    pub fn apply(&self, sent: &[Delta], totals: &[Total]) {
        let mut inner = self.lock();
        for d in sent {
            if let Some(e) = inner
                .days
                .get_mut(&d.day)
                .and_then(|m| m.get_mut(d.key_id.as_ref()))
            {
                e.flushed_requests = e.flushed_requests.saturating_add(d.requests);
                e.flushed_egress = e.flushed_egress.saturating_add(d.egress_bytes);
            }
        }
        for t in totals {
            if let Some(e) = inner
                .days
                .get_mut(&t.day)
                .and_then(|m| m.get_mut(t.key_id.as_str()))
            {
                e.store_requests = t.requests;
                e.store_egress = t.egress_bytes;
            }
        }
    }

    /// Drop days older than the ones kept.
    pub fn prune(&self, today: NaiveDate) {
        self.lock().prune(today);
    }

    /// Number of (key, day) entries held.
    pub fn len(&self) -> usize {
        self.lock().entries
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
