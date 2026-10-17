//! Optional persistence (`RUSTYBIN_MOCKS_STATE_DIR`): mocks created or
//! edited through the API or console (and edits to the built-in sample) are
//! saved as one JSON file per mock and loaded again at startup, so console
//! work survives a restart. File-defined mocks are not saved: their source
//! of truth is `rustybin.yaml` / the mocks directory.
//!
//! Each file holds the draft and, when different, the live revision
//! (`{name}.json`). Writes go to a temporary file renamed into place.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::model::MockSettings;
use super::store::{MockStore, Origin, Snapshot};

/// One revision on disk.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    /// Omitted for the live revision when it shares the draft's spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    settings: MockSettings,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    name: String,
    origin: Origin,
    enabled: bool,
    draft: Revision,
    /// `None`: never deployed. Absent settings mean "the draft is live".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    live: Option<Revision>,
    #[serde(default)]
    live_is_draft: bool,
}

fn file_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// Should this mock be saved?
pub fn saved_origin(origin: Origin) -> bool {
    matches!(origin, Origin::Api | Origin::Sample)
}

/// Write one mock (errors are logged, never fatal).
pub fn save(dir: &Path, snap: &Snapshot) {
    if !saved_origin(snap.origin) {
        return;
    }
    let draft = &snap.draft;
    let live_is_draft = snap
        .live
        .as_ref()
        .is_some_and(|l| l.revision == draft.revision);
    let live = match &snap.live {
        Some(l) if !live_is_draft => Some(Revision {
            source: (l.source != draft.source).then(|| l.source.to_string()),
            settings: l.settings.clone(),
        }),
        _ => None,
    };
    let saved = Saved {
        version: 1,
        name: snap.name.clone(),
        origin: snap.origin,
        enabled: snap.enabled,
        draft: Revision {
            source: Some(draft.source.to_string()),
            settings: draft.settings.clone(),
        },
        live,
        live_is_draft,
    };
    let text = match serde_json::to_vec_pretty(&saved) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("mock {:?} not saved: {e}", snap.name);
            return;
        }
    };
    let target = file_for(dir, &snap.name);
    let tmp = dir.join(format!(".{}.json.tmp", snap.name));
    let result = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&tmp, &text))
        .and_then(|_| std::fs::rename(&tmp, &target));
    if let Err(e) = result {
        tracing::warn!("mock {:?} not saved to {}: {e}", snap.name, dir.display());
    }
}

/// Remove a saved mock (deleted or renamed).
pub fn remove(dir: &Path, name: &str) {
    let path = file_for(dir, name);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("saved mock {} not removed: {e}", path.display());
        }
    }
}

/// Load every saved mock into the store. Returns the names loaded and the
/// problems (one per file).
pub fn load(store: &MockStore, dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut loaded = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (loaded, problems);
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
        })
        .collect();
    files.sort();
    let limits = store.limits();
    for path in files.into_iter().take(limits.max_mocks) {
        let shown = path.display().to_string();
        let result = std::fs::metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                if m.len() as usize > limits.max_spec_bytes * 3 {
                    Err("file too large".to_string())
                } else {
                    std::fs::read(&path).map_err(|e| e.to_string())
                }
            })
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).map_err(|e| e.to_string()))
            .and_then(|saved| restore(store, saved));
        match result {
            Ok(name) => loaded.push(name),
            Err(e) => problems.push(format!("{shown}: {e}")),
        }
    }
    (loaded, problems)
}

