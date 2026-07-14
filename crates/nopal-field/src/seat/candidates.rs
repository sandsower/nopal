//! Spawn-picker candidates: project roots, their `git worktree`s, and
//! recents from the managed-seats registry, merged into one deduplicated
//! list.
//!
//! Each source is a thin, independently testable read of the
//! filesystem/git/registry; `merge` is the one place that decides
//! ordering and de-duplication, kept pure so the picker's shape can be
//! unit-tested without a real git checkout.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::registry::ManagedSeat;
use crate::seat::config::SeatConfig;

/// What a candidate points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    ProjectRoot,
    Worktree,
    Recent,
    /// No real path yet - the picker collects a name and calls
    /// [`crate::seat::worktree::create`] before spawning.
    NewWorktree,
}

/// One row the spawn picker can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    /// Empty for [`CandidateKind::NewWorktree`].
    pub path: String,
    /// Owning project's basename, used only as a display label.
    pub project: String,
    /// Canonical project-root identity used for grouping and worktree
    /// creation. Empty for recents whose historical registry entry does not
    /// retain a trustworthy root path.
    pub project_root: String,
    pub kind: CandidateKind,
}

/// A source of spawn candidates. The picker fans out over every
/// registered source and passes the batches to [`merge`].
pub trait CandidateSource {
    fn candidates(&self) -> Vec<Candidate>;
}

/// Config-declared project roots plus parent dirs scanned one level
/// deep. Each project contributes a [`CandidateKind::ProjectRoot`] row
/// and a `+ new worktree in <name>` [`CandidateKind::NewWorktree`] row.
pub struct ProjectSource<'a> {
    config: &'a SeatConfig,
}

impl<'a> ProjectSource<'a> {
    pub fn new(config: &'a SeatConfig) -> Self {
        Self { config }
    }
}

/// Every project path the config resolves to: explicit `roots` plus the
/// git-repo children found one level under each `scan` dir. Shared by
/// [`ProjectSource`] and the spawn picker's own worktree lookup, so both
/// walk the same discovery rule.
pub fn discover_projects(config: &SeatConfig) -> Vec<String> {
    let mut projects: Vec<String> = config.projects.roots.clone();
    for scan_dir in &config.projects.scan {
        projects.extend(scan_children(scan_dir));
    }
    projects
}

impl CandidateSource for ProjectSource<'_> {
    fn candidates(&self) -> Vec<Candidate> {
        let projects = discover_projects(self.config);
        let mut out = Vec::with_capacity(projects.len() * 2);
        for project in projects {
            let name = basename(&project);
            let project_root = canonical_identity(&project);
            out.push(Candidate {
                label: name.clone(),
                path: project,
                project: name.clone(),
                project_root: project_root.clone(),
                kind: CandidateKind::ProjectRoot,
            });
            out.push(Candidate {
                label: format!("+ new worktree in {name}"),
                path: String::new(),
                project: name,
                project_root,
                kind: CandidateKind::NewWorktree,
            });
        }
        out
    }
}

