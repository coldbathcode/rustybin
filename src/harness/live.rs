//! Live tuning (`docs/plans/harness.md`, section 6.5) and the closed loops:
//! what a running run takes without stopping, held in [`Controls`] and
//! applied on top of its scenario the same way on every node. A node
//! evaluates its layers through a [`LiveView`] built from the scenario, the
//! controls and its share.
//!
//! - A [`Change`] (`PATCH .../runs/{run}`): `rate_multiplier` (0 to 10, the
//!   run or one layer), `pause` / `resume`, `hold` (freeze the pattern at its
//!   current point; `false` releases it), `skip_stage` (jump to the next
//!   stage), `set` (`{"<layer>.pattern.<field>": value}`, `then.` reaches the
//!   next pattern of the chain, `"<layer>.pattern"` replaces it), and
//!   `add_layer` / `remove_layer`. Every change is checked like a new
//!   scenario, the caps included ([`Controls::check`]), recorded in the log,
//!   and carried in the cluster snapshot, so it survives a failover.
//! - Times are seconds on the run's clock (since its synchronised start;
//!   the leader's for a cluster): a hold made at `t` freezes the pattern at
//!   the same point on every node, whenever the node hears of it.
//! - The closed loops ([`run_loops`]) read the run's per-second results (all
//!   layers together) as they complete: `target` on `overhead_p99_ms` moves
//!   the layer's rate toward the overhead goal every second; `breakpoint`
//!   judges each plateau when it ends and stops the run at the first that
//!   fails a threshold, with the last good rate in the report's findings;
//!   `limit_probe` (a `limit_test`) halves the range of rates every probe,
//!   by whether the probe's second half saw a rate-limit refusal. On
//!   a single node the engine runs them; in a cluster the leader does, on the
//!   merged results, and the nodes follow the controls.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::engine::Share;
use super::limit_test::{Behaviour, TestKind};
use super::pattern::{seed_of, Compiled, Ctx, Pattern, Shape, SyncMode};
use super::ratelimit::Grid;
use super::report::Bucket;
use super::scenario::{Caps, Layer, Model, Scenario, MAX_LAYERS};

/// Highest rate multiplier.
pub const MAX_MULTIPLIER: f64 = 10.0;
/// Changes kept in a run's log.
pub const MAX_LOG: usize = 64;
/// Findings kept (breakpoints, targets reached).
pub const MAX_FINDINGS: usize = 32;
/// Seconds within 10 percent of the goal before a `target` on overhead
/// counts as settled.
pub const SETTLE_SECONDS: u32 = 3;

/// A live change (`PATCH .../runs/{run}`). Fields left out are unchanged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// The layer the change applies to (by name); the whole run when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Multiply the rate (or users), 0 to 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_multiplier: Option<f64>,
    /// Stop sending (the pattern's clock keeps running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<bool>,
    /// Send again after a pause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<bool>,
    /// Freeze the pattern at its current point (`false` releases it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<bool>,
    /// Jump to the next stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_stage: Option<bool>,
    /// Pattern parameters: `{"<layer>.pattern.<field>": value}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, Value>,
    /// A new layer, started now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_layer: Option<Layer>,
    /// A layer to stop for the rest of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_layer: Option<String>,
}

impl Change {
    /// Whether the change may send more load than before (MCP confirms it).
    pub fn may_raise_load(&self) -> bool {
        self.rate_multiplier.is_some_and(|m| m > 1.0)
            || self.resume == Some(true)
            || self.pause == Some(false)
            || self.add_layer.is_some()
            || !self.set.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.rate_multiplier.is_none()
            && self.pause.is_none()
            && self.resume.is_none()
            && self.hold.is_none()
            && self.skip_stage.is_none()
            && self.set.is_empty()
            && self.add_layer.is_none()
            && self.remove_layer.is_none()
    }

    /// A short text for the log (what changed, not the values of `set`).
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        let on = self
            .layer
            .as_deref()
            .map(|l| format!(" on {l}"))
            .unwrap_or_default();
        if let Some(m) = self.rate_multiplier {
            parts.push(format!("rate x{m}{on}"));
        }
        if self.pause == Some(true) {
            parts.push(format!("pause{on}"));
        }
        if self.resume == Some(true) || self.pause == Some(false) {
            parts.push(format!("resume{on}"));
        }
        match self.hold {
            Some(true) => parts.push(format!("hold{on}")),
            Some(false) => parts.push(format!("release{on}")),
            None => {}
        }
        if self.skip_stage == Some(true) {
            parts.push(format!("skip stage{on}"));
        }
        if !self.set.is_empty() {
            parts.push(format!(
                "set {}",
                self.set.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if let Some(l) = &self.add_layer {
            parts.push(format!("add layer {}", l.name));
        }
        if let Some(l) = &self.remove_layer {
            parts.push(format!("remove layer {l}"));
        }
        parts.join("; ")
    }
}

/// What a closed loop of a layer has seen so far.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoopState {
    /// The first per-second result not looked at yet.
    pub next_bucket: usize,
    /// The stage and plateau being measured (breakpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<(usize, u64)>,
    #[serde(default)]
    pub seconds: u32,
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub p99_ms: f64,
    #[serde(default)]
    pub overhead_p99_ms: f64,
    /// The last plateau that passed (breakpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_good: Option<f64>,
    /// Seconds in a row near the goal (target on overhead).
    #[serde(default)]
    pub settled: u32,
    /// The stage the loop has finished with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_stage: Option<usize>,
    /// `limit_probe`: the range of rates left (the highest without a
    /// refusal, the lowest with one), the probes done, and the current
    /// probe's seconds and refusals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lo: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hi: Option<f64>,
    #[serde(default)]
    pub probes: u32,
    #[serde(default)]
    pub probe_limited: u64,
}

/// The live state of one layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerControl {
    pub multiplier: f64,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub removed: bool,
    /// The pattern runs at `run time - start + shift_secs` (skips and
    /// released holds move it).
    #[serde(default)]
    pub shift_secs: f64,
    /// Frozen at this run time (`hold`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_at: Option<f64>,
    /// The pattern, when `set` changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<Pattern>,
    /// A closed loop's rate (cluster-wide).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default)]
    pub watch: LoopState,
}

impl Default for LayerControl {
    fn default() -> Self {
        Self {
            multiplier: 1.0,
            paused: false,
            removed: false,
            shift_secs: 0.0,
            held_at: None,
            pattern: None,
            value: None,
            watch: LoopState::default(),
        }
    }
}

/// A layer added while the run runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddedLayer {
    pub layer: Layer,
    /// Run time when it was added (its `starts_after` counts from here).
    pub at_secs: f64,
}

/// One entry of the run's change log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at_secs: f64,
    pub version: u64,
    pub what: String,
}

/// What a closed loop found: the last good rate of a breakpoint, the rate
/// a target on overhead settled at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub layer: String,
    /// `breakpoint` or `target`.
    pub kind: String,
    pub at_secs: f64,
    /// The last good rate (breakpoint) or the settled rate (target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// The plateau that failed (breakpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<f64>,
    /// The thresholds it failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    pub summary: String,
}

