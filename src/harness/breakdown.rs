//! Per-operation and per-credential breakdowns (`docs/plans/harness.md`,
//! section 12): every answer of a layer that sends a mock's operations or
//! carries identities is counted by operation and by credential label, with
//! a small log-scale latency histogram (64 buckets, about 25 percent wide)
//! instead of an exact one: keys are many, exact histograms are large.
//! Accumulators add up exactly, so a cluster's leader merges its nodes'.
//! At most [`MAX_KEYS`] keys per layer and kind; the rest are counted under
//! `other`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Keys followed per layer and kind.
pub const MAX_KEYS: usize = 256;
/// Latency buckets.
const BUCKETS: usize = 64;
/// The upper bound of bucket 0, in microseconds.
const FIRST_US: f64 = 50.0;
const GROWTH: f64 = 1.25;
/// Keys shown in a report, per layer and kind.
pub const MAX_SHOWN: usize = 64;

fn bucket_of(us: u64) -> usize {
    if us as f64 <= FIRST_US {
        return 0;
    }
    (((us as f64 / FIRST_US).ln() / GROWTH.ln()).ceil() as usize).min(BUCKETS - 1)
}

fn bucket_upper_ms(k: usize) -> f64 {
    FIRST_US * GROWTH.powi(k as i32) / 1000.0
}

/// One key's counts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyAcc {
    pub answered: u64,
    pub ok: u64,
    pub unexpected: u64,
    pub transport: u64,
    pub limited: u64,
    pub latency_us: u64,
    #[serde(default)]
    pub hist: Vec<u32>,
}

/// What one answer adds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Answer {
    pub ok: bool,
    pub transport: bool,
    pub limited: bool,
    pub latency_us: u64,
}

impl KeyAcc {
    fn record(&mut self, a: &Answer) {
        if a.transport {
            self.transport += 1;
        } else {
            self.answered += 1;
            if a.ok {
                self.ok += 1;
            } else {
                self.unexpected += 1;
            }
        }
        if a.limited {
            self.limited += 1;
        }
        self.latency_us += a.latency_us;
        if self.hist.len() < BUCKETS {
            self.hist.resize(BUCKETS, 0);
        }
        self.hist[bucket_of(a.latency_us)] += 1;
    }

    fn merge(&mut self, o: &KeyAcc) {
        self.answered += o.answered;
        self.ok += o.ok;
        self.unexpected += o.unexpected;
        self.transport += o.transport;
        self.limited += o.limited;
        self.latency_us += o.latency_us;
        if self.hist.len() < o.hist.len() {
            self.hist.resize(o.hist.len(), 0);
        }
        for (a, b) in self.hist.iter_mut().zip(&o.hist) {
            *a += b;
        }
    }

    fn count(&self) -> u64 {
        self.answered + self.transport
    }

    /// The latency at quantile `q`, as the upper bound of its bucket.
    fn quantile_ms(&self, q: f64) -> f64 {
        let total: u64 = self.hist.iter().map(|&n| u64::from(n)).sum();
        if total == 0 {
            return 0.0;
        }
        let want = (q * total as f64).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (k, &n) in self.hist.iter().enumerate() {
            seen += u64::from(n);
            if seen >= want {
                return round(bucket_upper_ms(k));
            }
        }
        round(bucket_upper_ms(BUCKETS - 1))
    }
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// One layer's breakdowns.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayerKeys {
    #[serde(default)]
    pub operations: BTreeMap<String, KeyAcc>,
    #[serde(default)]
    pub credentials: BTreeMap<String, KeyAcc>,
}

fn add(map: &mut BTreeMap<String, KeyAcc>, key: &str, f: impl FnOnce(&mut KeyAcc)) {
    if let Some(k) = map.get_mut(key) {
        f(k);
    } else if map.len() < MAX_KEYS {
        f(map.entry(key.to_string()).or_default());
    } else {
        f(map.entry("other".into()).or_default());
    }
}

impl LayerKeys {
    pub fn record(&mut self, operation: Option<&str>, credential: Option<&str>, a: &Answer) {
        if let Some(op) = operation {
            add(&mut self.operations, op, |k| k.record(a));
        }
        if let Some(c) = credential {
            add(&mut self.credentials, c, |k| k.record(a));
        }
    }

