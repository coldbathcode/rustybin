//! Tests only: network partitions between the nodes of an in-process
//! cluster. Every cluster protocol call (join, heartbeat, votes, the
//! announcement, a stale leader's probes) asks [`blocked`] first; nodes in
//! different groups cannot reach each other. A partition refuses calls at
//! once, or drops them (they hang until their timeout, like packets lost on
//! the way). Load to the targets is never affected. Ports identify the
//! nodes (each test server has its own).

use std::collections::BTreeMap;
use std::sync::Mutex;

/// How a partition treats a call across it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    Refuse,
    Drop,
}

static GROUPS: Mutex<BTreeMap<u16, (u32, Cut)>> = Mutex::new(BTreeMap::new());

fn groups() -> std::sync::MutexGuard<'static, BTreeMap<u16, (u32, Cut)>> {
    GROUPS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Put the node listening on `port` into `group` (0 is everyone's default).
pub fn isolate(port: u16, group: u32, cut: Cut) {
    groups().insert(port, (group, cut));
}

/// Back into the default group.
pub fn heal(port: u16) {
    groups().remove(&port);
}

/// Whether a call from the node on `from` to `url` crosses a partition,
/// and how it fails.
pub fn blocked(from: Option<u16>, url: &str) -> Option<Cut> {
    let to = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.port_or_known_default());
    let g = groups();
    let of = |p: Option<u16>| p.and_then(|p| g.get(&p).copied());
    let (a, b) = (of(from), of(to));
    let group = |x: Option<(u32, Cut)>| x.map_or(0, |(g, _)| g);
    if group(a) == group(b) {
        return None;
    }
    // A dropping side drops; otherwise the call is refused.
    let drops = [a, b]
        .iter()
        .any(|x| x.is_some_and(|(_, c)| c == Cut::Drop));
    Some(if drops { Cut::Drop } else { Cut::Refuse })
}
