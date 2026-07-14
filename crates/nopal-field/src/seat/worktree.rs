//! Worktree creation behind a [`CandidateKind::NewWorktree`] pick:
//! `git worktree add` off the repo's default
//! branch, named from the configured dir/branch prefixes.
//!
//! [`CandidateKind::NewWorktree`]: crate::seat::candidates::CandidateKind::NewWorktree

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::seat::config::SeatConfig;

/// Create a worktree for `name` off `project_repo`, naming the
/// directory `<dir_prefix><slug>` and the branch
/// `<branch_prefix><slug>` per `cfg`. The new branch starts at the
/// repo's default branch: `origin/HEAD`'s target, falling back to the
/// currently checked-out branch when there is no such remote-tracking
/// ref (e.g. a repo with no `origin`). Returns the created worktree's
/// path; a git failure surfaces its stderr.
pub fn create(project_repo: &Path, name: &str, cfg: &SeatConfig) -> io::Result<PathBuf> {
    let slug = slugify(name);
    if slug.is_empty() {
        return Err(io::Error::other(
            "worktree name must contain at least one alphanumeric character",
        ));
    }
    let dir_name = format!("{}{}", cfg.worktrees.dir_prefix, slug);
    let branch = format!("{}{}", cfg.worktrees.branch_prefix, slug);
    let dir = project_repo.join(&dir_name);
    let base = default_branch(project_repo)?;

    let output = Command::new("git")
        .arg("-C")
        .arg(project_repo)
        .args(["worktree", "add"])
        .arg(&dir)
        .args(["-b", &branch])
        .arg(&base)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(dir)
}

/// The repo's default branch: `origin/HEAD`'s target with the
/// `origin/` prefix stripped, falling back to the current HEAD branch
/// when there is no `origin/HEAD` ref.
fn default_branch(project_repo: &Path) -> io::Result<String> {
    let origin_head = Command::new("git")
        .arg("-C")
        .arg(project_repo)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()?;
    if origin_head.status.success() {
        let target = String::from_utf8_lossy(&origin_head.stdout)
            .trim()
            .to_owned();
        let branch = target
            .strip_prefix("origin/")
            .map(str::to_owned)
            .unwrap_or(target);
        if !branch.is_empty() {
            return Ok(branch);
        }
    }

    let head = Command::new("git")
        .arg("-C")
        .arg(project_repo)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()?;
    if !head.status.success() {
        return Err(io::Error::other(format!(
            "git symbolic-ref HEAD failed: {}",
            String::from_utf8_lossy(&head.stderr).trim()
        )));
    }
    let branch = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if branch.is_empty() {
        return Err(io::Error::other(
            "could not determine the repo's current branch",
        ));
    }
    Ok(branch)
}

/// lowercase, non-alphanumerics collapse to a single `-`, leading and
/// trailing `-` trimmed.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn slugify_lowercases_and_collapses_non_alphanumerics() {
        assert_eq!(slugify("My Cool Feature!"), "my-cool-feature");
        assert_eq!(slugify("  leading and trailing  "), "leading-and-trailing");
        assert_eq!(slugify("already-slug"), "already-slug");
        assert_eq!(slugify("multi___under -- score"), "multi-under-score");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn create_makes_a_worktree_named_by_prefixes_and_slug() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);

        let cfg = SeatConfig {
            worktrees: crate::seat::config::WorktreesConfig {
                dir_prefix: "nopal-".to_owned(),
                branch_prefix: "nopal/".to_owned(),
            },
            ..Default::default()
        };

        let created = create(&repo, "My New Feature", &cfg).unwrap();
        assert_eq!(created, repo.join("nopal-my-new-feature"));
        assert!(created.join("README.md").exists());

        let branch_out = Command::new("git")
            .arg("-C")
            .arg(&created)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&branch_out.stdout).trim(),
            "nopal/my-new-feature"
        );
    }

    #[test]
    fn create_honors_configured_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);

        let cfg = SeatConfig {
            worktrees: crate::seat::config::WorktreesConfig {
                dir_prefix: "wt-".to_owned(),
                branch_prefix: "feat/".to_owned(),
            },
            ..Default::default()
        };

        let created = create(&repo, "thing", &cfg).unwrap();
        assert_eq!(created, repo.join("wt-thing"));
    }

    #[test]
    fn create_uses_current_branch_when_no_origin_head() {
        // No `origin` remote at all - default_branch must fall back to
        // the checked-out branch (`main`) instead of failing.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);

        let cfg = SeatConfig::default();
        let created = create(&repo, "fallback", &cfg).unwrap();

        let branch_out = Command::new("git")
            .arg("-C")
            .arg(&created)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&branch_out.stdout).trim(),
            "nopal/fallback"
        );
        // And it must have branched from `main`'s commit, not an empty tree.
        assert!(created.join("README.md").exists());
    }

    #[test]
    fn create_rejects_a_name_with_no_alphanumerics() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let cfg = SeatConfig::default();
        let err = create(&repo, "---", &cfg).unwrap_err();
        assert!(err.to_string().contains("alphanumeric"));
    }

    #[test]
    fn create_surfaces_git_stderr_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let cfg = SeatConfig::default();
        // Same slug twice -> second `worktree add` collides on branch name.
        create(&repo, "dup", &cfg).unwrap();
        let err = create(&repo, "dup", &cfg).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