    pub fn merge(&mut self, o: &LayerKeys) {
        for (mine, theirs) in [
            (&mut self.operations, &o.operations),
            (&mut self.credentials, &o.credentials),
        ] {
            for (key, acc) in theirs {
                add(mine, key, |k| k.merge(acc));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.operations.is_empty() && self.credentials.is_empty()
    }
}

/// One key as the report shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyReport {
    pub layer: String,
    pub name: String,
    pub answered: u64,
    pub ok: u64,
    /// Unexpected statuses plus transport failures.
    pub errors: u64,
    pub limited: u64,
    pub mean_ms: f64,
    /// From the bucketed histogram: an upper bound within 25 percent.
    pub p50_ms: f64,
    pub p99_ms: f64,
}

/// The report's breakdowns: (operations, credentials), busiest first.
pub fn reports(names: &[String], layers: &[LayerKeys]) -> (Vec<KeyReport>, Vec<KeyReport>) {
    let mut ops = Vec::new();
    let mut creds = Vec::new();
    for (name, keys) in names.iter().zip(layers) {
        for (into, map) in [
            (&mut ops, &keys.operations),
            (&mut creds, &keys.credentials),
        ] {
            let mut rows: Vec<KeyReport> = map
                .iter()
                .map(|(key, k)| KeyReport {
                    layer: name.clone(),
                    name: key.clone(),
                    answered: k.answered,
                    ok: k.ok,
                    errors: k.unexpected + k.transport,
                    limited: k.limited,
                    mean_ms: if k.count() == 0 {
                        0.0
                    } else {
                        round(k.latency_us as f64 / k.count() as f64 / 1000.0)
                    },
                    p50_ms: k.quantile_ms(0.5),
                    p99_ms: k.quantile_ms(0.99),
                })
                .collect();
            rows.sort_by(|a, b| b.answered.cmp(&a.answered).then(a.name.cmp(&b.name)));
            rows.truncate(MAX_SHOWN);
            into.extend(rows);
        }
    }
    (ops, creds)
}

/// What the operation breakdown says in words: invalid requests that were
/// not refused (nothing on the way validated them), reads after a create
/// that did not find the item.
pub fn findings(operations: &[KeyReport]) -> Vec<String> {
    let mut out = Vec::new();
    let sum = |suffix: &str, f: fn(&KeyReport) -> u64| -> (u64, u64) {
        operations
            .iter()
            .filter(|r| r.name.ends_with(suffix))
            .fold((0, 0), |(a, b), r| (a + f(r), b + r.answered))
    };
    let (accepted, invalid) = sum(" (invalid)", |r| r.answered - r.ok.min(r.answered));
    if accepted > 0 {
        out.push(format!(
            "{accepted} of {invalid} invalid requests were not refused with a 4xx: nothing on the way validated them"
        ));
    }
    let (missed, reads) = sum(" (after create)", |r| r.answered - r.ok.min(r.answered));
    if missed > 0 {
        out.push(format!(
            "{missed} of {reads} reads after a create did not find the item they had just created"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(ok: bool, us: u64) -> Answer {
        Answer {
            ok,
            latency_us: us,
            ..Answer::default()
        }
    }

    #[test]
    fn keys_count_merge_and_bound() {
        let mut a = LayerKeys::default();
        let mut b = LayerKeys::default();
        for i in 0..100 {
            a.record(Some("listPets"), Some("key 1"), &answer(true, 1_000 + i));
            b.record(
                Some("listPets"),
                Some("key 2"),
                &answer(i % 10 != 0, 20_000),
            );
        }
        a.merge(&b);
        let op = &a.operations["listPets"];
        assert_eq!((op.answered, op.ok, op.unexpected), (200, 190, 10));
        assert_eq!(a.credentials.len(), 2);
        let (ops, creds) = reports(&["l".to_string()], &[a.clone()]);
        assert_eq!(ops[0].name, "listPets");
        // p50 near 1 ms, p99 near 20 ms, within a bucket.
        assert!((1.0..=1.3).contains(&ops[0].p50_ms), "{}", ops[0].p50_ms);
        assert!((20.0..=25.0).contains(&ops[0].p99_ms), "{}", ops[0].p99_ms);
        assert_eq!(creds.len(), 2);
        for i in 0..(MAX_KEYS + 5) {
            a.record(None, Some(&format!("c{i}")), &answer(true, 10));
        }
        assert_eq!(a.credentials.len(), MAX_KEYS + 1);
        assert!(a.credentials["other"].answered >= 5);
        let mut k = LayerKeys::default();
        k.record(Some("createPet (invalid)"), None, &answer(false, 10));
        k.record(Some("createPet (invalid)"), None, &answer(true, 10));
        k.record(Some("getPet (after create)"), None, &answer(true, 10));
        let (ops, _) = reports(&["l".to_string()], &[k]);
        let f = findings(&ops);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].starts_with("1 of 2 invalid requests"), "{f:?}");
    }
}
