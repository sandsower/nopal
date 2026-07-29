//! Run-private executable identity for workflow gates.
//!
//! Core selects gates but never searches `PATH` or starts a process. The CLI
//! snapshots the launcher's executable-name mapping into a protected alias
//! directory, hashes every canonical executable, and revalidates that manifest
//! before each private authorization transaction. The Pi adapter executes gates
//! only through this directory, so ambient PATH shadows cannot win later.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nopal_core::gates::Run;
use nopal_core::selection::SelectedGate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct GateRuntime {
    pub bin_dir: PathBuf,
    pub home_dir: PathBuf,
    pub tmp_dir: PathBuf,
    tmp_device: u64,
    tmp_inode: u64,
    /// Stable executable closure bound into Core plans and receipts.
    pub digest: String,
    /// Full run-private manifest identity, including the scratch inode.
    pub runtime_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: String,
    tmp_dir: PathBuf,
    tmp_device: u64,
    tmp_inode: u64,
    entries: Vec<ExecutorEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecutorEntry {
    name: String,
    canonical_path: PathBuf,
    integrity: String,
}

const MANIFEST_RELATIVE: &str = "artifacts/gate-runtime/manifest.json";
const GATE_OUTPUT_LIMIT: usize = 1024 * 1024;
const GATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateExecution {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub killed: bool,
}

pub fn prepare(
    root: &Path,
    run_dir: &Path,
    selected_gates: &[SelectedGate],
) -> io::Result<GateRuntime> {
    #[cfg(not(unix))]
    {
        let _ = (root, run_dir, selected_gates);
        return Err(io::Error::other("gate executor snapshots require unix"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let runtime_dir = run_dir.join("artifacts/gate-runtime");
        let bin_dir = runtime_dir.join("bin");
        let home_dir = runtime_dir.join("home");
        fs::create_dir_all(&bin_dir)?;
        for relative in ["cargo", "npm-cache"] {
            fs::create_dir_all(home_dir.join(relative))?;
        }
        let tmp_guard = tempfile::Builder::new()
            .prefix("nopal-gate-")
            .tempdir_in("/tmp")?;
        let tmp_dir = tmp_guard.path().to_path_buf();
        fs::set_permissions(&tmp_dir, fs::Permissions::from_mode(0o700))?;
        link_read_cache(
            "CARGO_HOME",
            "registry",
            ".cargo/registry",
            &home_dir.join("cargo/registry"),
        )?;
        link_read_cache(
            "CARGO_HOME",
            "git",
            ".cargo/git",
            &home_dir.join("cargo/git"),
        )?;
        link_read_cache("RUSTUP_HOME", "", ".rustup", &home_dir.join("rustup"))?;
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&home_dir, fs::Permissions::from_mode(0o700))?;

        let canonical_root = root.canonicalize()?;
        let path = std::env::var_os("PATH")
            .ok_or_else(|| io::Error::other("gate executor preparation requires PATH"))?;
        let search_path = std::env::split_paths(&path).collect::<Vec<_>>();
        let desired = desired_executor_names(root, selected_gates)?;
        let mut available = BTreeSet::new();
        let mut executors = BTreeMap::<String, PathBuf>::new();
        for name in desired {
            if system_executable(&name).is_some() {
                available.insert(name);
                continue;
            }
            let Some(canonical) = resolve_named_executable(&name, &search_path)? else {
                continue;
            };
            if canonical.starts_with(&canonical_root) || path_is_ephemeral(&canonical) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "gate executable {name:?} resolves through repository or temporary authority {}",
                        canonical.display()
                    ),
                ));
            }
            available.insert(name.clone());
            executors.insert(name, canonical);
        }

        validate_selected_gate_entrypoints(root, selected_gates, &available)?;
        use std::os::unix::fs::MetadataExt as _;
        let tmp_metadata = fs::symlink_metadata(&tmp_dir)?;
        let mut manifest = Manifest {
            version: "nopal.gate-executors/v1".to_owned(),
            tmp_dir: tmp_dir.clone(),
            tmp_device: tmp_metadata.dev(),
            tmp_inode: tmp_metadata.ino(),
            entries: Vec::with_capacity(executors.len()),
        };
        for (name, canonical_path) in executors {
            let alias = bin_dir.join(&name);
            symlink(&canonical_path, &alias)?;
            manifest.entries.push(ExecutorEntry {
                name,
                integrity: file_integrity(&canonical_path)?,
                canonical_path,
            });
        }
        let digest = executor_digest(&manifest)?;
        let runtime_digest = runtime_manifest_digest(&manifest)?;
        let manifest_path = run_dir.join(MANIFEST_RELATIVE);
        let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
        bytes.push(b'\n');
        fs::write(&manifest_path, bytes)?;
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o400))?;
        fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o500))?;
        validate(run_dir, &digest, &runtime_digest)?;
        let kept_tmp_dir = tmp_guard.keep();
        debug_assert_eq!(kept_tmp_dir, tmp_dir);
        Ok(GateRuntime {
            bin_dir,
            home_dir,
            tmp_dir,
            tmp_device: manifest.tmp_device,
            tmp_inode: manifest.tmp_inode,
            digest,
            runtime_digest,
        })
    }
}

