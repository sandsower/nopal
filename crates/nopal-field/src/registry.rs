//! Durable record of the sessions nopal manages (Feature 1's re-stamp
//! story).
//!
//! tmux user options - session-scoped `@nopal_managed` included - do not
//! survive a tmux-resurrect/continuum restore: resurrect recreates sessions
//! from its own saved layout and drops custom `@`-options (the same fact the
//! field already relies on for pane options, see `cli::repair_session`).
//! So the marker alone is not durable. This module keeps a small JSON file
//! under the nopal state dir mapping session name -> seat metadata; the
//! field re-applies the marker to every still-live listed session on every
//! launch, healing a restore. The marker stays the fast per-frame filter
//! input; this file is the durable source of truth behind it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One managed session, keyed by tmux session name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSeat {
    pub session: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub recorded_at: String,
    /// The path the seat was spawned at; empty for entries
    /// recorded before this field existed. Feeds `RegistrySource`'s
    /// recent-candidate rows and the naming collision probe.
    #[serde(default)]
    pub path: String,
}

/// Path to the managed-seats registry under the given state dir, or a
/// sensible default when none was configured.
pub fn registry_path(state_dir: Option<&Path>) -> PathBuf {
    let base = match state_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_state_dir(),
    };
    base.join("field").join("managed-seats.json")
}

fn default_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NOPAL_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join(".local/state/nopal")
}

/// Load the registry, tolerating a missing or malformed file (returns
/// empty rather than failing the field).
pub fn load(path: &Path) -> Vec<ManagedSeat> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Record a managed session, replacing any prior entry for the same name.
/// Best-effort: a write failure degrades the resurrect re-stamp, never the
/// running field.
pub fn record(path: &Path, entry: ManagedSeat) {
    let mut entries = load(path);
    entries.retain(|e| e.session != entry.session);
    entries.push(entry);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&entries) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_load_round_trips_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("field/managed-seats.json");
        record(
            &path,
            ManagedSeat {
                session: "rondo".to_owned(),
                repo: "rondo".to_owned(),
                recorded_at: "t1".to_owned(),
                path: String::new(),
            },
        );
        record(
            &path,
            ManagedSeat {
                session: "rondo".to_owned(),
                repo: "rondo".to_owned(),
                recorded_at: "t2".to_owned(),
                path: String::new(),
            },
        );
        record(
            &path,
            ManagedSeat {
                session: "teotl".to_owned(),
                repo: "teotl".to_owned(),
                recorded_at: "t3".to_owned(),
                path: String::new(),
            },
        );
        let entries = load(&path);
        assert_eq!(entries.len(), 2, "same session replaced, not duplicated");
        let rondo = entries.iter().find(|e| e.session == "rondo").unwrap();
        assert_eq!(rondo.recorded_at, "t2", "latest record wins");
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("nope.json")).is_empty());
    }

    #[test]
    fn registry_path_honors_state_dir() {
        let p = registry_path(Some(Path::new("/x/state")));
        assert_eq!(p, Path::new("/x/state/field/managed-seats.json"));
    }

    #[test]
    fn path_field_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("field/managed-seats.json");
        record(
            &path,
            ManagedSeat {
                session: "teotl".to_owned(),
                repo: "teotl".to_owned(),
                recorded_at: "t1".to_owned(),
                path: "/home/alex/projects/teotl".to_owned(),
            },
        );
        let entries = load(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/home/alex/projects/teotl");
    }

    #[test]
    fn loads_legacy_entries_missing_the_path_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("field/managed-seats.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"[{"session": "teotl", "repo": "teotl", "recorded_at": "t1"}]"#,
        )
        .unwrap();
        let entries = load(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session, "teotl");
        assert_eq!(entries[0].path, "", "legacy entry defaults to empty path");
    }
}
