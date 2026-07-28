//! Effect adapter for Git workspace evidence used by continuous enforcement.
//!
//! Nopal Core consumes this immutable snapshot and hashes authorization facts.
//! Git process execution and filesystem traversal stay in the CLI adapter so
//! Core never executes a command or searches `PATH` while deciding.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct WorkspaceObservation {
    pub fingerprint: String,
    pub changed_files: Vec<String>,
}

pub fn apply_evidence(
    run_dir: &Path,
    directive: nopal_core::enforcement::EvidenceDirective,
) -> io::Result<()> {
    use nopal_core::enforcement::EvidenceEffect;

    for effect in directive.effects {
        match effect {
            EvidenceEffect::AppendEvent { event, payload } => {
                nopal_core::run_ledger_store::append_event(run_dir, &event, &payload, None)
                    .map_err(|error| io::Error::other(format!("{error:?}")))?;
            }
            EvidenceEffect::WriteJson {
                relative_path,
                payload,
            } => {
                nopal_core::run_ledger_store::write_json_durable(
                    &run_dir.join(relative_path),
                    &payload,
                )?;
            }
            EvidenceEffect::CreateJson {
                relative_path,
                payload,
            } => create_json_once(&run_dir.join(relative_path), &payload)?,
            EvidenceEffect::RemoveFile {
                relative_path,
                ignore_missing,
            } => {
                if let Err(error) = fs::remove_file(run_dir.join(relative_path))
                    && !(ignore_missing && error.kind() == io::ErrorKind::NotFound)
                {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

fn create_json_once(path: &Path, payload: &nopal_ledger_json::Value) -> io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("evidence path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, payload).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub fn observe(root: &Path) -> io::Result<WorkspaceObservation> {
    reject_git_code_carriers(root)?;
    let credential_helpers = credential_helper_evidence(root)?;
    let configured_pagers = configured_pager_evidence(root)?;
    let git_executable = git_executable()?;
    let head = git_optional(root, &["rev-parse", "--verify", "HEAD"])?;
    let has_head = head.is_some();
    let head = head.unwrap_or_else(|| b"<unborn>".to_vec());
    let diff = if has_head {
        git(root, &["diff", "--binary", "HEAD", "--"])?
    } else {
        git(root, &["diff", "--binary", "--"])?
    };
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let visible = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    reject_external_symlinks(root, &visible)?;

    let mut changed_files = lines(if has_head {
        git(root, &["diff", "--name-only", "HEAD", "--"])?
    } else {
        git(root, &["diff", "--name-only", "--"])?
    });
    changed_files.extend(
        untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned()),
    );
    changed_files.sort();
    changed_files.dedup();

    let mut hasher = Sha256::new();
    frame(
        &mut hasher,
        b"git-executable-path",
        git_executable.as_os_str().as_encoded_bytes(),
    );
    frame(
        &mut hasher,
        b"git-executable-bytes",
        &fs::read(&git_executable)?,
    );
    for (path, bytes) in credential_helpers.into_iter().chain(configured_pagers) {
        frame(
            &mut hasher,
            b"git-helper-path",
            path.as_os_str().as_encoded_bytes(),
        );
        frame(&mut hasher, b"git-helper-bytes", &bytes);
    }
    frame(&mut hasher, b"head", &head);
    frame(&mut hasher, b"diff", &diff);
    let config = git(root, &["config", "--list", "--show-origin"])?;
    frame(&mut hasher, b"git-config", &config);
    hash_hooks(root, &mut hasher)?;
    for path_bytes in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        frame(&mut hasher, b"untracked-path", path_bytes);
        let relative = String::from_utf8_lossy(path_bytes);
        let full_path = root.join(relative.as_ref());
        match fs::symlink_metadata(&full_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                frame(
                    &mut hasher,
                    b"untracked-symlink",
                    fs::read_link(full_path)?.to_string_lossy().as_bytes(),
                );
            }
            Ok(_) => frame(&mut hasher, b"untracked-content", &fs::read(full_path)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                frame(&mut hasher, b"untracked-missing", b"");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(WorkspaceObservation {
        fingerprint: format!("{:x}", hasher.finalize()),
        changed_files,
    })
}

fn reject_git_code_carriers(root: &Path) -> io::Result<()> {
    const DANGEROUS_ENV: &[&str] = &[
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_ASKPASS",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_DIFF_OPTS",
        "GIT_DIR",
        "GIT_EDITOR",
        "GIT_EXEC_PATH",
        "GIT_EXTERNAL_DIFF",
        "GIT_INDEX_FILE",
        "GIT_NAMESPACE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PROXY_COMMAND",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_WORK_TREE",
        "RIPGREP_CONFIG_PATH",
        "SSH_ASKPASS",
    ];
    if let Some(name) = DANGEROUS_ENV.iter().find(|name| {
        if std::env::var_os(name).is_none() {
            return false;
        }
        let debug_clean_system_config = cfg!(debug_assertions)
            && **name == "GIT_CONFIG_NOSYSTEM"
            && std::env::var_os("GIT_CONFIG_NOSYSTEM").as_deref()
                == Some(std::ffi::OsStr::new("1"))
            && std::env::var_os("NOPAL_TEST_CLEAN_GIT_CONFIG").as_deref()
                == Some(std::ffi::OsStr::new("1"));
        !debug_clean_system_config
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ambient {name} can retarget an audited command or execute an untrusted helper"
            ),
        ));
    }
    if std::env::vars_os().any(|(name, _)| {
        name.to_str().is_some_and(|name| {
            name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_")
        })
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ambient GIT_CONFIG_KEY_/GIT_CONFIG_VALUE_ can inject executable Git configuration",
        ));
    }

    let dangerous_config = git_optional(
        root,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^(core\.(editor|fsmonitor|sshcommand|hookspath)|sequence\.editor|difftool\..*\.cmd|mergetool\..*\.cmd|diff\..*\.(command|textconv)|filter\..*\.(clean|smudge|process)|commit\.gpgsign|tag\.gpgsign|remote\..*\.(receivepack|uploadpack)|url\..*\.(insteadof|pushinsteadof)|gpg\.program|gpg\..*\.program)$",
        ],
    )?;
    if dangerous_config.is_some_and(|output| !output.is_empty()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Git configuration contains an executable helper or target-rewriting carrier",
        ));
    }

    let hooks_path = git(root, &["rev-parse", "--git-path", "hooks"])?;
    let hooks_path = String::from_utf8(hooks_path)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let hooks_path = PathBuf::from(hooks_path.trim());
    let hooks_path = if hooks_path.is_absolute() {
        hooks_path
    } else {
        root.join(hooks_path)
    };
    if hooks_path.is_dir() {
        for entry in fs::read_dir(&hooks_path)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("sample") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "executable Git hook {} is an unconfined code carrier",
                            path.display()
                        ),
                    ));
                }
            }
        }
    }

    let remotes = git_optional(
        root,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^remote\..*\.(url|pushurl)$",
        ],
    )?;
    if let Some(remotes) = remotes {
        for field in remotes
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
        {
            let field = String::from_utf8_lossy(field);
            let url = field
                .split_once('\n')
                .map_or(field.as_ref(), |(_, value)| value);
            if url.contains("::") || url.starts_with("ext::") {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("Git remote URL {url:?} selects an untrusted remote helper"),
                ));
            }
        }
    }
    Ok(())
}