pub fn validate(
    run_dir: &Path,
    expected_digest: &str,
    expected_runtime_digest: &str,
) -> io::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (run_dir, expected_digest, expected_runtime_digest);
        return Err(io::Error::other("gate executor snapshots require unix"));
    }
    #[cfg(unix)]
    {
        let manifest_path = run_dir.join(MANIFEST_RELATIVE);
        let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if manifest.version != "nopal.gate-executors/v1" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gate executor manifest has an unsupported version",
            ));
        }
        validate_tmp_dir(&manifest.tmp_dir, manifest.tmp_device, manifest.tmp_inode)?;
        let observed_runtime_digest = runtime_manifest_digest(&manifest)?;
        if observed_runtime_digest != expected_runtime_digest {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "gate runtime manifest does not match the launch binding",
            ));
        }
        let observed_digest = executor_digest(&manifest)?;
        if observed_digest != expected_digest {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "gate executor manifest does not match the launch binding",
            ));
        }
        let bin_dir = run_dir.join("artifacts/gate-runtime/bin");
        for entry in &manifest.entries {
            if !safe_name(&entry.name) || !entry.canonical_path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gate executor manifest contains an invalid path",
                ));
            }
            if file_integrity(&entry.canonical_path)? != entry.integrity {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("gate executable {:?} changed after launch", entry.name),
                ));
            }
            let alias = bin_dir.join(&entry.name);
            let metadata = fs::symlink_metadata(&alias)?;
            if !metadata.file_type().is_symlink() || fs::read_link(&alias)? != entry.canonical_path
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "gate executable alias {:?} changed after launch",
                        entry.name
                    ),
                ));
            }
        }
        Ok(())
    }
}

pub fn load(
    run_dir: &Path,
    expected_digest: &str,
    expected_runtime_digest: &str,
) -> io::Result<GateRuntime> {
    validate(run_dir, expected_digest, expected_runtime_digest)?;
    let manifest_path = run_dir.join(MANIFEST_RELATIVE);
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(GateRuntime {
        bin_dir: run_dir.join("artifacts/gate-runtime/bin"),
        home_dir: run_dir.join("artifacts/gate-runtime/home"),
        tmp_dir: manifest.tmp_dir,
        tmp_device: manifest.tmp_device,
        tmp_inode: manifest.tmp_inode,
        digest: expected_digest.to_owned(),
        runtime_digest: expected_runtime_digest.to_owned(),
    })
}

#[cfg(unix)]
fn validate_tmp_dir(path: &Path, expected_device: u64, expected_inode: u64) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if path.parent() != Some(Path::new("/tmp")) || !name.starts_with("nopal-gate-") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "gate temporary directory is outside the short run-private namespace",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no pointer arguments or memory-safety preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != effective_uid
        || metadata.dev() != expected_device
        || metadata.ino() != expected_inode
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "gate temporary directory identity or permissions changed",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_tmp_dir(_path: &Path, _expected_device: u64, _expected_inode: u64) -> io::Result<()> {
    Err(io::Error::other("gate temporary directories require unix"))
}

pub fn cleanup(runtime: &GateRuntime) -> io::Result<()> {
    validate_tmp_dir(&runtime.tmp_dir, runtime.tmp_device, runtime.tmp_inode)?;
    remove_private_tree(&runtime.tmp_dir)
}

