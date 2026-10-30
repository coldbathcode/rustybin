//! Portal insights: bounded counters per item. Views per day for the last
//! [`DAYS`] days, downloads by format, Try it calls. No visitor identifier
//! is stored, only counts.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Days of views kept per item.
pub const DAYS: usize = 30;
/// Items with counters (the least recently counted goes first).
pub const MAX_TRACKED: usize = 512;
/// Download formats counted per item (the collection formats).
const MAX_FORMATS: usize = 16;

/// What the portal counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    View,
    Try,
    Download(&'a str),
}

/// One item's counters.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ItemStats {
    /// (day, views), oldest first, at most [`DAYS`] entries.
    #[serde(default)]
    pub views: Vec<(NaiveDate, u64)>,
    #[serde(default)]
    pub downloads: BTreeMap<String, u64>,
    #[serde(default)]
    pub tries: u64,
    /// The last day anything was counted (eviction order).
    #[serde(default)]
    pub last: Option<NaiveDate>,
}

impl ItemStats {
    /// Views in the `days` days ending `today`.
    pub fn views_since(&self, today: NaiveDate, days: i64) -> u64 {
        self.views
            .iter()
            .filter(|(d, _)| (today - *d).num_days() < days && *d <= today)
            .map(|(_, n)| n)
            .sum()
    }

    fn bound(&mut self, today: Option<NaiveDate>) {
        if let Some(today) = today {
            self.views
                .retain(|(d, _)| (today - *d).num_days() < DAYS as i64);
        }
        let excess = self.views.len().saturating_sub(DAYS);
        self.views.drain(..excess);
        while self.downloads.len() > MAX_FORMATS {
            self.downloads.pop_last();
        }
    }
}

/// Every item's counters.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Insights {
    #[serde(default)]
    pub items: BTreeMap<String, ItemStats>,
}

impl Insights {
    pub fn record(&mut self, id: &str, event: Event, today: NaiveDate) {
        if !self.items.contains_key(id) && self.items.len() >= MAX_TRACKED {
            // Evict the item counted least recently.
            if let Some(oldest) = self
                .items
                .iter()
                .min_by_key(|(_, s)| s.last)
                .map(|(k, _)| k.clone())
            {
                self.items.remove(&oldest);
            }
        }
        let stats = self.items.entry(id.to_string()).or_default();
        stats.last = Some(today);
        match event {
            Event::View => match stats.views.last_mut() {
                Some((d, n)) if *d == today => *n += 1,
                _ => stats.views.push((today, 1)),
            },
            Event::Try => stats.tries += 1,
            Event::Download(format) => {
                if stats.downloads.contains_key(format) || stats.downloads.len() < MAX_FORMATS {
                    *stats.downloads.entry(format.to_string()).or_default() += 1;
                }
            }
        }
        stats.bound(Some(today));
    }

    /// Enforce the caps on a document read from disk.
    pub fn bound(&mut self) {
        while self.items.len() > MAX_TRACKED {
            self.items.pop_last();
        }
        for s in self.items.values_mut() {
            s.bound(None);
        }
    }

    /// One item's counters for the console.
    pub fn item_json(&self, id: &str, today: NaiveDate) -> Value {
        let empty = ItemStats::default();
        let s = self.items.get(id).unwrap_or(&empty);
        json!({
            "views_7d": s.views_since(today, 7),
            "views_30d": s.views_since(today, DAYS as i64),
            "views_by_day": s
                .views
                .iter()
                .filter(|(d, _)| (today - *d).num_days() < DAYS as i64)
                .map(|(d, n)| json!({ "date": d.to_string(), "views": n }))
                .collect::<Vec<_>>(),
            "downloads": s.downloads,
            "downloads_total": s.downloads.values().sum::<u64>(),
            "tries": s.tries,
        })
    }
}