fn credential_helper_evidence(root: &Path) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let helpers = git_optional(
        root,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^credential\..*helper$",
        ],
    )?
    .unwrap_or_default();
    let exec_path = git(root, &["--exec-path"])?;
    let exec_path = PathBuf::from(
        String::from_utf8(exec_path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            .trim(),
    )
    .canonicalize()?;
    let mut evidence = Vec::new();
    for field in helpers
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
    {
        let field = String::from_utf8(field.to_vec())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let helper = field.split_once('\n').map_or("", |(_, helper)| helper);
        if helper.is_empty() {
            continue;
        }
        if helper == "!/usr/bin/env gh auth git-credential" {
            let env = PathBuf::from("/usr/bin/env").canonicalize()?;
            let gh = resolve_trusted_executable("gh")?;
            evidence.push((env.clone(), fs::read(env)?));
            evidence.push((gh.clone(), fs::read(gh)?));
            continue;
        }
        if !helper
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Git credential helper {helper:?} is not an exact trusted helper name"),
            ));
        }
        let executable = exec_path
            .join(format!("git-credential-{helper}"))
            .canonicalize()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot resolve Git credential helper {helper:?}: {error}"),
                )
            })?;
        if !executable.starts_with(&exec_path) || !executable.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Git credential helper {helper:?} is outside the trusted Git exec path"),
            ));
        }
        evidence.push((executable.clone(), fs::read(executable)?));
    }
    Ok(evidence)
}