#[cfg(unix)]
fn remove_private_tree(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return fs::remove_file(path);
    }
    if !metadata.file_type().is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).ok();
        return fs::remove_file(path);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        remove_private_tree(&entry?.path())?;
    }
    fs::remove_dir(path)
}

#[cfg(not(unix))]
fn remove_private_tree(_path: &Path) -> io::Result<()> {
    Err(io::Error::other("gate temporary cleanup requires unix"))
}

pub fn execute(
    root: &Path,
    run_dir: &Path,
    runtime: &GateRuntime,
    gate: &SelectedGate,
) -> io::Result<GateExecution> {
    execute_with_timeout(root, run_dir, runtime, gate, GATE_TIMEOUT)
}

fn execute_with_timeout(
    root: &Path,
    run_dir: &Path,
    runtime: &GateRuntime,
    gate: &SelectedGate,
    timeout: Duration,
) -> io::Result<GateExecution> {
    #[cfg(not(unix))]
    {
        let _ = (root, run_dir, runtime, gate, timeout);
        return Err(io::Error::other("gate execution requires unix"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        let expected_bin = run_dir.join("artifacts/gate-runtime/bin");
        let expected_home = run_dir.join("artifacts/gate-runtime/home");
        if runtime.bin_dir != expected_bin || runtime.home_dir != expected_home {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "gate runtime paths do not belong to this run",
            ));
        }
        validate(run_dir, &runtime.digest, &runtime.runtime_digest)?;

        let canonical_root = root.canonicalize()?;
        let cwd = root
            .join(gate.cwd.as_deref().unwrap_or("."))
            .canonicalize()?;
        if !cwd.starts_with(&canonical_root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "gate {:?} working directory escapes the repository",
                    gate.id
                ),
            ));
        }

        let mut command = match &gate.run {
            Run::Command(source) => {
                if source.trim().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("gate {:?} command is empty", gate.id),
                    ));
                }
                let mut command = Command::new("/bin/bash");
                command.args(["--noprofile", "--norc", "-c", source]);
                command
            }
            Run::Argv(argv) => {
                let (executable, args) = argv.split_first().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("gate {:?} argv is empty", gate.id),
                    )
                })?;
                validate_gate_executable_shape(root, gate, executable)?;
                let executable = if executable.contains('/') {
                    gate_script_path(root, gate, executable)?
                } else {
                    let alias = runtime.bin_dir.join(executable);
                    if alias.exists() {
                        alias
                    } else {
                        system_executable(executable).ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                format!(
                                    "gate {:?} executor {executable:?} is absent from the locked runtime",
                                    gate.id
                                ),
                            )
                        })?
                    }
                };
                let mut command = Command::new(executable);
                command.args(args);
                command
            }
        };
        command
            .current_dir(cwd)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", runtime.bin_dir.display()),
            )
            .env("HOME", &runtime.home_dir)
            .env("CARGO_HOME", runtime.home_dir.join("cargo"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("RUSTUP_HOME", runtime.home_dir.join("rustup"))
            .env("TMPDIR", &runtime.tmp_dir)
            .env("npm_config_cache", runtime.home_dir.join("npm-cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("gate stdout pipe is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("gate stderr pipe is unavailable"))?;
        let bytes = Arc::new(AtomicUsize::new(0));
        let exceeded = Arc::new(AtomicBool::new(false));
        let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
        let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
        let stdout_thread = collect_output(
            stdout,
            Arc::clone(&bytes),
            Arc::clone(&exceeded),
            Arc::clone(&stdout_bytes),
        );
        let stderr_thread = collect_output(
            stderr,
            Arc::clone(&bytes),
            Arc::clone(&exceeded),
            Arc::clone(&stderr_bytes),
        );

        let started = Instant::now();
        let mut status = None;
        let mut killed = false;
        let mut timed_out = false;
        loop {
            if exceeded.load(Ordering::Acquire) {
                kill_process_group(&mut child);
                killed = true;
                break;
            }
            if started.elapsed() >= timeout {
                kill_process_group(&mut child);
                killed = true;
                timed_out = true;
                break;
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            // A direct shell may exit while a background descendant keeps its
            // output pipe open. The process group remains supervised until
            // both collectors reach EOF, so descendants cannot outlive the
            // gate timeout by retaining stdout or stderr.
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
            .map_err(|_| io::Error::other("gate stdout collector panicked"))??;
        stderr_thread
            .join()
            .map_err(|_| io::Error::other("gate stderr collector panicked"))??;
        if exceeded.load(Ordering::Acquire) {
            return Err(io::Error::other("gate output exceeded one MiB"));
        }
        if timed_out {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "gate timed out"));
        }
        let status = status.ok_or_else(|| io::Error::other("gate process has no outcome"))?;

        Ok(GateExecution {
            stdout: String::from_utf8_lossy(&stdout_bytes.lock().map_err(lock_error)?).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes.lock().map_err(lock_error)?).into_owned(),
            exit_code: exit_code(status),
            killed,
        })
    }
}