/// The live state of a run: every node applies it on top of the scenario.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Controls {
    /// Bumped by every change (nodes compare it).
    pub version: u64,
    pub multiplier: f64,
    #[serde(default)]
    pub paused: bool,
    /// One per layer: the scenario's, then the added ones.
    #[serde(default)]
    pub layers: Vec<LayerControl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<AddedLayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<LogEntry>,
    /// A closed loop ended the run (a breakpoint was found).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            version: 0,
            multiplier: 1.0,
            paused: false,
            layers: Vec::new(),
            added: Vec::new(),
            log: Vec::new(),
            stop: None,
            findings: Vec::new(),
        }
    }
}

/// The schema of a live change, served with the scenario schema.
pub fn change_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "description": "A live change of a running run (PATCH .../runs/{run}). Fields left out are unchanged; every change is checked like a new scenario, the caps included, and recorded in the run's log.",
        "properties": {
            "layer": { "type": "string", "description": "The layer the change applies to (by name); the whole run when absent.", "maxLength": 64 },
            "rate_multiplier": { "type": "number", "description": "Multiply the rate (or users), 0 to 10.", "minimum": 0, "maximum": MAX_MULTIPLIER },
            "pause": { "type": "boolean", "description": "Stop sending; the pattern's clock keeps running." },
            "resume": { "type": "boolean", "description": "Send again after a pause." },
            "hold": { "type": "boolean", "description": "Freeze the pattern at its current point; false releases it." },
            "skip_stage": { "type": "boolean", "description": "Jump to the next stage of the pattern." },
            "set": { "type": "object", "description": "Pattern parameters by path: {\"<layer>.pattern.<field>\": value}; then. reaches the next pattern of the chain; \"<layer>.pattern\" replaces the pattern.", "additionalProperties": { "description": "The new value." }, "maxProperties": 32 },
            "add_layer": { "$ref": "#/$defs/layer", "description": "A new layer, started now (its starts_after counts from now)." },
            "remove_layer": { "type": "string", "description": "A layer to stop for the rest of the run.", "maxLength": 64 }
        },
        "additionalProperties": false
    })
}

impl Controls {
    /// The controls of a new run of `s` (normalised).
    pub fn for_scenario(s: &Scenario) -> Self {
        Self {
            layers: vec![LayerControl::default(); s.layers.len()],
            ..Self::default()
        }
    }

    /// The run's layers with their changes: the scenario's (patterns as
    /// `set` left them), then the added ones, each with the run time its
    /// pattern starts at.
    pub fn effective_layers(&self, s: &Scenario) -> Vec<(Layer, f64)> {
        let mut out: Vec<(Layer, f64)> = s
            .layers
            .iter()
            .map(|l| (l.clone(), l.starts_after.map_or(0.0, |d| d.0.as_secs_f64())))
            .collect();
        for a in &self.added {
            let after = a.layer.starts_after.map_or(0.0, |d| d.0.as_secs_f64());
            out.push((a.layer.clone(), a.at_secs + after));
        }
        for (i, (layer, _)) in out.iter_mut().enumerate() {
            if let Some(p) = self.layers.get(i).and_then(|c| c.pattern.clone()) {
                layer.pattern = p;
            }
        }
        out
    }

    /// The run's peak rate as these controls make it: the open layers'
    /// peaks with their multipliers (removed layers left out).
    pub fn peak_rate(&self, s: &Scenario) -> f64 {
        self.effective_layers(s)
            .iter()
            .enumerate()
            .filter(|(i, (l, _))| {
                l.model == Model::Open && !self.layers.get(*i).is_some_and(|c| c.removed)
            })
            .map(|(i, (l, _))| {
                l.pattern.peak()
                    * self.layers.get(i).map_or(1.0, |c| c.multiplier)
                    * self.multiplier
            })
            .sum()
    }

    /// Whether a layer has a closed loop (`target` on overhead, `breakpoint`).
    pub fn has_loops(&self, s: &Scenario) -> bool {
        self.effective_layers(s).iter().any(|(l, _)| {
            l.pattern
                .chain()
                .iter()
                .any(|p| matches!(p, Pattern::Breakpoint { .. }) || p.has_closed_loop())
        })
    }

    /// The names of the run's layers, scenario first.
    pub fn layer_names(&self, s: &Scenario) -> Vec<String> {
        s.layers
            .iter()
            .map(|l| l.name.clone())
            .chain(self.added.iter().map(|a| a.layer.name.clone()))
            .collect()
    }

    fn control(&mut self, i: usize) -> &mut LayerControl {
        while self.layers.len() <= i {
            self.layers.push(LayerControl::default());
        }
        &mut self.layers[i]
    }

    /// The pattern time of layer `i` at run time `t` (`None` before it starts).
    pub fn pattern_time(&self, i: usize, starts_at: f64, t: f64) -> Option<f64> {
        if t < starts_at {
            return None;
        }
        let c = self.layers.get(i);
        let shift = c.map_or(0.0, |c| c.shift_secs);
        Some(match c.and_then(|c| c.held_at) {
            Some(h) => (t.min(h).max(starts_at) - starts_at) + shift,
            None => t - starts_at + shift,
        })
    }

