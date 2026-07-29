//! Effect adapter for Git workspace evidence used by continuous enforcement.
//!
//! Nopal Core consumes this immutable snapshot and hashes authorization facts.
//! Git process execution and filesystem traversal stay in the CLI adapter so
//! Core never executes a command or searches `PATH` while deciding.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const GIT_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const WORKSPACE_FILE_LIMIT: usize = 100_000;
const WORKSPACE_FILE_BYTE_LIMIT: u64 = 16 * 1024 * 1024;
const WORKSPACE_TOTAL_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const EVIDENCE_FILE_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const EVIDENCE_TOTAL_BYTE_LIMIT: u64 = 256 * 1024 * 1024;

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
    use nopal_core::run_ledger_store::DurableEffect;

    let effects = directive
        .effects
        .into_iter()
        .map(|effect| match effect {
            EvidenceEffect::AppendEvent { event, payload } => {
                DurableEffect::AppendEvent { event, payload }
            }
            EvidenceEffect::WriteJson {
                relative_path,
                payload,
            } => DurableEffect::WriteJson {
                relative_path,
                payload,
            },
            EvidenceEffect::CreateJson {
                relative_path,
                payload,
            } => DurableEffect::CreateJson {
                relative_path,
                payload,
            },
            EvidenceEffect::RemoveFile {
                relative_path,
                ignore_missing,
            } => DurableEffect::RemoveFile {
                relative_path,
                ignore_missing,
            },
        })
        .collect::<Vec<_>>();
    nopal_core::run_ledger_store::commit_effect_batch(run_dir, &effects)
        .map(|_| ())
        .map_err(|error| io::Error::other(format!("{error:?}")))
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
    if changed_files.len() > WORKSPACE_FILE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace has {} changed files; limit is {WORKSPACE_FILE_LIMIT}",
                changed_files.len()
            ),
        ));
    }

    let mut hasher = Sha256::new();
    frame(
        &mut hasher,
        b"git-executable-path",
        git_executable.as_os_str().as_encoded_bytes(),
    );
    let mut observed_files = 0usize;
    let mut evidence_file_bytes = 0u64;
    hash_bounded_file_frame(
        &mut hasher,
        b"git-executable-bytes",
        &git_executable,
        &mut evidence_file_bytes,
        EVIDENCE_FILE_BYTE_LIMIT,
        EVIDENCE_TOTAL_BYTE_LIMIT,
        "workspace evidence",
    )?;
    for path in credential_helpers.into_iter().chain(configured_pagers) {
        observed_files = observed_files.saturating_add(1);
        if observed_files > WORKSPACE_FILE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace evidence has more than {WORKSPACE_FILE_LIMIT} files"),
            ));
        }
        frame(
            &mut hasher,
            b"git-helper-path",
            path.as_os_str().as_encoded_bytes(),
        );
        hash_bounded_file_frame(
            &mut hasher,
            b"git-helper-bytes",
            &path,
            &mut evidence_file_bytes,
            EVIDENCE_FILE_BYTE_LIMIT,
            EVIDENCE_TOTAL_BYTE_LIMIT,
            "workspace evidence",
        )?;
    }
    frame(&mut hasher, b"head", &head);
    frame(&mut hasher, b"diff", &diff);
    let config = git(root, &["config", "--list", "--show-origin"])?;
    frame(&mut hasher, b"git-config", &config);
    hash_hooks(
        root,
        &mut hasher,
        &mut observed_files,
        &mut evidence_file_bytes,
    )?;
    let mut observed_file_bytes = 0u64;
    for path_bytes in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        observed_files = observed_files.saturating_add(1);
        if observed_files > WORKSPACE_FILE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace has more than {WORKSPACE_FILE_LIMIT} untracked files"),
            ));
        }
        frame(&mut hasher, b"untracked-path", path_bytes);
        let relative = String::from_utf8_lossy(path_bytes);
        let full_path = root.join(relative.as_ref());
        match fs::symlink_metadata(&full_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(full_path)?;
                let target = target.as_os_str().as_encoded_bytes();
                account_workspace_bytes(&mut observed_file_bytes, target.len() as u64)?;
                frame(&mut hasher, b"untracked-symlink", target);
            }
            Ok(metadata) if metadata.file_type().is_file() => hash_file_frame(
                &mut hasher,
                b"untracked-content",
                &full_path,
                &mut observed_file_bytes,
            )?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "untracked path {} is not a regular file or symlink",
                        full_path.display()
                    ),
                ));
            }
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
        let clean_system_config = **name == "GIT_CONFIG_NOSYSTEM"
            && std::env::var_os("GIT_CONFIG_NOSYSTEM").as_deref()
                == Some(std::ffi::OsStr::new("1"));
        !clean_system_config
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

