//! Saved scenarios (`docs/plans/harness.md`, section 6.6): scenarios kept
//! by name next to the built-in ones, a starting point to copy and tune.
//! `PUT /_rustybin/harness/scenarios/{name}` saves one (validated, at most
//! [`MAX_SAVED`]), `DELETE` removes it; built-in names are taken. With
//! `RUSTYBIN_HARNESS_STATE_DIR` each is also written to
//! `<dir>/scenarios/<name>.json` (a temporary file renamed into place) and
//! loaded at startup, like the mocks' state directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::scenario::Scenario;

/// Most saved scenarios per node.
pub const MAX_SAVED: usize = 100;

/// One saved scenario.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub name: String,
    pub saved_at: String,
    pub scenario: Scenario,
}

/// The node's saved scenarios.
#[derive(Default)]
pub struct SavedScenarios {
    dir: Option<PathBuf>,
    items: Mutex<BTreeMap<String, Saved>>,
}

/// A name a saved scenario may have (it is also its file name).
pub fn valid_name(name: &str) -> bool {
    super::cluster::safe_id(name)
}

impl SavedScenarios {
    /// The store, loaded from `<dir>/scenarios` when a state directory is set.
    pub fn new(state_dir: Option<&Path>) -> Self {
        let dir = state_dir.map(|d| d.join("scenarios"));
        let mut items = BTreeMap::new();
        if let Some(dir) = &dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let loaded = std::fs::read(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|b| {
                            serde_json::from_slice::<Saved>(&b).map_err(|e| e.to_string())
                        });
                    match loaded {
                        Ok(s) if valid_name(&s.name) && items.len() < MAX_SAVED => {
                            items.insert(s.name.clone(), s);
                        }
                        Ok(_) => tracing::warn!(
                            "saved scenario {} skipped (name or count)",
                            path.display()
                        ),
                        Err(e) => {
                            tracing::warn!("saved scenario {} not loaded: {e}", path.display())
                        }
                    }
                }
            }
            if !items.is_empty() {
                tracing::info!(
                    "harness: {} saved scenario(s) loaded from {}",
                    items.len(),
                    dir.display()
                );
            }
        }
        Self {
            dir,
            items: Mutex::new(items),
        }
    }

    fn items(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Saved>> {
        self.items.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn list(&self) -> Vec<Saved> {
        self.items().values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<Saved> {
        self.items().get(name).cloned()
    }

    /// Save (or replace) a scenario under `name`. Returns whether it is new.
    pub fn put(&self, name: &str, scenario: Scenario) -> Result<bool, String> {
        if !valid_name(name) {
            return Err("the name: letters, digits, . _ - only, at most 64 characters".into());
        }
        let saved = Saved {
            name: name.to_string(),
            saved_at: chrono::Utc::now().to_rfc3339(),
            scenario,
        };
        let created = {
            let mut items = self.items();
            let created = !items.contains_key(name);
            if created && items.len() >= MAX_SAVED {
                return Err(format!(
                    "this node keeps at most {MAX_SAVED} saved scenarios: delete one first"
                ));
            }
            items.insert(name.to_string(), saved.clone());
            created
        };
        if let Some(dir) = &self.dir {
            let write = || -> std::io::Result<()> {
                std::fs::create_dir_all(dir)?;
                let tmp = dir.join(format!(".{name}.json.tmp"));
                std::fs::write(&tmp, serde_json::to_vec_pretty(&saved).unwrap_or_default())?;
                std::fs::rename(&tmp, dir.join(format!("{name}.json")))
            };
            if let Err(e) = write() {
                tracing::warn!(
                    "saved scenario {name:?} not written to {}: {e}",
                    dir.display()
                );
            }
        }
        Ok(created)
    }

    /// Remove a saved scenario. Returns whether it existed.
    pub fn delete(&self, name: &str) -> bool {
        let existed = self.items().remove(name).is_some();
        if existed {
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_file(dir.join(format!("{name}.json")));
            }
        }
        existed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(name: &str) -> Scenario {
        Scenario::parse(&format!(
            "name: {name}\ntarget: http://127.0.0.1:1\nsource: {{ request: {{ path: / }} }}\npattern: {{ type: constant, rate: 1, for: 1s }}\n"
        ))
        .unwrap()
    }

    #[test]
    fn saved_scenarios_are_bounded_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let store = SavedScenarios::new(Some(dir.path()));
        assert!(store.put("one", scenario("one")).unwrap());
        assert!(!store.put("one", scenario("one again")).unwrap());
        assert!(store.put("bad name", scenario("x")).is_err());
        assert!(dir.path().join("scenarios/one.json").exists());
        // A new store reads them back.
        let again = SavedScenarios::new(Some(dir.path()));
        assert_eq!(again.get("one").unwrap().scenario.name, "one again");
        assert!(again.delete("one"));
        assert!(!again.delete("one"));
        assert!(!dir.path().join("scenarios/one.json").exists());
        let mem = SavedScenarios::new(None);
        for i in 0..MAX_SAVED {
            mem.put(&format!("s{i}"), scenario("s")).unwrap();
        }
        assert!(mem.put("one-more", scenario("s")).is_err());
        assert!(
            mem.put("s0", scenario("replaced")).is_ok(),
            "replacing stays allowed"
        );
        assert_eq!(mem.list().len(), MAX_SAVED);
    }
}
