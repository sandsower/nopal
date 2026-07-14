//! Global field config: project roots/scan dirs for the spawn picker's
//! candidate list, worktree naming prefixes, and
//! remappable keybindings.
//!
//! Lives at `<state-dir>/field/config.json`, sibling of
//! `managed-seats.json`. Loading is tolerant like `registry::load`: a
//! missing or malformed file degrades to empty project lists, default
//! prefixes, and default keybindings rather than failing the field - a
//! fresh install still spawns seats at literal paths and dispatches every
//! key at its hardcoded default; it just has no candidate list.
//!
//! The `keys` section maps [`crate::keys::KeyAction`] names to key specs,
//! e.g. `{"keys": {"goto_picker": "g", "release_input": "ctrl-o"}}`; see
//! `crate::keys` for the full action inventory, key-spec grammar, and the
//! fail-soft validation `crate::keys::KeyRegistry::build` runs over this
//! map at startup.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

fn default_dir_prefix() -> String {
    "nopal-".to_owned()
}

fn default_branch_prefix() -> String {
    "nopal/".to_owned()
}

/// Repo discovery: explicit repo paths plus parent dirs scanned one
/// level deep for git repos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProjectsConfig {
    /// Explicit repo paths.
    #[serde(default)]
    pub roots: Vec<String>,
    /// Parent dirs scanned one level deep for a child containing a
    /// `.git` entry.
    #[serde(default)]
    pub scan: Vec<String>,
}

/// Worktree naming templates. `nopal-` and `nopal/` are defaults, not rules;
/// every deployment can rename them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreesConfig {
    #[serde(default = "default_dir_prefix")]
    pub dir_prefix: String,
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        Self {
            dir_prefix: default_dir_prefix(),
            branch_prefix: default_branch_prefix(),
        }
    }
}

/// Whole seat config document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SeatConfig {
    #[serde(default)]
    pub projects: ProjectsConfig,
    #[serde(default)]
    pub worktrees: WorktreesConfig,
    /// Remappable keybindings: `crate::keys::KeyAction` name -> key spec
    /// string. Parsed once at field startup by
    /// `crate::keys::KeyRegistry::build`, never here - this struct only
    /// carries the raw strings, the same tolerant-loading contract as
    /// `projects`/`worktrees`.
    #[serde(default)]
    pub keys: std::collections::BTreeMap<String, String>,
}

/// Path to the seat config under the given state dir, or the same
/// default `registry::registry_path` uses when none was configured.
pub fn config_path(state_dir: Option<&Path>) -> PathBuf {
    let base = match state_dir {
        Some(dir) => dir.to_path_buf(),
        None => default_state_dir(),
    };
    base.join("field").join("config.json")
}

fn default_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NOPAL_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join(".local/state/nopal")
}

/// Load the seat config, tolerating a missing or malformed file
/// (returns defaults rather than failing the field). Every configured
/// project path has a leading `~/` expanded against `$HOME`.
pub fn load(state_dir: Option<&Path>) -> SeatConfig {
    let path = config_path(state_dir);
    let mut cfg: SeatConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    cfg.projects.roots = cfg.projects.roots.iter().map(|p| expand_home(p)).collect();
    cfg.projects.scan = cfg.projects.scan.iter().map(|p| expand_home(p)).collect();
    cfg
}

/// Expand a leading `~/` against `$HOME`; any other path passes through
/// unchanged.
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        return format!("{home}/{rest}");
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_empty_projects_and_vic_prefixes() {
        let cfg = SeatConfig::default();
        assert!(cfg.projects.roots.is_empty());
        assert!(cfg.projects.scan.is_empty());
        assert_eq!(cfg.worktrees.dir_prefix, "nopal-");
        assert_eq!(cfg.worktrees.branch_prefix, "nopal/");
    }

    #[test]
    fn config_path_honors_state_dir() {
        let p = config_path(Some(Path::new("/x/state")));
        assert_eq!(p, Path::new("/x/state/field/config.json"));
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(Some(&dir.path().join("nope")));
        assert_eq!(cfg, SeatConfig::default());
    }

    #[test]
    fn load_malformed_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let field_dir = dir.path().join("field");
        std::fs::create_dir_all(&field_dir).unwrap();
        std::fs::write(field_dir.join("config.json"), "not json").unwrap();
        let cfg = load(Some(dir.path()));
        assert_eq!(cfg, SeatConfig::default());
    }

    #[test]
    fn load_partial_document_fills_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let field_dir = dir.path().join("field");
        std::fs::create_dir_all(&field_dir).unwrap();
        std::fs::write(
            field_dir.join("config.json"),
            r#"{"projects": {"roots": ["/a/b"]}}"#,
        )
        .unwrap();
        let cfg = load(Some(dir.path()));
        assert_eq!(cfg.projects.roots, vec!["/a/b".to_owned()]);
        assert!(cfg.projects.scan.is_empty());
        assert_eq!(cfg.worktrees.dir_prefix, "nopal-");
        assert_eq!(cfg.worktrees.branch_prefix, "nopal/");
    }

    #[test]
    fn load_expands_tilde_in_roots_and_scan() {
        let dir = tempfile::tempdir().unwrap();
        let field_dir = dir.path().join("field");
        std::fs::create_dir_all(&field_dir).unwrap();
        std::fs::write(
            field_dir.join("config.json"),
            r#"{"projects": {"roots": ["~/teotl"], "scan": ["~/Projects"]}}"#,
        )
        .unwrap();
        let cfg = load(Some(dir.path()));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(cfg.projects.roots, vec![format!("{home}/teotl")]);
        assert_eq!(cfg.projects.scan, vec![format!("{home}/Projects")]);
    }

    #[test]
    fn load_leaves_absolute_paths_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let field_dir = dir.path().join("field");
        std::fs::create_dir_all(&field_dir).unwrap();
        std::fs::write(
            field_dir.join("config.json"),
            r#"{"projects": {"roots": ["/abs/path"]}}"#,
        )
        .unwrap();
        let cfg = load(Some(dir.path()));
        assert_eq!(cfg.projects.roots, vec!["/abs/path".to_owned()]);
    }

    #[test]
    fn round_trips_through_serde() {
        let cfg = SeatConfig {
            projects: ProjectsConfig {
                roots: vec!["/a".to_owned()],
                scan: vec!["/b".to_owned()],
            },
            worktrees: WorktreesConfig {
                dir_prefix: "x-".to_owned(),
                branch_prefix: "x/".to_owned(),
            },
            keys: [("goto_picker".to_owned(), "o".to_owned())]
                .into_iter()
                .collect(),
        };
        let text = serde_json::to_string(&cfg).unwrap();
        let back: SeatConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn load_partial_document_with_keys_section_fills_in_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let field_dir = dir.path().join("field");
        std::fs::create_dir_all(&field_dir).unwrap();
        std::fs::write(
            field_dir.join("config.json"),
            r#"{"keys": {"goto_picker": "o", "release_input": "ctrl-o"}}"#,
        )
        .unwrap();
        let cfg = load(Some(dir.path()));
        assert_eq!(cfg.keys.get("goto_picker").map(String::as_str), Some("o"));
        assert_eq!(
            cfg.keys.get("release_input").map(String::as_str),
            Some("ctrl-o")
        );
        assert!(cfg.projects.roots.is_empty());
        assert_eq!(cfg.worktrees.dir_prefix, "nopal-");
    }
}