fn credential_helper_evidence(root: &Path) -> io::Result<Vec<PathBuf>> {
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
            evidence.push(env);
            evidence.push(gh);
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
        evidence.push(executable);
    }
    Ok(evidence)
}

fn configured_pager_evidence(root: &Path) -> io::Result<Vec<PathBuf>> {
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
        evidence.push(executable);
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

#[derive(Debug)]
struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn collect_bounded_output<R: Read + Send + 'static>(
    mut reader: R,
    total: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
    output: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            let previous = total.fetch_add(count, Ordering::AcqRel);
            if previous.saturating_add(count) > GIT_OUTPUT_LIMIT {
                exceeded.store(true, Ordering::Release);
                continue;
            }
            output
                .lock()
                .map_err(|_| io::Error::other("Git output lock was poisoned"))?
                .extend_from_slice(&buffer[..count]);
        }
    })
}

fn command_output_bounded(command: &mut Command) -> io::Result<BoundedOutput> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Git stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Git stderr pipe is unavailable"))?;
    let total = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
    let stdout_thread = collect_bounded_output(
        stdout,
        Arc::clone(&total),
        Arc::clone(&exceeded),
        Arc::clone(&stdout_bytes),
    );
    let stderr_thread = collect_bounded_output(
        stderr,
        Arc::clone(&total),
        Arc::clone(&exceeded),
        Arc::clone(&stderr_bytes),
    );
    let started = Instant::now();
    let mut status = None;
    let mut timed_out = false;
    loop {
        if exceeded.load(Ordering::Acquire) || started.elapsed() >= GIT_TIMEOUT {
            timed_out = started.elapsed() >= GIT_TIMEOUT;
            #[cfg(unix)]
            if let Ok(pid) = i32::try_from(child.id()) {
                // SAFETY: a negative pid addresses the process group created above.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let _ = child.kill();
            break;
        }
        if status.is_none() {
            status = child.try_wait()?;
        }
        if status.is_some() && stdout_thread.is_finished() && stderr_thread.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if status.is_none() {
        status = Some(child.wait()?);
    }
    stdout_thread
        .join()
        .map_err(|_| io::Error::other("Git stdout collector panicked"))??;
    stderr_thread
        .join()
        .map_err(|_| io::Error::other("Git stderr collector panicked"))??;
    if exceeded.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Git evidence output exceeds {GIT_OUTPUT_LIMIT} bytes"),
        ));
    }
    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Git evidence collection timed out",
        ));
    }
    let stdout = Arc::try_unwrap(stdout_bytes)
        .map_err(|_| io::Error::other("Git stdout remained shared"))?
        .into_inner()
        .map_err(|_| io::Error::other("Git stdout lock was poisoned"))?;
    let stderr = Arc::try_unwrap(stderr_bytes)
        .map_err(|_| io::Error::other("Git stderr remained shared"))?
        .into_inner()
        .map_err(|_| io::Error::other("Git stderr lock was poisoned"))?;
    Ok(BoundedOutput {
        status: status.ok_or_else(|| io::Error::other("Git process has no outcome"))?,
        stdout,
        stderr,
    })
}

fn configure_git_command(command: &mut Command, root: &Path) {
    // System Git configuration belongs to the host image rather than the
    // audited user/worktree contract. Ignoring it both prevents hidden code
    // carriers and keeps every subsequent Git observation on this same seam.
    command.current_dir(root).env("GIT_CONFIG_NOSYSTEM", "1");
}

fn git_command(root: &Path) -> io::Result<Command> {
    let mut command = Command::new(git_executable()?);
    configure_git_command(&mut command, root);
    Ok(command)
}

fn git_optional(root: &Path, args: &[&str]) -> io::Result<Option<Vec<u8>>> {
    let mut command = git_command(root)?;
    command.args(args);
    let output = command_output_bounded(&mut command)?;
    Ok(output.status.success().then_some(output.stdout))
}

fn git(root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let mut command = git_command(root)?;
    command.args(args);
    let output = command_output_bounded(&mut command)?;
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
    frame_header(hasher, label, bytes.len() as u64);
    hasher.update(bytes);
}

