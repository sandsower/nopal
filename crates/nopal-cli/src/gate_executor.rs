//! Run-private executable identity for workflow gates.
//!
//! Core selects gates but never searches `PATH` or starts a process. The CLI
//! snapshots the launcher's executable-name mapping into a protected alias
//! directory, hashes every canonical executable, and revalidates that manifest
//! before each private authorization transaction. The Pi adapter executes gates
//! only through this directory, so ambient PATH shadows cannot win later.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use nopal_core::gates::Run;
use nopal_core::selection::SelectedGate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct GateRuntime {
    pub bin_dir: PathBuf,
    pub home_dir: PathBuf,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: String,
    entries: Vec<ExecutorEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecutorEntry {
    name: String,
    canonical_path: PathBuf,
    integrity: String,
}

const MANIFEST_RELATIVE: &str = "artifacts/gate-runtime/manifest.json";

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
        for relative in ["cargo", "npm-cache", "tmp"] {
            fs::create_dir_all(home_dir.join(relative))?;
        }
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
        let mut manifest = Manifest {
            version: "nopal.gate-executors/v1".to_owned(),
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
        let digest = manifest_digest(&manifest)?;
        let manifest_path = run_dir.join(MANIFEST_RELATIVE);
        let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
        bytes.push(b'\n');
        fs::write(&manifest_path, bytes)?;
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o400))?;
        fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o500))?;
        validate(run_dir, &digest)?;
        Ok(GateRuntime {
            bin_dir,
            home_dir,
            digest,
        })
    }
}

pub fn validate(run_dir: &Path, expected_digest: &str) -> io::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (run_dir, expected_digest);
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
        let observed_digest = manifest_digest(&manifest)?;
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

fn manifest_digest(manifest: &Manifest) -> io::Result<String> {
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
            .env("RUSTUP_HOME", runtime.home_dir.join("rustup"))
            .env("npm_config_cache", runtime.home_dir.join("npm-cache"))
            .env("TMPDIR", runtime.home_dir.join("tmp"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        validate(run.path(), &runtime.digest).unwrap();
    }
}