/// One level deep under `scan_dir`: children containing a `.git` entry
/// (a directory for a normal clone, a file for a linked worktree).
fn scan_children(scan_dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(scan_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join(".git").exists() {
            out.push(path.to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

fn basename(path: &str) -> String {
    Path::new(path.trim_end_matches('/'))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

/// Canonicalize `path` for comparison purposes, falling back to the
/// trimmed literal when the path doesn't exist or canonicalization
/// otherwise fails (e.g. it was just removed).
fn canonical_or_trimmed(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path.trim_end_matches('/')))
}

fn canonical_identity(path: &str) -> String {
    canonical_or_trimmed(path).to_string_lossy().into_owned()
}

/// `git -C <project> worktree list --porcelain` per project; the
/// project root itself is skipped since [`ProjectSource`] already
/// covers it.
pub struct WorktreeSource {
    projects: Vec<String>,
}

impl WorktreeSource {
    pub fn new(projects: Vec<String>) -> Self {
        Self { projects }
    }
}

impl CandidateSource for WorktreeSource {
    fn candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();
        for project in &self.projects {
            let project_name = basename(project);
            let project_root = canonical_identity(project);
            let Ok(output) = Command::new("git")
                .args(["-C", project, "worktree", "list", "--porcelain"])
                .output()
            else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            let text = String::from_utf8_lossy(&output.stdout);
            // git reports worktree paths canonicalized (e.g. resolving
            // macOS's `/var` -> `/private/var` symlink), which may not
            // match `project` byte-for-byte even when it names the same
            // directory; canonicalize both sides before comparing.
            let root = canonical_or_trimmed(project);
            for line in text.lines() {
                let Some(path) = line.strip_prefix("worktree ") else {
                    continue;
                };
                if canonical_or_trimmed(path) == root {
                    continue;
                }
                out.push(Candidate {
                    label: basename(path),
                    path: path.to_owned(),
                    project: project_name.clone(),
                    project_root: project_root.clone(),
                    kind: CandidateKind::Worktree,
                });
            }
        }
        out
    }
}

/// Managed-seats registry entries that carry a real, still-live `path`.
/// Entries recorded before this slice (or whose path was since removed)
/// have no candidate row but remain valid for resurrect re-stamping.
pub struct RegistrySource {
    entries: Vec<ManagedSeat>,
}

impl RegistrySource {
    pub fn new(entries: Vec<ManagedSeat>) -> Self {
        Self { entries }
    }
}

impl CandidateSource for RegistrySource {
    fn candidates(&self) -> Vec<Candidate> {
        self.entries
            .iter()
            .filter(|entry| !entry.path.is_empty() && Path::new(&entry.path).exists())
            .map(|entry| Candidate {
                label: entry.session.clone(),
                path: entry.path.clone(),
                project: if entry.repo.is_empty() {
                    basename(&entry.path)
                } else {
                    entry.repo.clone()
                },
                project_root: String::new(),
                kind: CandidateKind::Recent,
            })
            .collect()
    }
}

/// Merge candidate batches from the three v1 sources into the display
/// order the plan calls for: recents first, then per-project groups
/// (root, its worktrees, its new-worktree row), each project in the
/// order it first appears in `projects`. Deduped by path, first wins;
/// [`CandidateKind::NewWorktree`] rows have no real path so they're
/// deduped by canonical project-root identity instead.
pub fn merge(
    recents: Vec<Candidate>,
    projects: Vec<Candidate>,
    worktrees: Vec<Candidate>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut seen_new_worktree_roots = HashSet::new();

    for candidate in recents {
        push_deduped(
            &mut out,
            &mut seen_paths,
            &mut seen_new_worktree_roots,
            candidate,
        );
    }

    let mut project_order: Vec<String> = Vec::new();
    for candidate in &projects {
        if !project_order.contains(&candidate.project_root) {
            project_order.push(candidate.project_root.clone());
        }
    }

    for project in project_order {
        if let Some(root) = projects
            .iter()
            .find(|c| c.project_root == project && c.kind == CandidateKind::ProjectRoot)
        {
            push_deduped(
                &mut out,
                &mut seen_paths,
                &mut seen_new_worktree_roots,
                root.clone(),
            );
        }
        for worktree in worktrees.iter().filter(|c| c.project_root == project) {
            push_deduped(
                &mut out,
                &mut seen_paths,
                &mut seen_new_worktree_roots,
                worktree.clone(),
            );
        }
        if let Some(new_worktree) = projects
            .iter()
            .find(|c| c.project_root == project && c.kind == CandidateKind::NewWorktree)
        {
            push_deduped(
                &mut out,
                &mut seen_paths,
                &mut seen_new_worktree_roots,
                new_worktree.clone(),
            );
        }
    }

    out
}

fn push_deduped(
    out: &mut Vec<Candidate>,
    seen_paths: &mut HashSet<String>,
    seen_new_worktree_roots: &mut HashSet<String>,
    candidate: Candidate,
) {
    let is_new = match candidate.kind {
        CandidateKind::NewWorktree => {
            seen_new_worktree_roots.insert(candidate.project_root.clone())
        }
        _ => seen_paths.insert(normalize(&candidate.path)),
    };
    if is_new {
        out.push(candidate);
    }
}

fn normalize(path: &str) -> String {
    path.trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(label: &str, path: &str, project: &str, kind: CandidateKind) -> Candidate {
        let project_root = match kind {
            CandidateKind::ProjectRoot => normalize(path),
            CandidateKind::Worktree | CandidateKind::NewWorktree => format!("/x/{project}"),
            CandidateKind::Recent => String::new(),
        };
        candidate_for_root(label, path, project, &project_root, kind)
    }

    fn candidate_for_root(
        label: &str,
        path: &str,
        project: &str,
        project_root: &str,
        kind: CandidateKind,
    ) -> Candidate {
        Candidate {
            label: label.to_owned(),
            path: path.to_owned(),
            project: project.to_owned(),
            project_root: project_root.to_owned(),
            kind,
        }
    }

    #[test]
    fn project_source_yields_root_and_new_worktree_for_explicit_roots() {
        let cfg = SeatConfig {
            projects: crate::seat::config::ProjectsConfig {
                roots: vec!["/a/teotl".to_owned()],
                scan: vec![],
            },
            worktrees: Default::default(),
            keys: Default::default(),
        };
        let out = ProjectSource::new(&cfg).candidates();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, CandidateKind::ProjectRoot);
        assert_eq!(out[0].path, "/a/teotl");
        assert_eq!(out[0].project, "teotl");
        assert_eq!(out[0].project_root, "/a/teotl");
        assert_eq!(out[1].kind, CandidateKind::NewWorktree);
        assert_eq!(out[1].label, "+ new worktree in teotl");
        assert_eq!(out[1].project, "teotl");
        assert_eq!(out[1].project_root, "/a/teotl");
        assert!(out[1].path.is_empty());
    }

    #[test]
    fn project_source_scans_one_level_deep_for_git_children() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo-a");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        let non_repo = dir.path().join("not-a-repo");
        std::fs::create_dir_all(&non_repo).unwrap();
        // A linked worktree uses a `.git` *file*, not a dir - must still count.
        let linked = dir.path().join("repo-b");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: /elsewhere").unwrap();

        let cfg = SeatConfig {
            projects: crate::seat::config::ProjectsConfig {
                roots: vec![],
                scan: vec![dir.path().to_string_lossy().into_owned()],
            },
            worktrees: Default::default(),
            keys: Default::default(),
        };
        let out = ProjectSource::new(&cfg).candidates();
        let roots: Vec<&str> = out
            .iter()
            .filter(|c| c.kind == CandidateKind::ProjectRoot)
            .map(|c| c.project.as_str())
            .collect();
        assert!(roots.contains(&"repo-a"));
        assert!(roots.contains(&"repo-b"));
        assert!(!roots.contains(&"not-a-repo"));
    }

    #[test]
    fn worktree_source_skips_the_project_root_itself() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("proj");
        init_repo(&repo);
        let source = WorktreeSource::new(vec![repo.to_string_lossy().into_owned()]);
        let out = source.candidates();
        assert!(out.is_empty(), "a fresh repo has no linked worktrees");
    }

    #[test]
    fn worktree_source_lists_linked_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("proj");
        init_repo(&repo);
        let wt_dir = dir.path().join("proj-wt");
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "add", "-b", "feature", wt_dir.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());

        let source = WorktreeSource::new(vec![repo.to_string_lossy().into_owned()]);
        let out = source.candidates();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CandidateKind::Worktree);
        assert_eq!(out[0].project, "proj");
        assert_eq!(out[0].label, "proj-wt");
    }

    #[test]
    fn registry_source_skips_empty_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        let entries = vec![
            ManagedSeat {
                session: "no-path".to_owned(),
                repo: String::new(),
                recorded_at: String::new(),
                path: String::new(),
            },
            ManagedSeat {
                session: "gone".to_owned(),
                repo: String::new(),
                recorded_at: String::new(),
                path: dir
                    .path()
                    .join("does-not-exist")
                    .to_string_lossy()
                    .into_owned(),
            },
            ManagedSeat {
                session: "live".to_owned(),
                repo: "myrepo".to_owned(),
                recorded_at: String::new(),
                path: live.to_string_lossy().into_owned(),
            },
        ];
        let out = RegistrySource::new(entries).candidates();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "live");
        assert_eq!(out[0].project, "myrepo");
        assert_eq!(out[0].kind, CandidateKind::Recent);
    }

    #[test]
    fn merge_orders_recents_then_root_worktrees_new_worktree_per_project() {
        let recents = vec![candidate(
            "r1",
            "/x/recent",
            "recent-proj",
            CandidateKind::Recent,
        )];
        let projects = vec![
            candidate("a", "/x/a", "a", CandidateKind::ProjectRoot),
            candidate("+ new worktree in a", "", "a", CandidateKind::NewWorktree),
            candidate("b", "/x/b", "b", CandidateKind::ProjectRoot),
            candidate("+ new worktree in b", "", "b", CandidateKind::NewWorktree),
        ];
        let worktrees = vec![candidate("a-wt", "/x/a-wt", "a", CandidateKind::Worktree)];

        let out = merge(recents, projects, worktrees);
        let labels: Vec<&str> = out.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "r1",
                "a",
                "a-wt",
                "+ new worktree in a",
                "b",
                "+ new worktree in b"
            ]
        );
    }

    #[test]
    fn merge_dedupes_by_path_first_wins() {
        let recents = vec![candidate("recent-a", "/x/a", "a", CandidateKind::Recent)];
        let projects = vec![candidate("a", "/x/a", "a", CandidateKind::ProjectRoot)];
        let out = merge(recents, projects, vec![]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "recent-a", "first occurrence (recent) wins");
    }

    #[test]
    fn merge_dedupes_new_worktree_rows_by_project_not_path() {
        let projects = vec![
            candidate("+ new worktree in a", "", "a", CandidateKind::NewWorktree),
            candidate(
                "+ new worktree in a (dup)",
                "",
                "a",
                CandidateKind::NewWorktree,
            ),
        ];
        let out = merge(vec![], projects, vec![]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "+ new worktree in a");
    }

    #[test]
    fn merge_keeps_projects_with_the_same_basename_distinct() {
        let projects = vec![
            candidate_for_root(
                "service",
                "/team-a/service",
                "service",
                "/team-a/service",
                CandidateKind::ProjectRoot,
            ),
            candidate_for_root(
                "+ new worktree in service",
                "",
                "service",
                "/team-a/service",
                CandidateKind::NewWorktree,
            ),
            candidate_for_root(
                "service",
                "/team-b/service",
                "service",
                "/team-b/service",
                CandidateKind::ProjectRoot,
            ),
            candidate_for_root(
                "+ new worktree in service",
                "",
                "service",
                "/team-b/service",
                CandidateKind::NewWorktree,
            ),
        ];
        let worktrees = vec![
            candidate_for_root(
                "service-a-wt",
                "/worktrees/service-a",
                "service",
                "/team-a/service",
                CandidateKind::Worktree,
            ),
            candidate_for_root(
                "service-b-wt",
                "/worktrees/service-b",
                "service",
                "/team-b/service",
                CandidateKind::Worktree,
            ),
        ];

        let out = merge(vec![], projects, worktrees);

        assert_eq!(
            out.iter()
                .filter(|candidate| candidate.kind == CandidateKind::ProjectRoot)
                .map(|candidate| candidate.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/team-a/service", "/team-b/service"]
        );
        assert_eq!(
            out.iter()
                .filter(|candidate| candidate.kind == CandidateKind::NewWorktree)
                .count(),
            2
        );
        assert_eq!(
            out.iter()
                .filter(|candidate| candidate.kind == CandidateKind::Worktree)
                .map(|candidate| candidate.project_root.as_str())
                .collect::<Vec<_>>(),
            vec!["/team-a/service", "/team-b/service"]
        );
    }

    #[test]
    fn merge_trailing_slash_paths_dedupe_as_equal() {
        let recents = vec![candidate("recent", "/x/a/", "a", CandidateKind::Recent)];
        let projects = vec![candidate("a", "/x/a", "a", CandidateKind::ProjectRoot)];
        let out = merge(recents, projects, vec![]);
        assert_eq!(out.len(), 1);
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(path.join("README.md"), "seed").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-m", "seed"]);
    }
}