fn frame_header(hasher: &mut Sha256, label: &[u8], length: u64) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update(length.to_be_bytes());
}

fn account_bounded_bytes(
    total: &mut u64,
    additional: u64,
    file_limit: u64,
    total_limit: u64,
    surface: &str,
) -> io::Result<()> {
    if additional > file_limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{surface} file is {additional} bytes; limit is {file_limit}"),
        ));
    }
    *total = total.checked_add(additional).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{surface} byte count overflowed"),
        )
    })?;
    if *total > total_limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{surface} exceeds {total_limit} bytes"),
        ));
    }
    Ok(())
}

fn account_workspace_bytes(total: &mut u64, additional: u64) -> io::Result<()> {
    account_bounded_bytes(
        total,
        additional,
        WORKSPACE_FILE_BYTE_LIMIT,
        WORKSPACE_TOTAL_BYTE_LIMIT,
        "workspace untracked content",
    )
}

fn hash_file_frame(
    hasher: &mut Sha256,
    label: &[u8],
    path: &Path,
    total: &mut u64,
) -> io::Result<()> {
    hash_bounded_file_frame(
        hasher,
        label,
        path,
        total,
        WORKSPACE_FILE_BYTE_LIMIT,
        WORKSPACE_TOTAL_BYTE_LIMIT,
        "workspace untracked content",
    )
}

fn hash_bounded_file_frame(
    hasher: &mut Sha256,
    label: &[u8],
    path: &Path,
    total: &mut u64,
    file_limit: u64,
    total_limit: u64,
    surface: &str,
) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("workspace path {} changed identity", path.display()),
        ));
    }
    let length = metadata.len();
    account_bounded_bytes(total, length, file_limit, total_limit, surface)?;
    frame_header(hasher, label, length);
    let mut remaining = length;
    let mut buffer = [0u8; 8192];
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = file.read(&mut buffer[..wanted])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace file {} changed while hashing", path.display()),
            ));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if file.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("workspace file {} changed while hashing", path.display()),
        ));
    }
    Ok(())
}

fn hash_hooks(
    root: &Path,
    hasher: &mut Sha256,
    observed_files: &mut usize,
    observed_bytes: &mut u64,
) -> io::Result<()> {
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
        *observed_files = observed_files.saturating_add(1);
        if *observed_files > WORKSPACE_FILE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace evidence has more than {WORKSPACE_FILE_LIMIT} files"),
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-utf8>");
        frame(hasher, b"hook-name", name.as_bytes());
        hash_bounded_file_frame(
            hasher,
            b"hook-content",
            &path,
            observed_bytes,
            EVIDENCE_FILE_BYTE_LIMIT,
            EVIDENCE_TOTAL_BYTE_LIMIT,
            "workspace evidence",
        )?;
    }
    Ok(())
}

fn reject_external_symlinks(root: &Path, nul_paths: &[u8]) -> io::Result<()> {
    let canonical_root = fs::canonicalize(root)?;
    for (index, path_bytes) in nul_paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .enumerate()
    {
        if index >= WORKSPACE_FILE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("workspace has more than {WORKSPACE_FILE_LIMIT} visible files"),
            ));
        }
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
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use sha2::Sha256;

    use super::{
        WORKSPACE_FILE_BYTE_LIMIT, command_output_bounded, configure_git_command, hash_file_frame,
        is_trusted_system_executable_path,
    };

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

    #[test]
    fn audited_git_commands_ignore_ambient_system_configuration() {
        let mut command = Command::new("unused-test-program");
        configure_git_command(&mut command, Path::new("."));
        let clean_system_config = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CONFIG_NOSYSTEM")
            .and_then(|(_, value)| value);
        assert_eq!(clean_system_config, Some(std::ffi::OsStr::new("1")));
    }

    #[test]
    fn subprocess_output_is_stopped_at_the_combined_limit() {
        if !Path::new("/usr/bin/yes").is_file() {
            return;
        }
        let mut command = Command::new("/usr/bin/yes");
        let error = command_output_bounded(&mut command).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn untracked_file_hashing_rejects_one_file_over_the_bound_before_reading_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.bin");
        let file = fs::File::create(&path).unwrap();
        file.set_len(WORKSPACE_FILE_BYTE_LIMIT + 1).unwrap();
        let mut hasher = Sha256::default();
        let mut total = 0;
        let error = hash_file_frame(&mut hasher, b"content", &path, &mut total).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(total, 0);
    }
}