    /// Apply `change` at run time `now`: the new controls (version + 1), or
    /// every problem. The caps are checked by [`Controls::check`].
    pub fn apply(&self, s: &Scenario, change: &Change, now: f64) -> Result<Controls, Vec<String>> {
        let mut c = self.clone();
        let mut problems = Vec::new();
        if change.is_empty() {
            return Err(vec!["the change is empty: nothing to do".into()]);
        }
        let names = c.layer_names(s);
        let target: Vec<usize> = match &change.layer {
            Some(name) => match names.iter().position(|n| n == name) {
                Some(i) => vec![i],
                None => return Err(vec![format!("layer: no layer {name:?} in the run")]),
            },
            None => (0..names.len()).collect(),
        };
        let one_layer = change.layer.is_some();
        if let Some(m) = change.rate_multiplier {
            if !m.is_finite() || !(0.0..=MAX_MULTIPLIER).contains(&m) {
                problems.push(format!("rate_multiplier: 0 to {MAX_MULTIPLIER}"));
            } else if one_layer {
                c.control(target[0]).multiplier = m;
            } else {
                c.multiplier = m;
            }
        }
        let pause = match (change.pause, change.resume) {
            (Some(true), Some(true)) => {
                problems.push("pause and resume: one or the other".into());
                None
            }
            (Some(p), _) => Some(p),
            (None, Some(true)) => Some(false),
            (None, _) => None,
        };
        if let Some(p) = pause {
            if one_layer {
                c.control(target[0]).paused = p;
            } else {
                c.paused = p;
                for i in &target {
                    c.control(*i).paused = false;
                }
            }
        }
        let effective = c.effective_layers(s);
        if let Some(hold) = change.hold {
            for &i in &target {
                let starts_at = effective[i].1;
                let lc = c.control(i);
                match (hold, lc.held_at) {
                    (true, None) => lc.held_at = Some(now.max(starts_at)),
                    (false, Some(h)) => {
                        lc.shift_secs -= (now - h).max(0.0);
                        lc.held_at = None;
                    }
                    _ => {}
                }
            }
        }
        if change.skip_stage == Some(true) {
            for &i in &target {
                let (layer, starts_at) = &effective[i];
                let Some(pt) = c.pattern_time(i, *starts_at, now) else {
                    continue;
                };
                let next = layer.pattern.compile().next_stage_start(pt);
                c.control(i).shift_secs += (next - pt).max(0.0);
            }
        }
        for (path, value) in &change.set {
            match set_path(&c, s, path, value) {
                Ok((i, pattern)) => c.control(i).pattern = Some(pattern),
                Err(e) => problems.push(e),
            }
        }
        if let Some(layer) = &change.add_layer {
            if names.contains(&layer.name) {
                problems.push(format!("add_layer: a layer named {:?} exists", layer.name));
            } else if names.len() >= MAX_LAYERS {
                problems.push(format!("add_layer: a run has at most {MAX_LAYERS} layers"));
            } else if layer
                .identities
                .as_ref()
                .is_some_and(|i| i.oauth.is_some() || !i.source_addresses.is_empty())
            {
                problems.push(
                    "add_layer: OAuth clients and source addresses are set up when the run starts; put the layer in the scenario".into(),
                );
            } else {
                let mut layer = layer.clone();
                super::scenario::fill_limit_tests(&mut layer);
                c.added.push(AddedLayer {
                    layer,
                    at_secs: now.max(0.0),
                });
                c.control(names.len());
            }
        }
        if let Some(name) = &change.remove_layer {
            match names.iter().position(|n| n == name) {
                Some(i) => c.control(i).removed = true,
                None => problems.push(format!("remove_layer: no layer {name:?} in the run")),
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        c.version += 1;
        if c.log.len() >= MAX_LOG {
            c.log.remove(0);
        }
        c.log.push(LogEntry {
            at_secs: (now * 1000.0).round() / 1000.0,
            version: c.version,
            what: change.describe(),
        });
        Ok(c)
    }

    /// Check the run as these controls make it, against `caps` for this
    /// node's `share`: the layers as a scenario (patterns, models, names),
    /// and the peak rate and users with the multipliers.
    pub fn check(&self, s: &Scenario, caps: &Caps, share: f64) -> Vec<String> {
        let share = if share.is_finite() && share > 0.0 {
            share.min(1.0)
        } else {
            1.0
        };
        let layers = self.effective_layers(s);
        let mut eff = s.clone();
        eff.layers = layers
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.layers.get(*i).is_some_and(|c| c.removed))
            .map(|(_, (l, _))| l.clone())
            .collect();
        // The structure, with the rate caps left to the check below.
        let loose = Caps {
            max_rate: u64::MAX / 4,
            max_inflight: usize::MAX / 4,
            max_duration_secs: caps.max_duration_secs,
        };
        let mut problems: Vec<String> = if eff.layers.is_empty() {
            Vec::new()
        } else {
            eff.validate_scaled(&loose, 1.0)
                .into_iter()
                .filter(|p| !p.starts_with("layers: the run would last"))
                .collect()
        };
        let node_rate = s
            .scale
            .max_rate_per_node
            .map_or(caps.max_rate, |r| r.min(caps.max_rate));
        let mut rate = 0.0;
        for (i, (layer, _)) in layers.iter().enumerate() {
            let lc = self.layers.get(i).cloned().unwrap_or_default();
            if lc.removed {
                continue;
            }
            let peak = layer.pattern.peak() * lc.multiplier * self.multiplier;
            match layer.model {
                Model::Open => rate += peak,
                Model::Closed => {
                    if peak * share > caps.max_inflight as f64 {
                        problems.push(format!(
                            "{}: {:.0} users is over this node's in-flight cap of {}",
                            layer.name,
                            peak * share,
                            caps.max_inflight
                        ));
                    }
                    if peak > f64::from(s.scale.virtual_clients) {
                        problems.push(format!(
                            "{}: {peak:.0} users is over scale.virtual_clients ({})",
                            layer.name, s.scale.virtual_clients
                        ));
                    }
                }
            }
        }
        if rate * share > node_rate as f64 + 1e-9 {
            problems.push(format!(
                "the peak rates with the multipliers add up to {:.0} requests per second{}, over the cap of {node_rate}",
                rate * share,
                if share < 1.0 {
                    format!(" (this node's share of {:.1}%)", share * 100.0)
                } else {
                    String::new()
                }
            ));
        }
        problems
    }
}

/// `set` one path: the layer index and its new pattern.
fn set_path(
    c: &Controls,
    s: &Scenario,
    path: &str,
    value: &Value,
) -> Result<(usize, Pattern), String> {
    let Some((layer, rest)) = path.split_once(".pattern") else {
        return Err(format!(
            "set.{path}: use <layer>.pattern or <layer>.pattern.<field> (then. for the next pattern)"
        ));
    };
    let names = c.layer_names(s);
    let Some(i) = names.iter().position(|n| n == layer) else {
        return Err(format!("set.{path}: no layer {layer:?} in the run"));
    };
    let current = c.effective_layers(s)[i].0.pattern.clone();
    let mut json = serde_json::to_value(&current).map_err(|e| format!("set.{path}: {e}"))?;
    let rest = rest.trim_start_matches('.');
    if rest.is_empty() {
        json = value.clone();
    } else {
        let parts: Vec<&str> = rest.split('.').collect();
        let mut node = &mut json;
        for (k, part) in parts.iter().enumerate() {
            let last = k + 1 == parts.len();
            let Value::Object(map) = node else {
                return Err(format!("set.{path}: {part} is not inside a pattern"));
            };
            if last {
                if *part == "type" {
                    return Err(format!(
                        "set.{path}: change the type by replacing the pattern (<layer>.pattern)"
                    ));
                }
                map.insert((*part).to_string(), value.clone());
                break;
            }
            if *part != "then" {
                return Err(format!(
                    "set.{path}: only then. may come between pattern and the field"
                ));
            }
            node = map
                .get_mut("then")
                .ok_or_else(|| format!("set.{path}: the pattern has no then"))?;
        }
    }
    let pattern: Pattern = serde_json::from_value(json).map_err(|e| format!("set.{path}: {e}"))?;
    let mut problems = Vec::new();
    pattern.validate(&format!("set.{path}"), &mut problems);
    if problems.is_empty() {
        Ok((i, pattern))
    } else {
        Err(problems.join("; "))
    }
}

/// What a layer needs beyond its definition, set up on the node when the
/// run starts (or when a layer is added): a mock source's requests and the
/// layer's identities.
#[derive(Default)]
pub struct LayerExtras {
    pub mock: Option<Arc<super::mocksource::MockPlan>>,
    pub identities: Option<Arc<super::identity::LayerIdentities>>,
}

impl std::fmt::Debug for LayerExtras {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerExtras")
            .field("mock", &self.mock.as_ref().map(|m| &m.mock))
            .field("identities", &self.identities.as_ref().map(|i| i.size))
            .finish()
    }
}

