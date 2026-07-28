//! Where nopal config lives inside a project.
//!
//! `.nopal/` is discovered by walking up from the starting directory to the
//! enclosing git repo's toplevel, not fixed at `--dir` (the
//! earlier behavior that anchored directly at `--dir`). A repo with no
//! `.nopal/` directory
//! anywhere between the starting point and the toplevel is unconfigured,
//! not misconfigured; [`project_root`] still returns a concrete directory
//! (the toplevel) so a real `nopal cli` launch has somewhere to scaffold
//! into - see `crate::scaffold`.

use std::path::{Path, PathBuf};

use crate::profile::Module;
use crate::run_ledger_store::{git_stdout, resolve_like_python};

pub const NOPAL_DIR: &str = ".nopal";
/// Historical project-state marker retained only so v0.3 can preserve and reject it.
pub const LEGACY_DIR: &str = ".crust";
pub const MANIFEST_FILE: &str = "nopal.jsonc";

/// Project-relative manifest path (used in diagnostics and display).
pub fn manifest_rel_path() -> String {
    format!("{NOPAL_DIR}/{MANIFEST_FILE}")
}

/// Project-relative module path (used in diagnostics and display).
pub fn module_rel_path(module: Module) -> String {
    format!("{NOPAL_DIR}/{}", module.file_name())
}

pub fn manifest_path(root: &Path) -> PathBuf {
    root.join(NOPAL_DIR).join(MANIFEST_FILE)
}

pub fn module_path(root: &Path, module: Module) -> PathBuf {
    root.join(NOPAL_DIR).join(module.file_name())
}

/// Discover the project root for a launch/config lookup starting at
/// `start`.
///
/// 1. Absolutize `start` (lexical only, no symlink resolution - the same
///    idiom `bundle::bundle_report` uses, since a caller may `chdir` before
///    resolving anything relative to the result).
/// 2. Ask git for the enclosing work tree's toplevel
///    (`git -C start rev-parse --show-toplevel`). Git failing for any
///    reason - `start` is not inside a work tree, git is not installed -
///    means there is no repo to walk: `start` is the root, unchanged.
///    Nopal never walks past a git repo boundary; a `.nopal/` directory
///    that happens to sit above an unrelated repo must not get attached to
///    it.
/// 3. Walk from `start` up to and including the toplevel. The *nearest*
///    directory containing a `.nopal/` **directory** (existence, not the
///    manifest/bundle files inside it) wins - a partially-configured
///    `.nopal/` must anchor the search there so the existing fail-closed
///    diagnostics (`manifest_missing`/`bundle_missing`, D10) fire for it
///    instead of the walk stepping past it to some other ancestor.
/// 4. No `.nopal/` directory anywhere in that range: the toplevel is the
///    root - the landing spot [`crate::scaffold::write_baseline`] uses on
///    a first real launch.
pub fn project_root(start: &Path) -> PathBuf {
    let start_abs = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
    let Some(toplevel) = git_toplevel(&start_abs) else {
        return start_abs;
    };
    // Both sides of the ancestor-walk comparison must go through the same
    // resolution git's own output already had applied to it, or the
    // `/var` vs `/private/var`-style symlink mismatch (see
    // `run_ledger_store::resolve_like_python`) would make `dir == toplevel`
    // never match and silently fall through to the toplevel fallback below
    // even when the walk should have stopped earlier.
    let start_resolved = resolve_like_python(&start_abs);
    walk_to_root(&start_resolved, &toplevel)
}

