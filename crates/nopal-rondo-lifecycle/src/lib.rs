//! User-scoped lifecycle supervision for a verified local Rondo Core.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use nopal_rondo_client::{HealthResponse, RondoCoreClient};
use serde::{Deserialize, Serialize};

pub const REPORT_KIND: &str = "nopal.rondo_service/v1";
pub const SUPPORTED_RONDO_RUNTIME_VERSION: &str = "0.1.0";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_RESTARTS: usize = 3;
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_environment() -> io::Result<Self> {
        if let Some(path) = env::var_os("NOPAL_RONDO_STATE_DIR") {
            return Ok(Self::new(PathBuf::from(path)));
        }
        if let Some(path) = env::var_os("XDG_STATE_HOME") {
            return Ok(Self::new(PathBuf::from(path).join("nopal/rondo-core")));
        }
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| Self::new(home.join(".local/state/nopal/rondo-core")))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Nopal cannot resolve a user state directory",
                )
            })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn descriptor(&self) -> PathBuf {
        self.root.join("runtime.json")
    }

    pub fn startup_lock(&self) -> PathBuf {
        self.root.join("startup.lock")
    }

    pub fn host_lock(&self) -> PathBuf {
        self.root.join("host.lock")
    }

    pub fn log(&self) -> PathBuf {
        self.root.join("rondo-core.log")
    }

    fn bootstrap(&self, token: &str) -> PathBuf {
        self.root.join(format!("bootstrap-{token}.json"))
    }

    fn logs_root(&self) -> PathBuf {
        self.root.join("rondo-state")
    }

    fn workspaces_root(&self) -> PathBuf {
        self.root.join("rondo-state/workspaces")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDescriptor {
    pub schema: String,
    pub base_url: String,
    pub runtime_version: String,
    pub instance_id: String,
    pub host_pid: u32,
    pub core_pid: u32,
}

impl RuntimeDescriptor {
    pub fn verified(
        base_url: impl Into<String>,
        runtime_version: impl Into<String>,
        instance_id: impl Into<String>,
        host_pid: u32,
        core_pid: u32,
    ) -> Self {
        Self {
            schema: "nopal.rondo_runtime/v1".to_owned(),
            base_url: base_url.into(),
            runtime_version: runtime_version.into(),
            instance_id: instance_id.into(),
            host_pid,
            core_pid,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LifecycleReport {
    pub kind: &'static str,
    pub ok: bool,
    pub status: String,
    pub health: String,
    pub base_url: Option<String>,
    pub runtime_version: Option<String>,
    pub instance_id: Option<String>,
    pub active_run_count: Option<u64>,
    pub state_path: String,
    pub log_path: String,
    pub diagnostics: Vec<String>,
}

pub struct StartOptions {
    pub paths: StatePaths,
    pub nopal_executable: PathBuf,
    pub rondo_runtime: PathBuf,
    pub timeout: Duration,
}

impl StartOptions {
    pub fn new(paths: StatePaths, nopal_executable: PathBuf, rondo_runtime: PathBuf) -> Self {
        Self {
            paths,
            nopal_executable,
            rondo_runtime,
            timeout: STARTUP_TIMEOUT,
        }
    }
}

pub struct HostOptions {
    pub paths: StatePaths,
    pub rondo_runtime: PathBuf,
    pub timeout: Duration,
}

impl HostOptions {
    pub fn new(paths: StatePaths, rondo_runtime: PathBuf) -> Self {
        Self {
            paths,
            rondo_runtime,
            timeout: STARTUP_TIMEOUT,
        }
    }
}

pub fn health(paths: &StatePaths) -> LifecycleReport {
    let descriptor = match read_descriptor(&paths.descriptor()) {
        Ok(Some(descriptor)) => descriptor,
        Ok(None) => {
            return report(
                paths,
                false,
                "not_started",
                "unknown",
                None,
                vec!["Rondo Core has not been started".to_owned()],
            );
        }
        Err(_) => {
            return report(
                paths,
                false,
                "unverified",
                "invalid_state",
                None,
                vec!["Rondo Core state is unreadable or malformed".to_owned()],
            );
        }
    };

    let observed = RondoCoreClient::new(&descriptor.base_url, HEALTH_TIMEOUT)
        .and_then(|client| client.health());
    match observed {
        Ok(observed) if identity_matches(&descriptor, &observed) => {
            let mut result = report(
                paths,
                observed.ready,
                "running",
                if observed.ready { "ok" } else { "starting" },
                Some(&descriptor),
                Vec::new(),
            );
            result.active_run_count = Some(observed.active_run_count);
            result
        }
        Ok(_) => report(
            paths,
            false,
            "unverified",
            "identity_mismatch",
            Some(&descriptor),
            vec!["Live Rondo Core identity does not match the recorded instance".to_owned()],
        ),
        Err(_) => report(
            paths,
            false,
            "unavailable",
            "unreachable",
            Some(&descriptor),
            vec!["Recorded Rondo Core endpoint is unavailable".to_owned()],
        ),
    }
}

pub fn start(options: &StartOptions) -> io::Result<LifecycleReport> {
    ensure_private_root(&options.paths)?;
    let lock = open_lock(&options.paths.startup_lock())?;
    FileExt::lock_exclusive(&lock)?;

    start_locked(options)
}

pub fn stop(paths: &StatePaths) -> io::Result<LifecycleReport> {
    ensure_private_root(paths)?;
    let lock = open_lock(&paths.startup_lock())?;
    FileExt::lock_exclusive(&lock)?;

    stop_locked(paths)
}

pub fn restart(options: &StartOptions) -> io::Result<LifecycleReport> {
    ensure_private_root(&options.paths)?;
    let lock = open_lock(&options.paths.startup_lock())?;
    FileExt::lock_exclusive(&lock)?;

    let stopped = stop_locked(&options.paths)?;
    if !stopped.ok {
        return Ok(stopped);
    }
    start_locked(options)
}

fn start_locked(options: &StartOptions) -> io::Result<LifecycleReport> {
    let current = health(&options.paths);
    if current.ok {
        return Ok(current);
    }

    remove_if_present(&options.paths.descriptor())?;
    spawn_host(options)?;

    let deadline = Instant::now() + options.timeout;
    while Instant::now() < deadline {
        let current = health(&options.paths);
        if current.ok {
            return Ok(current);
        }
        thread::sleep(POLL_INTERVAL);
    }

    Ok(report(
        &options.paths,
        false,
        "failed",
        "unavailable",
        None,
        vec!["Rondo Core did not become ready before the startup deadline".to_owned()],
    ))
}

fn stop_locked(paths: &StatePaths) -> io::Result<LifecycleReport> {
    let current = health(paths);
    if current.status == "not_started" {
        return Ok(report(paths, true, "stopped", "stopped", None, Vec::new()));
    }
    if !current.ok {
        let mut refused = current;
        refused.diagnostics = vec![
            "Rondo Core stop refused because the recorded instance is not verified".to_owned(),
        ];
        return Ok(refused);
    }
    if current.active_run_count.unwrap_or(0) > 0 {
        let mut protected = current;
        protected.ok = false;
        protected.status = "blocked".to_owned();
        protected.health = "active_runs".to_owned();
        protected.diagnostics =
            vec!["Rondo Core stop refused while verified active runs are present".to_owned()];
        return Ok(protected);
    }

    let descriptor = read_descriptor(&paths.descriptor())?
        .ok_or_else(|| io::Error::other("verified Rondo Core descriptor disappeared"))?;
    terminate_verified_core(descriptor.core_pid)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if !health(paths).ok {
            remove_descriptor_if_owned(&paths.descriptor(), &descriptor)?;
            wait_for_host_exit(paths, deadline)?;
            return Ok(report(paths, true, "stopped", "stopped", None, Vec::new()));
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(report(
        paths,
        false,
        "failed",
        "still_running",
        Some(&descriptor),
        vec!["Rondo Core did not stop before the shutdown deadline".to_owned()],
    ))
}

fn wait_for_host_exit(paths: &StatePaths, deadline: Instant) -> io::Result<()> {
    let host_lock = open_lock(&paths.host_lock())?;
    while Instant::now() < deadline {
        match FileExt::try_lock_exclusive(&host_lock) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Rondo Core host did not exit before the shutdown deadline",
    ))
}

#[cfg(unix)]
fn terminate_verified_core(pid: u32) -> io::Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "verified Rondo Core could not be signaled",
        ))
    }
}