fn collect_output<R: Read + Send + 'static>(
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
            if previous.saturating_add(count) > GATE_OUTPUT_LIMIT {
                exceeded.store(true, Ordering::Release);
                continue;
            }
            output
                .lock()
                .map_err(lock_error)?
                .extend_from_slice(&buffer[..count]);
        }
    })
}

fn lock_error<T>(_error: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("gate output collector lock was poisoned")
}

#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // The child is the leader of the process group requested above.
        // Killing the negative pid terminates descendants as well as the shell.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}

const SHELL_BUILTINS: &[&str] = &[
    ":", ".", "break", "cd", "continue", "echo", "eval", "exec", "exit", "export", "false",
    "printf", "pwd", "read", "readonly", "set", "shift", "test", "times", "trap", "true", "type",
    "ulimit", "umask", "unset", "wait", "[",
];

// Non-system executors used by generated ecosystem gates and their common
// subprocesses. Unknown top-level gate executors are added from the selected
// definitions; checked-in scripts retain `/usr/bin:/bin` for OS utilities.
const ECOSYSTEM_EXECUTORS: &[&str] = &[
    "bun",
    "bundle",
    "cargo",
    "cargo-clippy",
    "cargo-fmt",
    "clippy-driver",
    "cmake",
    "composer",
    "ctest",
    "deno",
    "dotnet",
    "go",
    "gradle",
    "gradlew",
    "java",
    "javac",
    "just",
    "make",
    "meson",
    "mvn",
    "ninja",
    "node",
    "npm",
    "npx",
    "php",
    "pip",
    "pip3",
    "pnpm",
    "poetry",
    "python",
    "python3",
    "pytest",
    "rake",
    "rg",
    "ruby",
    "rustc",
    "rustdoc",
    "rustfmt",
    "swift",
    "swiftc",
    "uv",
    "yarn",
];

fn desired_executor_names(root: &Path, gates: &[SelectedGate]) -> io::Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for gate in gates {
        let executable = gate_executable(gate)?;
        validate_gate_executable_shape(root, gate, executable)?;
        if !SHELL_BUILTINS.contains(&executable) && !executable.contains('/') {
            names.insert(executable.to_owned());
        }
        let source = match &gate.run {
            Run::Command(command) => command.clone(),
            Run::Argv(argv) => argv.join(" "),
        };
        collect_known_names(&source, &mut names);
        if executable.contains('/') {
            let script_path = gate_script_path(root, gate, executable)?;
            let script = fs::read_to_string(script_path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not inspect gate {:?} script: {error}", gate.id),
                )
            })?;
            collect_known_names(&script, &mut names);
        }
    }
    if names.contains("cargo") {
        names.extend(
            [
                "cargo-clippy",
                "cargo-fmt",
                "clippy-driver",
                "rustc",
                "rustdoc",
                "rustfmt",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    if names
        .iter()
        .any(|name| matches!(name.as_str(), "npm" | "npx" | "pnpm" | "yarn"))
    {
        names.insert("node".to_owned());
    }
    Ok(names)
}

fn collect_known_names(source: &str, names: &mut BTreeSet<String>) {
    let tokens = source
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
        })
        .filter(|token| !token.is_empty())
        .collect::<BTreeSet<_>>();
    for name in ECOSYSTEM_EXECUTORS {
        if tokens.contains(name) {
            names.insert((*name).to_owned());
        }
    }
}