fn configured_pager_evidence(root: &Path) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let pagers = git_optional(
        root,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^(core\.pager|pager\..*)$",
        ],
    )?
    .unwrap_or_default();
    let mut evidence = Vec::new();
    for field in pagers
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
    {
        let field = String::from_utf8(field.to_vec())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let pager = field.split_once('\n').map_or("", |(_, pager)| pager);
        if pager.is_empty() {
            continue;
        }
        if !matches!(pager, "cat" | "less") {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("configured Git pager {pager:?} is not an exact trusted pager"),
            ));
        }
        let executable = resolve_trusted_executable(pager)?;
        evidence.push((executable.clone(), fs::read(executable)?));
    }
    Ok(evidence)
}

fn resolve_trusted_executable(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os("PATH").ok_or_else(|| {
        io::Error::other(format!("cannot resolve {name} executable without PATH"))
    })?;
    let candidate = std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| io::Error::other(format!("cannot resolve {name} executable on PATH")))?
        .canonicalize()?;
    #[cfg(debug_assertions)]
    if std::env::var_os("NOPAL_TEST_PI_BIN").is_some()
        || std::env::var_os("PROOF_GIT_BIN").is_some()
    {
        return Ok(candidate);
    }
    if !is_trusted_system_executable_path(&candidate) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("resolved {name} executable is outside a trusted installation prefix"),
        ));
    }
    Ok(candidate)
}

fn is_trusted_system_executable_path(candidate: &Path) -> bool {
    candidate.starts_with("/usr/bin") || candidate.starts_with("/bin")
}

fn git_executable() -> io::Result<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("PROOF_GIT_BIN") {
        let path = PathBuf::from(path).canonicalize()?;
        if path.is_file() {
            return Ok(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "debug proof Git executable is not a regular file",
        ));
    }
    #[cfg(debug_assertions)]
    if std::env::var_os("NOPAL_TEST_PI_BIN").is_some() {
        let path = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|directory| directory.join("git"))
                    .find(|candidate| candidate.is_file())
            })
            .ok_or_else(|| io::Error::other("debug launch proof cannot resolve Git"))?
            .canonicalize()?;
        return Ok(path);
    }
    resolve_trusted_executable("git")
}

fn git_optional(root: &Path, args: &[&str]) -> io::Result<Option<Vec<u8>>> {
    let output = Command::new(git_executable()?)
        .args(args)
        .current_dir(root)
        .output()?;
    Ok(output.status.success().then_some(output.stdout))
}

fn git(root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new(git_executable()?)
        .args(args)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed while collecting enforcement evidence: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

fn lines(bytes: Vec<u8>) -> Vec<String> {
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn frame(hasher: &mut Sha256, label: &[u8], bytes: &[u8]) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn hash_hooks(root: &Path, hasher: &mut Sha256) -> io::Result<()> {
    let git_dir = git(root, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(String::from_utf8_lossy(&git_dir).trim());
    let hooks = git_dir.join("hooks");
    if !hooks.is_dir() {
        frame(hasher, b"hooks", b"missing");
        return Ok(());
    }
    let mut entries = fs::read_dir(&hooks)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-utf8>");
        frame(hasher, b"hook-name", name.as_bytes());
        frame(hasher, b"hook-content", &fs::read(path)?);
    }
    Ok(())
}

fn reject_external_symlinks(root: &Path, nul_paths: &[u8]) -> io::Result<()> {
    let canonical_root = fs::canonicalize(root)?;
    for path_bytes in nul_paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = String::from_utf8_lossy(path_bytes);
        let path = root.join(relative.as_ref());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let target = fs::canonicalize(&path).map_err(|error| {
            io::Error::other(format!(
                "workspace symlink {} cannot provide stable gate evidence: {error}",
                path.display()
            ))
        })?;
        if target != canonical_root && !target.starts_with(&canonical_root) {
            return Err(io::Error::other(format!(
                "workspace symlink {} escapes the repository proof surface",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::is_trusted_system_executable_path;

    #[test]
    fn executable_trust_excludes_user_writable_usr_local_prefixes() {
        assert!(is_trusted_system_executable_path(Path::new("/usr/bin/git")));
        assert!(is_trusted_system_executable_path(Path::new("/bin/sh")));
        assert!(!is_trusted_system_executable_path(Path::new(
            "/usr/local/bin/git"
        )));
        assert!(!is_trusted_system_executable_path(Path::new(
            "/opt/homebrew/bin/git"
        )));
    }
}