/// One layer as a node evaluates it.
#[derive(Clone, Debug)]
pub struct LiveLayer {
    pub def: Layer,
    pub compiled: Compiled,
    /// Run time the layer's pattern starts at.
    pub starts_at: f64,
    pub control: LayerControl,
    pub ctx: Ctx,
    /// Cumulative weights of the layer's request templates.
    pub weights: Vec<u32>,
    /// The statuses that count as a rate-limit refusal.
    pub limited: Vec<u16>,
    /// What the engine adds while a test runs, by pattern of the chain.
    pub behaviours: Vec<Option<Behaviour>>,
    /// The rate-limit analysis grid (runs that analyse rate limits).
    pub grid: Option<Grid>,
    /// The header naming the gateway node (`expect.node_header`).
    pub node_header: Option<String>,
    /// The node's mock plan and identities for the layer.
    pub extras: Option<Arc<LayerExtras>>,
}

/// A layer's level at an instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    /// The layer has not started yet.
    Waiting,
    /// The rate (or users) to run now, this node's share not applied; 0
    /// while paused.
    Run(f64),
    /// The pattern is over (or the layer was removed).
    Ended,
}

/// The run as the scheduler evaluates it, rebuilt when the controls change.
#[derive(Clone, Debug)]
pub struct LiveView {
    pub version: u64,
    pub layers: Vec<LiveLayer>,
    pub multiplier: f64,
    pub paused: bool,
    pub stop: Option<String>,
    /// A layer asks for the rate-limit analysis: every layer is counted
    /// (the others for collateral damage).
    pub analysed: bool,
}

fn cumulative(weights: impl Iterator<Item = u32>) -> Vec<u32> {
    let mut acc = 0u32;
    weights
        .map(|w| {
            acc = acc.saturating_add(w.max(1));
            acc
        })
        .collect()
}

impl LiveView {
    /// The view of `s` with `controls`, for the node `node_id` sending
    /// `share` of the run `run_id` (the jitter seed and the phase).
    pub fn build(
        s: &Scenario,
        controls: &Controls,
        run_id: &str,
        node_id: &str,
        share: Share,
    ) -> Self {
        let layers = controls
            .effective_layers(s)
            .into_iter()
            .enumerate()
            .map(|(i, (def, starts_at))| {
                let compiled = def.pattern.compile();
                let node = compiled.sync == SyncMode::Node;
                let seed = if node {
                    seed_of(&[run_id, &def.name, node_id])
                } else {
                    seed_of(&[run_id, &def.name])
                };
                let weights = cumulative(def.source.templates().iter().map(|t| t.weight));
                LiveLayer {
                    control: controls.layers.get(i).cloned().unwrap_or_default(),
                    ctx: Ctx {
                        seed,
                        phase: if node { share.from } else { 0.0 },
                        wall: 0.0,
                    },
                    limited: def.limited_statuses(),
                    behaviours: def
                        .pattern
                        .limit_tests()
                        .into_iter()
                        .map(|t| t.map(|t| t.behaviour()))
                        .collect(),
                    grid: None,
                    node_header: def
                        .expect
                        .as_ref()
                        .and_then(|e| e.node_header.as_ref())
                        .map(|h| h.to_ascii_lowercase()),
                    extras: None,
                    def,
                    compiled,
                    starts_at,
                    weights,
                }
            })
            .collect::<Vec<LiveLayer>>();
        let analysed = layers.iter().any(|l| l.def.analysed());
        Self {
            version: controls.version,
            layers,
            multiplier: controls.multiplier,
            paused: controls.paused,
            stop: controls.stop.clone(),
            analysed,
        }
    }

    /// Put the view on the wall clock: `wall` is the Unix time (seconds) of
    /// the run's start. Stages aligned to the wall clock's windows shift to
    /// them, and the layers get their rate-limit grids.
    pub fn with_wall(mut self, wall: Option<f64>) -> Self {
        let Some(wall) = wall else {
            return self;
        };
        let analysed = self.analysed;
        for l in &mut self.layers {
            l.ctx.wall = wall + l.starts_at;
            if analysed {
                l.grid = Some(super::ratelimit::grid_for(&l.def, l.starts_at, wall));
            }
        }
        self
    }

    /// Give the layers their extras (by layer index).
    pub fn with_extras(mut self, extras: &[Option<Arc<LayerExtras>>]) -> Self {
        for (l, e) in self.layers.iter_mut().zip(extras) {
            l.extras = e.clone();
        }
        self
    }

    /// The rate-limit test running in layer `i` at run time `t`: the
    /// pattern of the chain it comes from and what the engine adds.
    pub fn behaviour(&self, i: usize, t: f64) -> Option<(usize, &Behaviour)> {
        let l = self.layers.get(i)?;
        let (stage, _) = self.position(i, t)?;
        let chain = l.compiled.stages.get(stage)?.pattern;
        l.behaviours.get(chain)?.as_ref().map(|b| (chain, b))
    }

    /// The pattern time of layer `i` at run time `t`.
    pub fn pattern_time(&self, i: usize, t: f64) -> Option<f64> {
        let l = self.layers.get(i)?;
        if t < l.starts_at {
            return None;
        }
        Some(match l.control.held_at {
            Some(h) => (t.min(h).max(l.starts_at) - l.starts_at) + l.control.shift_secs,
            None => t - l.starts_at + l.control.shift_secs,
        })
    }

    /// The level of layer `i` at run time `t`.
    pub fn level(&self, i: usize, t: f64) -> Level {
        let Some(l) = self.layers.get(i) else {
            return Level::Ended;
        };
        if l.control.removed {
            return Level::Ended;
        }
        let Some(pt) = self.pattern_time(i, t) else {
            return Level::Waiting;
        };
        let Some(stage) = l.compiled.stage_at(pt) else {
            return Level::Ended;
        };
        let value = match (&stage.shape, l.control.value) {
            (Shape::Closed { .. }, Some(v)) => v,
            _ => l.compiled.value_at(pt, &l.ctx).unwrap_or(0.0),
        };
        if self.paused || l.control.paused {
            return Level::Run(0.0);
        }
        Level::Run((value * l.control.multiplier * self.multiplier).max(0.0))
    }

    /// The next run time after `t` where layer `i`'s level may jump (an
    /// edge of its pattern), or infinity (held, or no edge ahead).
    pub fn next_boundary(&self, i: usize, t: f64) -> f64 {
        let Some(l) = self.layers.get(i) else {
            return f64::INFINITY;
        };
        if l.control.held_at.is_some() {
            return f64::INFINITY;
        }
        let Some(pt) = self.pattern_time(i, t) else {
            return l.starts_at;
        };
        let edge = l.compiled.next_boundary(pt, &l.ctx);
        if edge.is_finite() {
            t + (edge - pt)
        } else {
            f64::INFINITY
        }
    }

    /// The stage and part (plateau) of layer `i` at run time `t`.
    pub fn position(&self, i: usize, t: f64) -> Option<(usize, u64)> {
        let l = self.layers.get(i)?;
        if l.control.removed {
            return None;
        }
        l.compiled.position(self.pattern_time(i, t)?)
    }
}

/// Most stage marks a run keeps.
pub const MAX_STAGE_MARKS: usize = 1000;

/// From `at_secs` (run time) layer `layer` ran `stage` / `part` (`None`:
/// it was not running: done or removed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StageMark {
    pub layer: usize,
    pub at_secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<usize>,
    #[serde(default)]
    pub part: u64,
    /// The stage's pattern type.
    #[serde(default)]
    pub kind: String,
    /// The part's nominal rate (or users).
    #[serde(default)]
    pub target: f64,
}