fn validate_selected_gate_entrypoints(
    root: &Path,
    gates: &[SelectedGate],
    available: &BTreeSet<String>,
) -> io::Result<()> {
    for gate in gates {
        let executable = gate_executable(gate)?;
        validate_gate_executable_shape(root, gate, executable)?;
        if SHELL_BUILTINS.contains(&executable) || executable.contains('/') {
            continue;
        }
        if !available.contains(executable) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "gate {:?} requires unavailable executor {executable:?}",
                    gate.id
                ),
            ));
        }
    }
    Ok(())
}

fn gate_executable(gate: &SelectedGate) -> io::Result<&str> {
    match &gate.run {
        Run::Command(command) => command.split_whitespace().next(),
        Run::Argv(argv) => argv.first().map(String::as_str),
    }
    .ok_or_else(|| io::Error::other(format!("gate {:?} has no executable", gate.id)))
}

fn validate_gate_executable_shape(
    root: &Path,
    gate: &SelectedGate,
    executable: &str,
) -> io::Result<()> {
    if executable.contains(['\'', '"', '$', '`']) || executable.contains('=') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("gate {:?} has an ambiguous executable envelope", gate.id),
        ));
    }
    if !executable.contains('/') {
        return Ok(());
    }
    let path = Path::new(executable);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("gate {:?} executable must be repository-relative", gate.id),
        ));
    }
    let _ = gate_script_path(root, gate, executable)?;
    Ok(())
}

fn gate_script_path(root: &Path, gate: &SelectedGate, executable: &str) -> io::Result<PathBuf> {
    let canonical_root = root.canonicalize()?;
    let cwd = root
        .join(gate.cwd.as_deref().unwrap_or("."))
        .canonicalize()?;
    if !cwd.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "gate {:?} working directory escapes the repository",
                gate.id
            ),
        ));
    }
    let canonical = cwd.join(executable).canonicalize()?;
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("gate {:?} executable escapes the repository", gate.id),
        ));
    }
    Ok(canonical)
}

#[cfg(unix)]
fn system_executable(name: &str) -> Option<PathBuf> {
    [Path::new("/usr/bin"), Path::new("/bin")]
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(not(unix))]
fn system_executable(_name: &str) -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn resolve_named_executable(name: &str, search_path: &[PathBuf]) -> io::Result<Option<PathBuf>> {
    use std::os::unix::fs::PermissionsExt;

    for directory in search_path {
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join(name);
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        let metadata = canonical.metadata()?;
        if metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && fs::read(&canonical).is_ok()
        {
            return Ok(Some(canonical));
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn link_read_cache(
    variable: &str,
    variable_relative: &str,
    home_relative: &str,
    destination: &Path,
) -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let source = std::env::var_os(variable)
        .map(PathBuf::from)
        .map(|base| base.join(variable_relative))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_relative)));
    let Some(source) = source.filter(|source| source.is_dir()) else {
        return Ok(());
    };
    let source = source.canonicalize()?;
    symlink(source, destination)
}