#[cfg(not(unix))]
fn terminate_verified_core(_pid: u32) -> io::Result<()> {
    Err(io::Error::other(
        "Rondo Core stop is not supported on this platform",
    ))
}

pub fn run_host(options: &HostOptions) -> io::Result<()> {
    ensure_private_root(&options.paths)?;
    let host_lock = open_lock(&options.paths.host_lock())?;
    if let Err(error) = FileExt::try_lock_exclusive(&host_lock) {
        return if error.kind() == io::ErrorKind::WouldBlock {
            Ok(())
        } else {
            Err(error)
        };
    }

    for attempt in 0..MAX_RESTARTS {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(250 * attempt as u64));
        }
        match supervise_once(options) {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 == MAX_RESTARTS => return Err(error),
            Err(_) => continue,
        }
    }
    Ok(())
}

fn supervise_once(options: &HostOptions) -> io::Result<()> {
    let token = random_token()?;
    let bootstrap = options.paths.bootstrap(&token);
    remove_if_present(&bootstrap)?;
    let mut child = spawn_core(options, &bootstrap)?;

    let result = wait_for_bootstrap(options, &bootstrap, &mut child);
    remove_if_present(&bootstrap)?;
    let descriptor = match result {
        Ok(descriptor) => descriptor,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    write_descriptor(&options.paths.descriptor(), &descriptor)?;
    let wait_result = child.wait();
    remove_descriptor_if_owned(&options.paths.descriptor(), &descriptor)?;
    wait_result.map(|_| ())
}

fn wait_for_bootstrap(
    options: &HostOptions,
    path: &Path,
    child: &mut Child,
) -> io::Result<RuntimeDescriptor> {
    let deadline = Instant::now() + options.timeout;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Err(io::Error::other("Rondo Core exited before readiness"));
        }
        match fs::read(path) {
            Ok(bytes) => {
                let bootstrap: Bootstrap =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                let observed = RondoCoreClient::new(&bootstrap.base_url, HEALTH_TIMEOUT)
                    .and_then(|client| client.health())
                    .map_err(io::Error::other)?;
                if bootstrap.health != observed
                    || observed.runtime_version != SUPPORTED_RONDO_RUNTIME_VERSION
                    || !observed.ready
                {
                    return Err(io::Error::other(
                        "Rondo Core readiness identity is incompatible",
                    ));
                }
                return Ok(RuntimeDescriptor::verified(
                    bootstrap.base_url,
                    observed.runtime_version,
                    observed.instance_id,
                    std::process::id(),
                    child.id(),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Rondo Core bootstrap timed out",
    ))
}

#[derive(Deserialize)]
struct Bootstrap {
    base_url: String,
    #[serde(flatten)]
    health: HealthResponse,
}

fn spawn_host(options: &StartOptions) -> io::Result<()> {
    rotate_oversized_log(&options.paths.log())?;
    let log = append_log(&options.paths.log())?;
    let mut command = Command::new(&options.nopal_executable);
    command
        .arg("__rondo-host")
        .arg("--state-root")
        .arg(options.paths.root())
        .arg("--rondo-runtime")
        .arg(&options.rondo_runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    detach(&mut command);
    command.spawn().map(|_| ())
}

fn rotate_oversized_log(path: &Path) -> io::Result<()> {
    let size = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if size <= MAX_LOG_BYTES {
        return Ok(());
    }
    let rotated = path.with_extension("log.1");
    remove_if_present(&rotated)?;
    fs::rename(path, rotated)
}

fn spawn_core(options: &HostOptions, bootstrap: &Path) -> io::Result<Child> {
    let log = append_log(&options.paths.log())?;
    let mut command = Command::new(&options.rondo_runtime);
    command
        .arg("core")
        .arg("--port")
        .arg("0")
        .arg("--logs-root")
        .arg(options.paths.logs_root())
        .arg("--workspace-root")
        .arg(options.paths.workspaces_root())
        .arg("--ready-file")
        .arg(bootstrap)
        .current_dir(options.paths.root())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    detach(&mut command);
    command.spawn()
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) {}

fn identity_matches(descriptor: &RuntimeDescriptor, observed: &HealthResponse) -> bool {
    descriptor.schema == "nopal.rondo_runtime/v1"
        && descriptor.runtime_version == SUPPORTED_RONDO_RUNTIME_VERSION
        && descriptor.runtime_version == observed.runtime_version
        && descriptor.instance_id == observed.instance_id
        && observed.service_mode == "trackerless_core"
}

fn report(
    paths: &StatePaths,
    ok: bool,
    status: &str,
    health: &str,
    descriptor: Option<&RuntimeDescriptor>,
    diagnostics: Vec<String>,
) -> LifecycleReport {
    LifecycleReport {
        kind: REPORT_KIND,
        ok,
        status: status.to_owned(),
        health: health.to_owned(),
        base_url: descriptor.map(|value| value.base_url.clone()),
        runtime_version: descriptor.map(|value| value.runtime_version.clone()),
        instance_id: descriptor.map(|value| value.instance_id.clone()),
        active_run_count: None,
        state_path: paths.descriptor().display().to_string(),
        log_path: paths.log().display().to_string(),
        diagnostics,
    }
}

fn ensure_private_root(paths: &StatePaths) -> io::Result<()> {
    fs::create_dir_all(paths.root())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(paths.root(), fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn append_log(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn read_descriptor(path: &Path) -> io::Result<Option<RuntimeDescriptor>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_descriptor(path: &Path, descriptor: &RuntimeDescriptor) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("runtime state path has no parent"))?;
    let temporary = parent.join(format!(".runtime-{}.tmp", random_token()?));
    let bytes = serde_json::to_vec(descriptor).map_err(io::Error::other)?;
    fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temporary, path)
}

fn remove_descriptor_if_owned(path: &Path, expected: &RuntimeDescriptor) -> io::Result<()> {
    if read_descriptor(path)?.as_ref() == Some(expected) {
        remove_if_present(path)?;
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| io::Error::other(format!("secure random generation failed: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