/// Where each layer's pattern was as the run went, noted with the
/// controls in effect at the time (a hold stretches a part, a skip cuts
/// one short, `set` starts a new one), so the report's stages and the
/// closed loops see the run as it ran. Bounded: once full, nothing is
/// noted past `full_at`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StageLog {
    #[serde(default)]
    pub marks: Vec<StageMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_at: Option<f64>,
}

impl StageLog {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty() && self.full_at.is_none()
    }

    /// Note where every layer of `view` is at run time `t`.
    pub fn note(&mut self, view: &LiveView, t: f64) {
        if self.full_at.is_some() {
            return;
        }
        for (i, l) in view.layers.iter().enumerate() {
            let pos = view.position(i, t);
            let stage = pos.and_then(|(s, _)| l.compiled.stages.get(s));
            // A closed loop's rate moves every second: its start stands in.
            let target = match (stage, pos) {
                (Some(st), Some((_, p))) => match &st.shape {
                    Shape::Closed { initial, .. } => *initial,
                    _ => st.segment_value(p),
                },
                _ => 0.0,
            };
            let last = self.marks.iter().rev().find(|m| m.layer == i);
            let same = match (last, pos) {
                (Some(m), Some((s, p))) => m.stage == Some(s) && m.part == p && m.target == target,
                (Some(m), None) => m.stage.is_none(),
                // Not started yet: nothing to note.
                (None, None) => true,
                (None, Some(_)) => false,
            };
            if same {
                continue;
            }
            if self.marks.len() >= MAX_STAGE_MARKS {
                self.full_at = Some(t);
                return;
            }
            self.marks.push(StageMark {
                layer: i,
                at_secs: t,
                stage: pos.map(|(s, _)| s),
                part: pos.map_or(0, |(_, p)| p),
                kind: stage.map_or("", |s| s.kind).to_string(),
                target,
            });
        }
    }

    /// The run stops sending at run time `t`: no layer is in a stage after
    /// it (the drain, the collection).
    pub fn end(&mut self, layers: usize, t: f64) {
        if self.full_at.is_some() {
            return;
        }
        for i in 0..layers {
            let running = self
                .marks
                .iter()
                .rev()
                .find(|m| m.layer == i)
                .is_some_and(|m| m.stage.is_some());
            if !running {
                continue;
            }
            if self.marks.len() >= MAX_STAGE_MARKS {
                self.full_at = Some(t);
                return;
            }
            self.marks.push(StageMark {
                layer: i,
                at_secs: t,
                stage: None,
                part: 0,
                kind: String::new(),
                target: 0.0,
            });
        }
    }

    /// The mark of `layer` in effect at run time `t` (`None`: not noted,
    /// before the layer started or past `full_at`).
    pub fn at(&self, layer: usize, t: f64) -> Option<(usize, &StageMark)> {
        if self.full_at.is_some_and(|f| t >= f) {
            return None;
        }
        self.marks
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.layer == layer && m.at_secs <= t)
    }

    /// The stage and part of `layer` at `t` as noted, else as `view` says
    /// now.
    pub fn position(&self, view: &LiveView, layer: usize, t: f64) -> Option<(usize, u64)> {
        match self.at(layer, t) {
            Some((_, m)) => m.stage.map(|s| (s, m.part)),
            None => view.position(layer, t),
        }
    }
}

/// Run the closed loops of every layer over the per-second results not
/// looked at yet (`timeline`, the run's complete seconds). `last` judges
/// what is still open (the run ended). Returns whether the controls changed.
pub fn run_loops(
    c: &mut Controls,
    view: &LiveView,
    stages: &StageLog,
    timeline: &[Bucket],
    last: bool,
) -> bool {
    let mut changed = false;
    for (i, l) in view.layers.iter().enumerate() {
        let looping = l
            .compiled
            .stages
            .iter()
            .any(|s| matches!(s.shape, Shape::Closed { .. } | Shape::Growth { .. }));
        if !looping || c.stop.is_some() {
            continue;
        }
        let chain = l.def.pattern.chain();
        let mut lc = c.layers.get(i).cloned().unwrap_or_default();
        let mut findings = Vec::new();
        let mut stop = None;
        let start = lc.watch.next_bucket.min(timeline.len());
        for (bi, b) in timeline.iter().enumerate().skip(start) {
            lc.watch.next_bucket = bi + 1;
            let mid = b.t_secs + b.width_secs / 2.0;
            let pos = stages.position(view, i, mid);
            let stage = pos.and_then(|(si, _)| l.compiled.stages.get(si));
            // A breakpoint plateau that is over: judge it.
            if let Some(part) = lc.watch.part {
                if pos != Some(part) {
                    if let Some(f) = judge(&l.def.name, l, &chain, &mut lc.watch, part, mid) {
                        if !f.failed.is_empty() {
                            stop = Some(f.summary.clone());
                        }
                        findings.push(f);
                    }
                    if stop.is_some() {
                        break;
                    }
                }
            }
            let (Some(stage), Some((si, seg))) = (stage, pos) else {
                continue;
            };
            if lc.watch.done_stage == Some(si) {
                continue;
            }
            match &stage.shape {
                Shape::Growth { .. } => {
                    if lc.watch.part != Some((si, seg)) {
                        // A new plateau; its first second settles.
                        lc.watch.part = Some((si, seg));
                        lc.watch.seconds = 0;
                        lc.watch.completed = 0;
                        lc.watch.errors = 0;
                        lc.watch.p99_ms = 0.0;
                        lc.watch.overhead_p99_ms = 0.0;
                        continue;
                    }
                    lc.watch.seconds += 1;
                    lc.watch.completed += b.completed;
                    lc.watch.errors += b.errors;
                    lc.watch.p99_ms = lc.watch.p99_ms.max(b.p99_ms);
                    lc.watch.overhead_p99_ms = lc.watch.overhead_p99_ms.max(b.overhead_p99_ms);
                }
                Shape::Closed { initial, .. } if matches!(chain.get(stage.pattern), Some(Pattern::LimitTest(t)) if t.test == TestKind::LimitProbe) =>
                {
                    let Some(Pattern::LimitTest(t)) = chain.get(stage.pattern) else {
                        continue;
                    };
                    let every = (t.every_secs().round() as u32).max(2);
                    // The first half of a probe settles (a window the last
                    // probe used up); the second half decides.
                    let settle = every / 2;
                    let w = &mut lc.watch;
                    if w.hi.is_none() {
                        w.lo = Some(t.probe_min());
                        w.hi = Some(t.probe_max());
                    }
                    w.seconds += 1;
                    if w.seconds > settle {
                        w.probe_limited += b.limited;
                    }
                    if w.seconds < every {
                        continue;
                    }
                    let current = lc.value.unwrap_or(*initial);
                    if w.probe_limited > 0 {
                        w.hi = Some(current);
                    } else {
                        w.lo = Some(current);
                    }
                    w.probes += 1;
                    w.seconds = 0;
                    w.probe_limited = 0;
                    let (lo, hi) = (w.lo.unwrap_or(0.0), w.hi.unwrap_or(0.0));
                    if w.probes >= t.steps() {
                        w.done_stage = Some(si);
                        lc.value = Some(lo);
                        findings.push(Finding {
                            layer: l.def.name.clone(),
                            kind: "limit_probe".into(),
                            at_secs: mid,
                            rate: Some(lo),
                            failed_at: Some(hi),
                            failed: Vec::new(),
                            summary: format!(
                                "limit_probe of {}: no refusal up to {lo:.2} requests per second, refusals from {hi:.2} (after {} probes)",
                                l.def.name, w.probes
                            ),
                        });
                    } else {
                        lc.value = Some(((lo + hi) / 2.0 * 100.0).round() / 100.0);
                    }
                }
                Shape::Closed { initial, max } => {
                    let goal = match chain.get(stage.pattern) {
                        Some(Pattern::Target {
                            overhead_p99_ms: Some(g),
                            ..
                        }) => *g,
                        _ => continue,
                    };
                    let current = lc.value.unwrap_or(*initial);
                    // Only answers tagged by a Rustybin server carry the
                    // overhead.
                    if b.upstream == 0 || b.overhead_p99_ms <= 0.0 {
                        continue;
                    }
                    let measured = b.overhead_p99_ms;
                    let factor = (goal / measured).clamp(0.8, 1.25);
                    let next = (current * factor).clamp(1.0, max.max(1.0));
                    lc.value = Some((next * 100.0).round() / 100.0);
                    if (measured - goal).abs() <= goal * 0.1 {
                        lc.watch.settled += 1;
                    } else {
                        lc.watch.settled = 0;
                    }
                    if lc.watch.settled == SETTLE_SECONDS {
                        findings.push(Finding {
                            layer: l.def.name.clone(),
                            kind: "target".into(),
                            at_secs: mid,
                            rate: Some(current),
                            failed_at: None,
                            failed: Vec::new(),
                            summary: format!(
                                "{}: the gateway overhead p99 settled at {measured:.2} ms (goal {goal} ms) at {current:.1} requests per second",
                                l.def.name
                            ),
                        });
                    }
                }
                _ => {}
            }
        }
        if last && stop.is_none() {
            if let Some(part) = lc.watch.part {
                if let Some(f) = judge(&l.def.name, l, &chain, &mut lc.watch, part, f64::MAX) {
                    if !f.failed.is_empty() {
                        stop = Some(f.summary.clone());
                    }
                    findings.push(f);
                }
            }
        }
        if lc != c.layers.get(i).cloned().unwrap_or_default()
            || !findings.is_empty()
            || stop.is_some()
        {
            changed = true;
            if c.layers.len() <= i {
                c.layers.resize(i + 1, LayerControl::default());
            }
            c.layers[i] = lc;
            for f in findings {
                if c.findings.len() < MAX_FINDINGS {
                    c.findings.push(f);
                }
            }
            if stop.is_some() {
                c.stop = stop;
            }
        }
    }
    changed
}