/// The pure walk half of [`project_root`], split from the git probe so the
/// repo-boundary rules stay unit-testable without a git fixture: the
/// non-ancestor guard below is only reachable through env-driven layouts
/// (`GIT_WORK_TREE`/`GIT_DIR`), and injecting those into `project_root`'s
/// internal git spawn cannot be scoped to a single test under the parallel
/// runner. Both arguments must already be resolved through
/// `resolve_like_python`, or the `/var` vs `/private/var`-style symlink
/// mismatch would keep `dir == toplevel` from ever matching.
fn walk_to_root(start_resolved: &Path, toplevel: &Path) -> PathBuf {
    // Env-driven layouts (GIT_WORK_TREE/GIT_DIR) can make git report a
    // toplevel that is NOT an ancestor of `start`; the loop's `dir ==
    // toplevel` stop would then never trigger and the walk would escape
    // past the repo boundary all the way to `/`. Nopal never walks outside
    // the repo, so a non-ancestor toplevel short-circuits straight to it.
    if !start_resolved.starts_with(toplevel) {
        return toplevel.to_path_buf();
    }
    for dir in start_resolved.ancestors() {
        if dir.join(NOPAL_DIR).is_dir() {
            return dir.to_path_buf();
        }
        if dir == toplevel {
            break;
        }
    }
    toplevel.to_path_buf()
}

/// `git -C start rev-parse --show-toplevel`, resolved through the same path
/// as `run_ledger_store::repo_root`, or `None` when `start` is not inside a
/// git work tree.
fn git_toplevel(start: &Path) -> Option<PathBuf> {
    let raw = git_stdout(start, &["rev-parse", "--show-toplevel"])?;
    Some(resolve_like_python(&PathBuf::from(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
    }

    #[test]
    fn non_ancestor_toplevel_short_circuits_to_toplevel() {
        // The GIT_WORK_TREE/GIT_DIR escape guard, tested on the pure walk:
        // `start` carries its own `.nopal/`, so a missing guard would anchor
        // there instead of short-circuiting to the unrelated toplevel.
        let start = tempfile::tempdir().unwrap();
        fs::create_dir(start.path().join(NOPAL_DIR)).unwrap();
        let toplevel = tempfile::tempdir().unwrap();

        let got = walk_to_root(
            &resolve_like_python(start.path()),
            &resolve_like_python(toplevel.path()),
        );
        assert_eq!(got, resolve_like_python(toplevel.path()));
    }

    #[test]
    fn non_git_dir_anchors_at_start_even_without_nopal() {
        // No git repo means no walk at all: `start` comes back merely
        // absolutized (`std::path::absolute`), not symlink-resolved - unlike
        // every git-repo case below, which normalizes through
        // `resolve_like_python` to match git's own output.
        let temp = tempfile::tempdir().unwrap();
        let root = project_root(temp.path());
        assert_eq!(root, std::path::absolute(temp.path()).unwrap());
    }

    #[test]
    fn git_repo_with_nopal_at_root_found_from_subdir() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        fs::create_dir_all(temp.path().join(".nopal")).unwrap();
        let sub = temp.path().join("sub/dir");
        fs::create_dir_all(&sub).unwrap();

        let root = project_root(&sub);
        assert_eq!(root, resolve_like_python(temp.path()));
    }

    #[test]
    fn git_repo_without_nopal_anywhere_returns_toplevel() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        let sub = temp.path().join("sub/dir");
        fs::create_dir_all(&sub).unwrap();

        let root = project_root(&sub);
        assert_eq!(root, resolve_like_python(temp.path()));
    }

    #[test]
    fn legacy_product_directory_does_not_anchor_discovery() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        let sub = temp.path().join("sub");
        fs::create_dir_all(sub.join(LEGACY_DIR)).unwrap();
        let deeper = sub.join("deeper");
        fs::create_dir_all(&deeper).unwrap();

        let root = project_root(&deeper);
        assert_eq!(root, resolve_like_python(temp.path()));
    }

    #[test]
    fn nested_nopal_wins_over_root_nopal() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        fs::create_dir_all(temp.path().join(".nopal")).unwrap();
        let sub = temp.path().join("sub");
        fs::create_dir_all(sub.join(".nopal")).unwrap();
        let deeper = sub.join("deeper");
        fs::create_dir_all(&deeper).unwrap();

        let root = project_root(&deeper);
        assert_eq!(root, resolve_like_python(&sub));
    }

    #[test]
    fn start_at_toplevel_with_nopal_there_returns_toplevel() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        fs::create_dir_all(temp.path().join(".nopal")).unwrap();

        let root = project_root(temp.path());
        assert_eq!(root, resolve_like_python(temp.path()));
    }
}