#[cfg(not(unix))]
fn link_read_cache(
    _variable: &str,
    _variable_relative: &str,
    _home_relative: &str,
    _destination: &Path,
) -> io::Result<()> {
    Ok(())
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

fn path_is_ephemeral(path: &Path) -> bool {
    ["/tmp", "/private/tmp", "/var/tmp", "/private/var/folders"]
        .iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
}

fn file_integrity(path: &Path) -> io::Result<String> {
    Ok(format!("sha256:{:x}", Sha256::digest(fs::read(path)?)))
}

fn executor_digest(manifest: &Manifest) -> io::Result<String> {
    // Plans and receipts bind the stable executable closure. The separate
    // runtime digest below binds the random scratch path and exact inode
    // without making equivalent interactive and headless plans diverge.
    let bytes = serde_json::to_vec(&(manifest.version.as_str(), &manifest.entries))
        .map_err(io::Error::other)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn runtime_manifest_digest(manifest: &Manifest) -> io::Result<String> {
    let bytes = serde_json::to_vec(manifest).map_err(io::Error::other)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn gate(id: &str, command: &str) -> SelectedGate {
        SelectedGate {
            id: id.to_owned(),
            stage: nopal_core::gates::GateStage::PrePr,
            run: Run::Command(command.to_owned()),
            cwd: None,
            autofix: None,
            parallel_safe: None,
            mutates: None,
            via: nopal_core::selection::Via::Default,
        }
    }

    #[test]
    fn bounded_runner_uses_only_the_sanitized_gate_environment() {
        if !cfg!(unix) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let selected = gate(
            "environment",
            "test -z \"${NOPAL_ENFORCEMENT_CAPABILITY_FD:-}\" && test \"$GIT_CONFIG_NOSYSTEM\" = 1 && printf clean",
        );
        let runtime = prepare(root.path(), run.path(), std::slice::from_ref(&selected)).unwrap();
        assert!(runtime.tmp_dir.as_os_str().as_encoded_bytes().len() < 80);
        let result = execute(root.path(), run.path(), &runtime, &selected).unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "clean");
        assert!(!result.killed);
        cleanup(&runtime).unwrap();
    }

    #[test]
    fn bounded_runner_terminates_the_process_group_on_timeout() {
        if !cfg!(unix) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let selected = gate("timeout", "sleep 5");
        let runtime = prepare(root.path(), run.path(), std::slice::from_ref(&selected)).unwrap();
        let error = execute_with_timeout(
            root.path(),
            run.path(),
            &runtime,
            &selected,
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        cleanup(&runtime).unwrap();
    }

    #[test]
    fn bounded_runner_keeps_supervising_descendants_that_hold_output_pipes() {
        if !cfg!(unix) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let selected = gate("background-timeout", "true && sleep 5 &");
        let runtime = prepare(root.path(), run.path(), std::slice::from_ref(&selected)).unwrap();
        let started = Instant::now();
        let error = execute_with_timeout(
            root.path(),
            run.path(),
            &runtime,
            &selected,
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        cleanup(&runtime).unwrap();
    }

    #[test]
    fn bounded_runner_rejects_combined_output_over_one_mib() {
        if !cfg!(unix) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let selected = gate("output", "yes x");
        let runtime = prepare(root.path(), run.path(), std::slice::from_ref(&selected)).unwrap();
        let error = execute(root.path(), run.path(), &runtime, &selected).unwrap_err();
        assert!(error.to_string().contains("exceeded one MiB"));
        cleanup(&runtime).unwrap();
    }

    #[test]
    fn runtime_binding_rejects_a_substituted_scratch_directory() {
        if !cfg!(unix) {
            return;
        }
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let root = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let selected = gate("scratch", "true");
        let runtime = prepare(root.path(), run.path(), std::slice::from_ref(&selected)).unwrap();
        let replacement = tempfile::Builder::new()
            .prefix("nopal-gate-replacement-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(replacement.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let manifest_path = run.path().join(MANIFEST_RELATIVE);
        let original = fs::read(&manifest_path).unwrap();
        let mut manifest: Manifest = serde_json::from_slice(&original).unwrap();
        let metadata = fs::symlink_metadata(replacement.path()).unwrap();
        manifest.tmp_dir = replacement.path().to_path_buf();
        manifest.tmp_device = metadata.dev();
        manifest.tmp_inode = metadata.ino();
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let error = validate(run.path(), &runtime.digest, &runtime.runtime_digest).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        fs::write(&manifest_path, original).unwrap();
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o400)).unwrap();
        cleanup(&runtime).unwrap();
    }

    #[test]
    fn private_runtime_executes_selected_cargo_and_npm_without_ambient_path() {
        if !cfg!(target_os = "macos") && !cfg!(target_os = "linux") {
            return;
        }
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let run = tempfile::tempdir().unwrap();
        let runtime = prepare(
            workspace,
            run.path(),
            &[
                gate("rust", "cargo --version"),
                gate("node", "npm --version"),
            ],
        )
        .unwrap();
        let output = Command::new("/bin/bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "cargo metadata --offline --format-version 1 --no-deps >/dev/null && npm --version",
            ])
            .current_dir(workspace)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", runtime.bin_dir.display()),
            )
            .env("HOME", &runtime.home_dir)
            .env("CARGO_HOME", runtime.home_dir.join("cargo"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("RUSTUP_HOME", runtime.home_dir.join("rustup"))
            .env("npm_config_cache", runtime.home_dir.join("npm-cache"))
            .env("TMPDIR", &runtime.tmp_dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        validate(run.path(), &runtime.digest, &runtime.runtime_digest).unwrap();
        cleanup(&runtime).unwrap();
    }
}