/// Judge a breakpoint plateau that ended. A finding when it failed (the
/// run stops), or when it was the last one (no breakpoint up to `max`).
fn judge(
    layer: &str,
    l: &LiveLayer,
    chain: &[&Pattern],
    w: &mut LoopState,
    (si, seg): (usize, u64),
    at: f64,
) -> Option<Finding> {
    w.part = None;
    let stage = l.compiled.stages.get(si)?;
    let Shape::Growth { plateaus, .. } = &stage.shape else {
        return None;
    };
    let Some(Pattern::Breakpoint { until, .. }) = chain.get(stage.pattern) else {
        return None;
    };
    let rate = stage.segment_value(seg);
    let mut failed = Vec::new();
    if w.seconds > 0 {
        if let Some(limit) = until.error_rate {
            let r = if w.completed == 0 {
                0.0
            } else {
                w.errors as f64 / w.completed as f64
            };
            if r > limit {
                failed.push(format!("error_rate {r:.4} over {limit}"));
            }
        }
        if let Some(limit) = until.latency_p99_ms {
            if w.p99_ms > limit {
                failed.push(format!("latency_p99_ms {:.2} over {limit}", w.p99_ms));
            }
        }
        if let Some(limit) = until.overhead_p99_ms {
            if w.overhead_p99_ms > limit {
                failed.push(format!(
                    "overhead_p99_ms {:.2} over {limit}",
                    w.overhead_p99_ms
                ));
            }
        }
    }
    let at = if at == f64::MAX { 0.0 } else { at };
    if !failed.is_empty() {
        w.done_stage = Some(si);
        let summary = match w.last_good {
            Some(good) => format!(
                "breakpoint of {layer}: {rate:.1} requests per second failed ({}); the last good rate is {good:.1}",
                failed.join(", ")
            ),
            None => format!(
                "breakpoint of {layer}: the first plateau ({rate:.1} requests per second) failed already ({})",
                failed.join(", ")
            ),
        };
        return Some(Finding {
            layer: layer.into(),
            kind: "breakpoint".into(),
            at_secs: at,
            rate: w.last_good,
            failed_at: Some(rate),
            failed,
            summary,
        });
    }
    if w.seconds > 0 {
        w.last_good = Some(rate);
    }
    if seg + 1 >= *plateaus {
        w.done_stage = Some(si);
        return Some(Finding {
            layer: layer.into(),
            kind: "breakpoint".into(),
            at_secs: at,
            rate: w.last_good,
            failed_at: None,
            failed: Vec::new(),
            summary: format!(
                "breakpoint of {layer}: no plateau failed up to {rate:.1} requests per second"
            ),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::scenario::Dur;

    fn scenario(yaml_layers: &str) -> Scenario {
        let mut s = Scenario::parse(&format!(
            "name: live\ntarget: http://127.0.0.1:1\nscale: {{ virtual_clients: 10 }}\nlayers:\n{yaml_layers}"
        ))
        .unwrap();
        s.normalise();
        s
    }

    fn two_layers() -> Scenario {
        scenario(
            "  - name: a\n    source: { request: { path: / } }\n    pattern: { type: ramp, from: 10, to: 110, over: 10s, then: { type: constant, rate: 110, for: 10s } }\n  - name: b\n    source: { request: { path: / } }\n    pattern: { type: constant, rate: 50, for: 20s }\n",
        )
    }

    fn change(json: &str) -> Change {
        serde_json::from_str(json).unwrap()
    }

    fn view(s: &Scenario, c: &Controls) -> LiveView {
        LiveView::build(s, c, "run", "node", Share::default())
    }

    #[test]
    fn multipliers_pause_and_resume() {
        let s = two_layers();
        let c = Controls::for_scenario(&s);
        assert_eq!(view(&s, &c).level(0, 5.0), Level::Run(60.0));
        let c = c
            .apply(&s, &change(r#"{"rate_multiplier": 2}"#), 1.0)
            .unwrap();
        assert_eq!(view(&s, &c).level(1, 5.0), Level::Run(100.0));
        let c = c
            .apply(
                &s,
                &change(r#"{"layer": "b", "rate_multiplier": 0.5}"#),
                2.0,
            )
            .unwrap();
        assert_eq!(view(&s, &c).level(1, 5.0), Level::Run(50.0));
        let c = c
            .apply(&s, &change(r#"{"layer": "a", "pause": true}"#), 3.0)
            .unwrap();
        assert_eq!(view(&s, &c).level(0, 5.0), Level::Run(0.0));
        let c = c.apply(&s, &change(r#"{"resume": true}"#), 4.0).unwrap();
        assert_eq!(view(&s, &c).level(0, 5.0), Level::Run(120.0));
        assert_eq!(c.version, 4);
        assert_eq!(c.log.len(), 4);
        assert_eq!(c.log[1].what, "rate x0.5 on b");
        assert!(c
            .apply(&s, &change(r#"{"rate_multiplier": 11}"#), 5.0)
            .is_err());
        assert!(c
            .apply(&s, &change(r#"{"layer": "zz", "pause": true}"#), 5.0)
            .is_err());
        assert!(c.apply(&s, &change("{}"), 5.0).is_err());
    }

    #[test]
    fn hold_release_and_skip_move_the_pattern_clock() {
        let s = two_layers();
        let c = Controls::for_scenario(&s);
        // Held at 4 s: the ramp stays at 50 whatever the time.
        let c = c
            .apply(&s, &change(r#"{"layer": "a", "hold": true}"#), 4.0)
            .unwrap();
        let v = view(&s, &c);
        assert_eq!(v.level(0, 9.0), Level::Run(50.0));
        assert_eq!(v.level(0, 60.0), Level::Run(50.0));
        // Released at 9 s: it carries on from 4 s.
        let c = c
            .apply(&s, &change(r#"{"layer": "a", "hold": false}"#), 9.0)
            .unwrap();
        let v = view(&s, &c);
        assert_eq!(v.pattern_time(0, 10.0), Some(5.0));
        assert_eq!(v.level(0, 10.0), Level::Run(60.0));
        // Skip at 10 s (pattern time 5): the constant stage starts now.
        let c = c
            .apply(&s, &change(r#"{"layer": "a", "skip_stage": true}"#), 10.0)
            .unwrap();
        let v = view(&s, &c);
        assert_eq!(v.pattern_time(0, 10.0), Some(10.0));
        assert_eq!(v.level(0, 10.5), Level::Run(110.0));
        assert_eq!(v.level(0, 20.0), Level::Ended);
        assert_eq!(
            v.level(1, 10.0),
            Level::Run(50.0),
            "the other layer is untouched"
        );
    }

    #[test]
    fn set_add_and_remove_layers() {
        let s = two_layers();
        let c = Controls::for_scenario(&s);
        let c = c
            .apply(
                &s,
                &change(r#"{"set": {"a.pattern.then.rate": 200, "b.pattern.rate": 70}}"#),
                1.0,
            )
            .unwrap();
        let v = view(&s, &c);
        assert_eq!(v.level(0, 15.0), Level::Run(200.0));
        assert_eq!(v.level(1, 15.0), Level::Run(70.0));
        let c = c
            .apply(
                &s,
                &change(r#"{"set": {"b.pattern": {"type": "pulse", "high": 20, "low": 0, "on": "1s", "off": "1s", "cycles": 5}}}"#),
                2.0,
            )
            .unwrap();
        assert_eq!(view(&s, &c).level(1, 2.5), Level::Run(20.0));
        for bad in [
            r#"{"set": {"a.pattern.type": "ramp"}}"#,
            r#"{"set": {"a.pattern.rate": -1}}"#,
            r#"{"set": {"zz.pattern.rate": 1}}"#,
            r#"{"set": {"a.rate": 1}}"#,
            r#"{"set": {"b.pattern.then.rate": 1}}"#,
        ] {
            assert!(c.apply(&s, &change(bad), 3.0).is_err(), "{bad}");
        }
        let c = c
            .apply(
                &s,
                &change(r#"{"add_layer": {"name": "late", "source": {"request": {"path": "/x"}}, "pattern": {"type": "constant", "rate": 5, "for": "2s"}}}"#),
                6.0,
            )
            .unwrap();
        let v = view(&s, &c);
        assert_eq!(v.layers.len(), 3);
        assert_eq!(v.level(2, 5.0), Level::Waiting);
        assert_eq!(v.level(2, 7.0), Level::Run(5.0));
        assert_eq!(v.level(2, 8.5), Level::Ended);
        assert!(c
            .apply(&s, &change(r#"{"add_layer": {"name": "a", "source": {}, "pattern": {"type": "constant", "rate": 1, "for": "1s"}}}"#), 7.0)
            .is_err());
        let c = c
            .apply(&s, &change(r#"{"remove_layer": "b"}"#), 8.0)
            .unwrap();
        assert_eq!(view(&s, &c).level(1, 9.0), Level::Ended);
        assert_eq!(c.layer_names(&s), vec!["a", "b", "late"]);
    }

    #[test]
    fn an_added_limit_test_takes_its_layers_expect() {
        let s = two_layers();
        let c = Controls::for_scenario(&s);
        let next = c
            .apply(
                &s,
                &change(
                    r#"{"add_layer": {"name": "heavy", "source": {"request": {"path": "/x"}}, "pattern": {"type": "limit_test", "test": "over_limit", "for": "2s"}, "expect": {"limit": 5, "window": "1s"}}}"#,
                ),
                1.0,
            )
            .unwrap();
        let problems = next.check(&s, &Caps::default(), 1.0);
        assert!(problems.is_empty(), "{problems:?}");
        let layers = next.effective_layers(&s);
        match &layers.last().unwrap().0.pattern {
            Pattern::LimitTest(t) => assert_eq!((t.limit, t.window), (Some(5), Some(Dur::secs(1)))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn caps_are_checked_with_the_multipliers() {
        let s = two_layers();
        let caps = Caps {
            max_rate: 400,
            max_inflight: 100,
            max_duration_secs: 3600,
        };
        let c = Controls::for_scenario(&s);
        assert!(c.check(&s, &caps, 1.0).is_empty());
        // 110 + 50 at x3 is 480, over 400; half of it on a node of two fits.
        let c = c
            .apply(&s, &change(r#"{"rate_multiplier": 3}"#), 1.0)
            .unwrap();
        let p = c.check(&s, &caps, 1.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("480"), "{p:?}");
        assert!(c.check(&s, &caps, 0.5).is_empty());
        // A removed layer no longer counts.
        let c = c
            .apply(&s, &change(r#"{"remove_layer": "b"}"#), 2.0)
            .unwrap();
        assert!(
            c.check(&s, &caps, 1.0).is_empty(),
            "{:?}",
            c.check(&s, &caps, 1.0)
        );
    }

    fn bucket(t: f64, completed: u64, errors: u64, p99: f64, overhead: f64) -> Bucket {
        Bucket {
            t_secs: t,
            width_secs: 1.0,
            completed,
            errors,
            upstream: completed,
            p99_ms: p99,
            overhead_p99_ms: overhead,
            ..Bucket::default()
        }
    }

    #[test]
    fn breakpoint_stops_at_the_first_failing_plateau() {
        let s = scenario(
            "  - name: grow\n    source: { request: { path: / } }\n    pattern: { type: breakpoint, start: 100, growth: 1, every: 3s, max: 1600, until: { error_rate: 0.05 } }\n",
        );
        let mut c = Controls::for_scenario(&s);
        let v = view(&s, &c);
        // Plateaus 100, 200, 400, 800, 1600 of 3 s each; errors from 800.
        let timeline: Vec<Bucket> = (0..12)
            .map(|t| {
                let rate = [100, 200, 400, 800][t / 3] as u64;
                let errors = if rate >= 800 { rate / 5 } else { 0 };
                bucket(t as f64, rate, errors, 5.0, 0.0)
            })
            .collect();
        assert!(run_loops(
            &mut c,
            &v,
            &StageLog::default(),
            &timeline[..7],
            false
        ));
        assert!(c.stop.is_none());
        assert_eq!(c.layers[0].watch.last_good, Some(200.0));
        run_loops(&mut c, &v, &StageLog::default(), &timeline, false);
        assert!(c.stop.is_none(), "the 800 plateau is judged when it ends");
        run_loops(&mut c, &v, &StageLog::default(), &timeline, true);
        let f = &c.findings[0];
        assert_eq!(f.kind, "breakpoint");
        assert_eq!(f.rate, Some(400.0));
        assert_eq!(f.failed_at, Some(800.0));
        assert!(c
            .stop
            .as_deref()
            .unwrap()
            .contains("last good rate is 400.0"));
        // Nothing more once stopped.
        assert!(!run_loops(
            &mut c,
            &v,
            &StageLog::default(),
            &timeline,
            true
        ));
    }

    #[test]
    fn target_on_overhead_moves_toward_the_goal() {
        let s = scenario(
            "  - name: t\n    source: { request: { path: / } }\n    pattern: { type: target, overhead_p99_ms: 10, within: 20s, hold: 10s, start: 100, max: 1000 }\n",
        );
        let mut c = Controls::for_scenario(&s);
        let v = view(&s, &c);
        assert_eq!(v.level(0, 0.5), Level::Run(100.0));
        // Overhead far below the goal: the rate goes up by at most 25 %.
        run_loops(
            &mut c,
            &v,
            &StageLog::default(),
            &[bucket(0.0, 100, 0, 5.0, 2.0)],
            false,
        );
        assert_eq!(c.layers[0].value, Some(125.0));
        let v = view(&s, &c);
        assert_eq!(v.level(0, 1.5), Level::Run(125.0));
        // Too high: down by at most 20 %.
        let t2 = [
            bucket(0.0, 100, 0, 5.0, 2.0),
            bucket(1.0, 100, 0, 5.0, 40.0),
        ];
        run_loops(&mut c, &v, &StageLog::default(), &t2, false);
        assert_eq!(c.layers[0].value, Some(100.0));
        // Near the goal for three seconds: settled.
        let mut t = t2.to_vec();
        for k in 2..5 {
            t.push(bucket(k as f64, 100, 0, 5.0, 10.5));
        }
        run_loops(&mut c, &v, &StageLog::default(), &t, false);
        assert_eq!(c.findings.len(), 1);
        assert_eq!(c.findings[0].kind, "target");
        assert!(c.stop.is_none());
    }

    #[test]
    fn the_stage_log_follows_holds_skips_and_sets() {
        // Plateaus 20, 40, 60 of 4 s each, and a constant 5 for 30 s.
        let s = scenario(
            "  - name: main\n    source: { request: { path: / } }\n    pattern: { type: step, start: 20, step: 20, every: 4s, max: 60 }\n  - name: probe\n    source: { request: { path: / } }\n    pattern: { type: constant, rate: 5, for: 30s }\n",
        );
        let mut log = StageLog::default();
        // Note every 100 ms from tick `from` (inclusive) to `to`.
        let note = |log: &mut StageLog, c: &Controls, from: u32, to: u32| {
            let v = view(&s, c);
            for k in from..to {
                log.note(&v, f64::from(k) / 10.0);
            }
        };
        let c = Controls::for_scenario(&s);
        note(&mut log, &c, 0, 60);
        let c = c
            .apply(&s, &change(r#"{"layer": "main", "hold": true}"#), 6.0)
            .unwrap();
        note(&mut log, &c, 60, 100);
        let c = c
            .apply(&s, &change(r#"{"set": {"probe.pattern.rate": 8}}"#), 10.0)
            .unwrap();
        note(&mut log, &c, 100, 160);
        let c = c
            .apply(&s, &change(r#"{"layer": "main", "hold": false}"#), 16.0)
            .unwrap();
        note(&mut log, &c, 160, 190);
        let c = c
            .apply(
                &s,
                &change(r#"{"layer": "main", "skip_stage": true}"#),
                19.0,
            )
            .unwrap();
        note(&mut log, &c, 190, 200);
        let main: Vec<(f64, Option<usize>, u64, f64)> = log
            .marks
            .iter()
            .filter(|m| m.layer == 0)
            .map(|m| (m.at_secs, m.stage, m.part, m.target))
            .collect();
        // Plateau 1 ran from 4 s, held from 6 s to 16 s, to 18 s; the skip
        // at 19 s ended the pattern (one stage).
        assert_eq!(
            main,
            vec![
                (0.0, Some(0), 0, 20.0),
                (4.0, Some(0), 1, 40.0),
                (18.0, Some(0), 2, 60.0),
                (19.0, None, 0, 0.0),
            ]
        );
        let probe: Vec<(f64, f64)> = log
            .marks
            .iter()
            .filter(|m| m.layer == 1)
            .map(|m| (m.at_secs, m.target))
            .collect();
        assert_eq!(probe, vec![(0.0, 5.0), (10.0, 8.0)]);
        assert_eq!(log.at(0, 12.0).map(|(_, m)| m.part), Some(1));
        assert_eq!(log.at(0, 18.5).map(|(_, m)| m.part), Some(2));
        assert_eq!(log.at(0, 19.5).and_then(|(_, m)| m.stage), None);
        assert!(log.at(0, -1.0).is_none());
        // Sending stops at 20 s: the probe (still running) is in no stage
        // after it, the main layer (over) gets no second end.
        let before = log.marks.len();
        log.end(2, 20.0);
        assert_eq!(log.marks.len(), before + 1);
        assert_eq!(log.at(1, 20.5).and_then(|(_, m)| m.stage), None);
        assert_eq!(log.at(1, 19.5).and_then(|(_, m)| m.stage), Some(0));
        // Bounded: nothing past the last mark that fits.
        let mut small = StageLog::default();
        let v = view(&s, &Controls::for_scenario(&s));
        for k in 0..(MAX_STAGE_MARKS as u32 + 10) {
            // A new plateau every 4 s: alternate between two times.
            small.note(&v, if k % 2 == 0 { 1.0 } else { 5.0 });
        }
        assert_eq!(small.marks.len(), MAX_STAGE_MARKS);
        assert!(small.full_at.is_some());
        assert!(small.at(0, 1000.0).is_none());
    }

    #[test]
    fn controls_and_changes_round_trip() {
        let s = two_layers();
        let c = Controls::for_scenario(&s)
            .apply(
                &s,
                &change(r#"{"layer": "a", "hold": true, "rate_multiplier": 2}"#),
                1.0,
            )
            .unwrap();
        let text = serde_json::to_string(&c).unwrap();
        let back: Controls = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
        assert!(change(r#"{"rate_multiplier": 2}"#).may_raise_load());
        assert!(!change(r#"{"pause": true}"#).may_raise_load());
        assert!(serde_json::from_str::<Change>(r#"{"faults": {}}"#).is_err());
    }
}