fn restore(store: &MockStore, saved: Saved) -> Result<String, String> {
    if saved.version != 1 {
        return Err(format!("unsupported version {}", saved.version));
    }
    let name = saved.name;
    let draft_source = saved
        .draft
        .source
        .ok_or_else(|| "the draft has no spec".to_string())?;
    // A saved copy of the sample replaces the built-in one.
    if store.get(&name).is_some_and(|s| s.origin == Origin::Sample) {
        store.delete_quietly(&name);
    }
    let draft = store.compile(&draft_source, saved.draft.settings)?;
    match saved.live {
        Some(live) => {
            let source = live.source.unwrap_or_else(|| draft_source.clone());
            let live = store.compile(&source, live.settings)?;
            store
                .restore(&name, live, Some(draft), saved.origin, saved.enabled)
                .map_err(|e| e.message())?;
        }
        None if saved.live_is_draft => {
            store
                .restore(&name, draft, None, saved.origin, saved.enabled)
                .map_err(|e| e.message())?;
        }
        None => {
            store
                .restore_undeployed(&name, draft, saved.origin, saved.enabled)
                .map_err(|e| e.message())?;
        }
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mocks::{sample_settings, SAMPLE_SPEC};

    const SPEC: &str = r#"{"openapi":"3.0.3","info":{"title":"T","version":"1"},
      "paths":{"/a":{"get":{"operationId":"a","responses":{"200":{"description":"ok"}}}}}}"#;

    fn store_in(dir: &Path) -> MockStore {
        MockStore::new(&Config::for_tests()).with_persistence(dir.to_path_buf())
    }

    #[test]
    fn round_trip_through_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let s = store_in(tmp.path());
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("live-one", c, Origin::Api, true, true, false)
            .expect("create");
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("draft-only", c, Origin::Api, false, false, false)
            .expect("create");
        // A draft change on top of a live revision.
        let base = s.draft("live-one").expect("draft");
        let mut settings = base.settings.clone();
        settings.responses.delay_ms = 5;
        let next = s.recompile(&base, None, settings).expect("recompile");
        s.set_draft("live-one", Some(next), None).expect("draft");
        let sample = s.compile(SAMPLE_SPEC, sample_settings()).expect("sample");
        s.create("petstore", sample, Origin::Sample, true, true, false)
            .expect("sample");
        s.set_enabled("petstore", false).expect("off");
        let c = s.compile(SPEC, MockSettings::default()).expect("compile");
        s.create("from-file", c, Origin::File, true, true, false)
            .expect("file");
        s.set_draft("draft-only", None, Some("renamed"))
            .expect("rename");

        let mut names: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["live-one.json", "petstore.json", "renamed.json"]
        );

        let fresh = MockStore::new(&Config::for_tests());
        let sample = fresh
            .compile(SAMPLE_SPEC, sample_settings())
            .expect("sample");
        fresh
            .create("petstore", sample, Origin::Sample, true, true, false)
            .expect("sample");
        let (loaded, problems) = load(&fresh, tmp.path());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(loaded.len(), 3);
        let one = fresh.get("live-one").expect("live-one");
        assert_eq!(one.state(), "live");
        assert!(one.has_draft_changes());
        assert_eq!(one.draft.settings.responses.delay_ms, 5);
        assert_eq!(
            one.live.as_ref().map(|l| l.settings.responses.delay_ms),
            Some(0)
        );
        assert_eq!(fresh.get("renamed").expect("renamed").state(), "draft");
        let pet = fresh.get("petstore").expect("petstore");
        assert!(!pet.enabled);
        assert_eq!(pet.origin, Origin::Sample);

        s.delete("renamed");
        assert!(!tmp.path().join("renamed.json").exists());
    }

    #[test]
    fn bad_files_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("broken.json"), "{").expect("write");
        std::fs::write(tmp.path().join("notes.txt"), "ignored").expect("write");
        let (loaded, problems) = load(&MockStore::new(&Config::for_tests()), tmp.path());
        assert!(loaded.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("broken.json"));
    }
}

#[cfg(test)]
mod startup_tests {
    use crate::config::Config;
    use crate::mocks::{new_store, SAMPLE_NAME};

    #[test]
    fn new_store_keeps_sample_edits_across_restarts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = Config::for_tests();
        config.mocks_state_dir = Some(tmp.path().display().to_string());
        let first = new_store(&config);
        // A pristine sample is not written.
        assert!(!tmp.path().join("petstore.json").exists());
        first.set_enabled(SAMPLE_NAME, false).expect("off");
        assert!(tmp.path().join("petstore.json").exists());
        let second = new_store(&config);
        let pet = second.get(SAMPLE_NAME).expect("sample");
        assert!(!pet.enabled);
        // Loading wrote nothing new and lost nothing.
        assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 1);
        assert!(second.load_errors().is_empty());
    }
}
