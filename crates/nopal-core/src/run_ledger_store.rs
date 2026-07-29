//! Run ledger store - the effectful half of `run-ledger-v1`.
//!
//! Owns everything `run_ledger` deliberately does not: the durable atomic
//! write path (temp file, fsync, rename, fsync parent dir), the `.lock`
//! exclusive-flock guard (std `File::lock`, non-reentrant - sequence holds,
//! never nest), attempt-dir allocation, run discovery under
//! `${BEISLID_STATE_DIR:-~/.local/state/beislid}/runs/<flow>/<repo_hash>/`,
//! and the git subprocess probes (repo root, root-commit hash, branch).
//!
//! `NOPAL_LEDGER_TEST_EPOCH` / `NOPAL_LEDGER_TEST_TOKEN` pin the clock and the
//! run-id token; they exist for the interop write-equivalence tests and are
//! honored unconditionally because a pinned ledger is still a valid ledger.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use cap_fs_ext::OpenOptionsExt as _;
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use sha2::{Digest as _, Sha256};

use ledger_json::Value;
use nopal_ledger_json as ledger_json;

use crate::diagnostics::{Code, Diagnostic};
use crate::run_ledger as core;
use crate::run_ledger::Status;

/// Store failures split into hard IO (CLI exit 2) and domain problems that
/// belong in the envelope as diagnostics (CLI exit 1).
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Domain(Diagnostic),
}

impl From<io::Error> for StoreError {
    fn from(err: io::Error) -> StoreError {
        StoreError::Io(err)
    }
}

pub(crate) fn domain(
    code: Code,
    path: impl Into<String>,
    message: impl Into<String>,
) -> StoreError {
    StoreError::Domain(Diagnostic::error(code, path, message))
}

// ---------------------------------------------------------------------------
// Effect seams: clock, randomness, environment
// ---------------------------------------------------------------------------

pub(crate) fn epoch_now() -> i64 {
    if let Some(pinned) = std::env::var_os("NOPAL_LEDGER_TEST_EPOCH")
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.parse::<i64>().ok())
    {
        return pinned;
    }
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

pub(crate) fn now_iso() -> String {
    core::iso_utc(epoch_now())
}

pub(crate) fn now_stamp() -> String {
    core::stamp_utc(epoch_now())
}

/// Python `secrets.token_hex(n)`.
pub(crate) fn token_hex(bytes: usize) -> String {
    if let Some(pinned) =
        std::env::var_os("NOPAL_LEDGER_TEST_TOKEN").and_then(|v| v.into_string().ok())
    {
        return pinned;
    }
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // Randomness only makes ids unique; the collision loop below is the
        // correctness backstop, so a degraded fallback beats aborting.
        let fallback = epoch_now().to_le_bytes();
        buf.iter_mut()
            .zip(fallback.iter().cycle())
            .for_each(|(b, f)| *b = *f);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Python `Path.resolve()` (non-strict): absolutize against the cwd, then
/// resolve symlinks for the longest existing prefix and reattach the rest.
/// The Python tool resolves both the state dir and the repo root, and those
/// resolved forms are embedded in `run.json`, so skipping this would make
/// the two tools write different bytes into the same tree (macOS `/var` vs
/// `/private/var` being the everyday case).
///
/// `pub(crate)`: `discover::project_root` reuses this to put the
/// starting dir and the `git rev-parse --show-toplevel` output through the
/// same resolution before comparing them - otherwise the `/var` vs
/// `/private/var` mismatch above would make an exact-match ancestor walk
/// silently miss the toplevel.
pub(crate) fn resolve_like_python(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut existing = absolute.clone();
    let mut tail = Vec::new();
    loop {
        match fs::canonicalize(&existing) {
            Ok(resolved) => {
                let mut result = resolved;
                for component in tail.iter().rev() {
                    result.push(component);
                }
                return result;
            }
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    tail.push(name.to_owned());
                    existing = parent.to_path_buf();
                }
                _ => return absolute,
            },
        }
    }
}

/// Resolved effect context for one invocation.
pub struct LedgerEnv {
    pub state_dir: PathBuf,
    pub repo: PathBuf,
    pub repo_hash: String,
}

impl LedgerEnv {
    /// `dir` is the caller's repo root candidate (the CLI `--dir`);
    /// `state_dir_flag` beats `BEISLID_STATE_DIR` beats the XDG default.
    pub fn discover(dir: &Path, state_dir_flag: Option<&Path>) -> LedgerEnv {
        let state_dir = state_dir_flag.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("BEISLID_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(".local")
                        .join("state")
                        .join("beislid")
                })
        });
        let repo = repo_root(dir);
        let repo_hash = repo_hash_of(&repo);
        LedgerEnv {
            state_dir: resolve_like_python(&state_dir),
            repo,
            repo_hash,
        }
    }

    pub fn run_root(&self, flow: &str) -> PathBuf {
        self.state_dir.join("runs").join(flow).join(&self.repo_hash)
    }

    /// Root for this repo's persisted policy asks. Sibling of
    /// `runs/`, keyed by repo hash (not flow): an ask belongs to a session or
    /// run, not a flow. A field lists a repo's asks by scanning this dir and
    /// can sweep every repo with `asks/*/`.
    pub fn ask_root(&self) -> PathBuf {
        self.state_dir.join("asks").join(&self.repo_hash)
    }
}

// ---------------------------------------------------------------------------
// Git probes
// ---------------------------------------------------------------------------
//
// `pub(crate)`: `discover::project_root` reuses `git_stdout` for its
// own `rev-parse --show-toplevel` probe rather than duplicating the
// subprocess/UTF-8/empty-output handling below.

pub(crate) fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() { None } else { Some(text) }
}

fn repo_root(dir: &Path) -> PathBuf {
    let root = git_stdout(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf());
    resolve_like_python(&root)
}

/// First 12 chars of the lexically-first root commit, else `unknown-repo`.
fn repo_hash_of(repo: &Path) -> String {
    match git_stdout(repo, &["rev-list", "--max-parents=0", "HEAD"]) {
        Some(out) => {
            let mut roots: Vec<&str> = out
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            roots.sort_unstable();
            roots
                .first()
                .map(|r| r.chars().take(12).collect())
                .unwrap_or_else(|| "unknown-repo".to_owned())
        }
        None => "unknown-repo".to_owned(),
    }
}

fn current_branch(repo: &Path) -> String {
    git_stdout(repo, &["branch", "--show-current"]).unwrap_or_else(|| "unknown".to_owned())
}

// ---------------------------------------------------------------------------
// Durable writes and locking
// ---------------------------------------------------------------------------

fn open_directory_nofollow(path: &Path, create: bool) -> io::Result<Dir> {
    let mut absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // macOS exposes these root-owned compatibility aliases as symlinks.
    // Normalize only the fixed platform aliases before enforcing no-follow
    // traversal for every caller-controlled state component.
    #[cfg(target_os = "macos")]
    for (alias, canonical) in [
        (Path::new("/var"), Path::new("/private/var")),
        (Path::new("/tmp"), Path::new("/private/tmp")),
        (Path::new("/etc"), Path::new("/private/etc")),
    ] {
        if let Ok(relative) = absolute.strip_prefix(alias) {
            absolute = canonical.join(relative);
            break;
        }
    }
    let mut current = Dir::open_ambient_dir(Path::new("/"), ambient_authority())?;
    for component in absolute.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "ledger directory contains an unsafe component: {}",
                    path.display()
                ),
            ));
        };
        match current.open_dir_nofollow(name) {
            Ok(next) => current = next,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match current.create_dir(name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                current = current.open_dir_nofollow(name)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(current)
}

fn ensure_directory_chain(path: &Path) -> io::Result<()> {
    let _ = open_directory_nofollow(path, true).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("create no-follow directory {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Open once without following a final symlink, validate that exact file
/// descriptor, and read at most `limit` bytes. Callers parse or redact only
/// the returned bounded bytes, so a pathname swap cannot change the input
/// after validation.
pub fn read_bounded_regular_file(path: &Path, limit: usize, label: &str) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
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
            format!("{label} must be a regular non-symlink file"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{label} must not be multiply linked"),
            ));
        }
    }
    if metadata.len() > limit as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} exceeds {limit} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(limit).min(limit));
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} exceeds {limit} bytes"),
        ));
    }
    Ok(bytes)
}

/// Python `write_json`: temp file in the target dir, write, fsync, rename
/// over the target, fsync the parent dir; the temp file never survives.
pub fn write_json_durable(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_directory_chain(parent)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("payload.json");
    let tmp = parent.join(format!(".{name}.{}.tmp", token_hex(4)));
    let result = (|| -> io::Result<()> {
        let mut file = create_private_file(&tmp)?;
        file.write_all(core::json_pretty(value).as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() && tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_text_durable(path: &Path, text: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_directory_chain(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("evidence.txt");
    let tmp = parent.join(format!(".{name}.{}.tmp", token_hex(4)));
    let result = (|| -> io::Result<()> {
        let mut file = create_private_file(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() && tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn read_json(path: &Path) -> Result<Value, StoreError> {
    let size = fs::metadata(path)?.len();
    if size > TRANSACTION_BYTE_LIMIT {
        return Err(domain(
            Code::LedgerLimitExceeded,
            path.display().to_string(),
            format!("ledger JSON is {size} bytes; limit is {TRANSACTION_BYTE_LIMIT}"),
        ));
    }
    let text = fs::read_to_string(path)?;
    ledger_json::from_str(&text).map_err(|err| {
        domain(
            Code::LedgerEntryInvalid,
            path.display().to_string(),
            format!("unreadable ledger JSON: {err}"),
        )
    })
}

fn truncate_utf8_bytes(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn ensure_value_limit(
    path: impl Into<String>,
    value: &Value,
    limit: usize,
) -> Result<(), StoreError> {
    let size = core::json_line(value).len();
    if size > limit {
        return Err(domain(
            Code::LedgerLimitExceeded,
            path,
            format!("ledger value is {size} bytes; limit is {limit} bytes"),
        ));
    }
    Ok(())
}

/// Exclusive cross-process lock on `<run_dir>/.lock`.
///
/// The lock lives on a dedicated file because `write_json_durable` replaces
/// JSON files by rename, which would detach a lock held on the old inode.
/// Like the Python guard it is NOT reentrant: acquiring while the same
/// process already holds it deadlocks. Callers sequence holds, never nest.
pub struct RunLock {
    file: File,
    _directory: Dir,
}

impl RunLock {
    pub fn acquire(run_dir: &Path) -> io::Result<RunLock> {
        let directory = match open_directory_nofollow(run_dir, false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_directory_chain(run_dir).map_err(|error| {
                    io::Error::new(error.kind(), format!("create run directory chain: {error}"))
                })?;
                open_directory_nofollow(run_dir, false).map_err(|error| {
                    io::Error::new(error.kind(), format!("reopen run directory chain: {error}"))
                })?
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("open run directory without symlinks: {error}"),
                ));
            }
        };
        let mut options = cap_std::fs::OpenOptions::new();
        options.create(true).append(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        options.mode(0o600);
        let open_deadline = Instant::now() + Duration::from_secs(1);
        let file = loop {
            match directory.open_with(".lock", &options) {
                Ok(file) => break file.into_std(),
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && Instant::now() < open_deadline =>
                {
                    // Concurrent first-open calls can race inside the
                    // no-follow create path. Retry against the same held
                    // directory capability rather than falling back to an
                    // ambient pathname.
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("open run lock: {error}"),
                    ));
                }
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if file.metadata()?.nlink() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "run ledger lock must not be multiply linked",
                ));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(RunLock {
                        file,
                        _directory: directory,
                    });
                }
                Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out acquiring the run ledger lock",
                    ));
                }
                Err(fs::TryLockError::Error(err)) => return Err(err),
            }
        }
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

// ---------------------------------------------------------------------------
// Authoritative append-only transaction journal
// ---------------------------------------------------------------------------

const RUN_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const RUN_FILE_LIMIT: usize = 100_000;
const RUN_DEPTH_LIMIT: usize = 16;
const JOURNAL_ORDINARY_LIMIT: u64 = 100_000;
const JOURNAL_FILE_LIMIT: usize = 100_002;
const TRANSACTION_BYTE_LIMIT: u64 = 4 * 1024 * 1024;
const FLOW_SCAN_LIMIT: usize = 256;
const RUN_SCAN_LIMIT: usize = 4096;

#[derive(Clone)]
struct ProjectionEffect {
    path: String,
    operation: &'static str,
    content: String,
}

impl ProjectionEffect {
    fn replace(path: impl Into<String>, content: impl Into<String>) -> ProjectionEffect {
        ProjectionEffect {
            path: path.into(),
            operation: "replace",
            content: content.into(),
        }
    }

    fn append(path: impl Into<String>, content: impl Into<String>) -> ProjectionEffect {
        ProjectionEffect {
            path: path.into(),
            operation: "append",
            content: content.into(),
        }
    }

    fn remove(path: impl Into<String>) -> ProjectionEffect {
        ProjectionEffect {
            path: path.into(),
            operation: "remove",
            content: String::new(),
        }
    }
}

struct JournalState {
    revision: u64,
    digest: Option<String>,
    projections: BTreeMap<String, Vec<u8>>,
    history: BTreeMap<String, BTreeSet<String>>,
    tombstones: BTreeSet<String>,
    identities: BTreeMap<String, (String, String, u64, String, Value)>,
}

impl JournalState {
    fn empty() -> JournalState {
        JournalState {
            revision: 0,
            digest: None,
            projections: BTreeMap::new(),
            history: BTreeMap::new(),
            tombstones: BTreeSet::new(),
            identities: BTreeMap::new(),
        }
    }

    fn json(&self, path: &str, run_dir: &Path) -> Result<Value, StoreError> {
        let bytes = self.projections.get(path).ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                run_dir.join(path).display().to_string(),
                "journal projection is missing",
            )
        })?;
        let text = std::str::from_utf8(bytes).map_err(|err| {
            domain(
                Code::LedgerEntryInvalid,
                run_dir.join(path).display().to_string(),
                format!("journal projection is not UTF-8: {err}"),
            )
        })?;
        ledger_json::from_str(text).map_err(|err| {
            domain(
                Code::LedgerEntryInvalid,
                run_dir.join(path).display().to_string(),
                format!("journal projection is malformed JSON: {err}"),
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitOutcome {
    pub revision: u64,
    pub digest: String,
    pub replayed: bool,
    pub result: Value,
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = core::json_pretty(value).into_bytes();
    bytes.push(b'\n');
    bytes
}

fn safe_projection_path(path: &str) -> bool {
    let candidate = Path::new(path);
    let reserved_runtime = Path::new("artifacts/gate-runtime");
    !candidate.as_os_str().is_empty()
        && !candidate.is_absolute()
        && candidate != reserved_runtime
        && !candidate.starts_with(reserved_runtime)
        && candidate.components().all(|component| {
            matches!(component, Component::Normal(_))
                && component.as_os_str() != ".lock"
                && component.as_os_str() != "transactions"
        })
}

fn effect_value(effect: &ProjectionEffect, before: &[u8]) -> Value {
    ledger_json::json!({
        "path": effect.path,
        "operation": effect.operation,
        "offset": if effect.operation == "append" { Value::from(before.len() as u64) } else { Value::Null },
        "before_digest": sha256(before),
        "content": effect.content,
    })
}

fn transaction_digest(value: &Value) -> String {
    sha256(core::json_line(value).as_bytes())
}

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ledger directory is not a real directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path),
        Err(error) => Err(error),
    }
}

fn publish_transaction(run_dir: &Path, revision: u64, record: &Value) -> io::Result<PathBuf> {
    let journal_dir = run_dir.join("transactions");
    ensure_real_directory(&journal_dir)?;
    let encoded_len = core::json_pretty(record).len().saturating_add(1);
    if encoded_len > TRANSACTION_BYTE_LIMIT as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("transaction is {encoded_len} bytes; limit is {TRANSACTION_BYTE_LIMIT}"),
        ));
    }
    let target = journal_dir.join(format!("{revision:020}.json"));
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("transaction revision already exists: {}", target.display()),
        ));
    }
    write_json_durable(&target, record)?;
    Ok(target)
}

fn projection_metadata(path: &Path) -> Result<Option<fs::Metadata>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(domain(
                    Code::LedgerEntryInvalid,
                    path.display().to_string(),
                    "ledger evidence must be a regular file",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if metadata.nlink() != 1 {
                    return Err(domain(
                        Code::LedgerEntryInvalid,
                        path.display().to_string(),
                        "ledger evidence must not be multiply linked",
                    ));
                }
            }
            Ok(Some(metadata))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn validate_projection_tree(run_dir: &Path) -> Result<(), StoreError> {
    let mut stack = vec![(run_dir.to_path_buf(), 0usize)];
    let mut files = 0usize;
    let mut bytes = 0u64;
    while let Some((dir, depth)) = stack.pop() {
        if depth > RUN_DEPTH_LIMIT {
            return Err(domain(
                Code::LedgerLimitExceeded,
                dir.display().to_string(),
                format!("ledger tree exceeds depth limit {RUN_DEPTH_LIMIT}"),
            ));
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            if depth == 0 && (name == ".lock" || name == "transactions") {
                continue;
            }
            let relative = path.strip_prefix(run_dir).unwrap_or(&path);
            if relative == Path::new("artifacts/gate-runtime") {
                // The CLI validates this separately authenticated executor
                // manifest immediately before every verification transaction.
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(domain(
                    Code::LedgerEntryInvalid,
                    path.display().to_string(),
                    "symlinked ledger evidence is not allowed",
                ));
            }
            if metadata.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                return Err(domain(
                    Code::LedgerEntryInvalid,
                    path.display().to_string(),
                    "ledger evidence must be a regular file",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if metadata.nlink() != 1 {
                    return Err(domain(
                        Code::LedgerEntryInvalid,
                        path.display().to_string(),
                        "ledger evidence must not be multiply linked",
                    ));
                }
            }
            files += 1;
            bytes = bytes.saturating_add(metadata.len());
            if files > RUN_FILE_LIMIT || bytes > RUN_BYTE_LIMIT {
                return Err(domain(
                    Code::LedgerLimitExceeded,
                    run_dir.display().to_string(),
                    "ledger projection tree exceeds its file or byte limit",
                ));
            }
        }
    }
    Ok(())
}

fn value_u64(value: &Value) -> Option<u64> {
    value.as_i64().and_then(|value| u64::try_from(value).ok())
}

fn apply_effect(state: &mut JournalState, effect: &Value, source: &Path) -> Result<(), StoreError> {
    let path = effect.get("path").and_then(Value::as_str).unwrap_or("");
    if !safe_projection_path(path) {
        return Err(domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            format!("unsafe journal projection path: {path:?}"),
        ));
    }
    let operation = effect
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let content = effect
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                source.display().to_string(),
                "journal effect content must be text",
            )
        })?;
    let before = state.projections.get(path).cloned().unwrap_or_default();
    let before_digest = effect
        .get("before_digest")
        .and_then(Value::as_str)
        .unwrap_or("");
    if before_digest != sha256(&before) {
        return Err(domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            format!("journal effect changed-prefix conflict for {path}"),
        ));
    }
    let next = match operation {
        "replace" => content.as_bytes().to_vec(),
        "append" => {
            let offset = effect.get("offset").and_then(value_u64).unwrap_or(u64::MAX);
            if offset != before.len() as u64 {
                return Err(domain(
                    Code::LedgerEntryInvalid,
                    source.display().to_string(),
                    format!("journal append offset conflict for {path}"),
                ));
            }
            let mut next = before.clone();
            next.extend_from_slice(content.as_bytes());
            next
        }
        "remove" => Vec::new(),
        _ => {
            return Err(domain(
                Code::LedgerEntryInvalid,
                source.display().to_string(),
                format!("unknown journal effect operation {operation:?}"),
            ));
        }
    };
    let history = state.history.entry(path.to_owned()).or_default();
    history.insert(sha256(&before));
    history.insert(sha256(&next));
    if operation == "remove" {
        state.projections.remove(path);
        state.tombstones.insert(path.to_owned());
    } else {
        state.projections.insert(path.to_owned(), next);
        state.tombstones.remove(path);
    }
    Ok(())
}

fn projected_status(state: &JournalState, source: &Path) -> Result<Option<Status>, StoreError> {
    let Some(bytes) = state.projections.get("run.json") else {
        return Ok(None);
    };
    let text = std::str::from_utf8(bytes).map_err(|error| {
        domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            format!("projected run.json is not UTF-8: {error}"),
        )
    })?;
    let run: Value = ledger_json::from_str(text).map_err(|error| {
        domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            format!("projected run.json is malformed: {error}"),
        )
    })?;
    let status = run
        .get("status")
        .and_then(Value::as_str)
        .and_then(Status::parse)
        .ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                source.display().to_string(),
                "projected run.json has an invalid status",
            )
        })?;
    Ok(Some(status))
}

fn validate_replayed_command(
    command: &Value,
    revision: u64,
    before: Option<Status>,
    after: Option<Status>,
    source: &Path,
) -> Result<(), StoreError> {
    let kind = command.get("kind").and_then(Value::as_str).ok_or_else(|| {
        domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            "transaction command kind is missing",
        )
    })?;
    if kind == "legacy_anchor" {
        if revision != 0 || before.is_some() || after.is_none() {
            return Err(domain(
                Code::LedgerEntryInvalid,
                source.display().to_string(),
                "legacy_anchor is valid only as revision zero with one run projection",
            ));
        }
        return Ok(());
    }
    if revision == 0 {
        return Err(domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            "revision zero must be a legacy_anchor",
        ));
    }
    let structural = match kind {
        "initialize" => core::LedgerCommand::Initialize,
        "event" => core::LedgerCommand::Event,
        "checkpoint" => core::LedgerCommand::Checkpoint,
        "gate" => core::LedgerCommand::Gate,
        "evidence" => core::LedgerCommand::Evidence,
        "interrupt" => core::LedgerCommand::Interrupt,
        "resume" => core::LedgerCommand::Resume,
        "finalize" => {
            let target = command
                .get("target_status")
                .and_then(Value::as_str)
                .and_then(Status::parse)
                .ok_or_else(|| {
                    domain(
                        Code::LedgerEntryInvalid,
                        source.display().to_string(),
                        "finalize transaction is missing a valid target_status",
                    )
                })?;
            core::LedgerCommand::Finalize(target)
        }
        _ => {
            return Err(domain(
                Code::LedgerEntryInvalid,
                source.display().to_string(),
                format!("unknown transaction command kind {kind:?}"),
            ));
        }
    };
    let expected = core::transition(before, structural).map_err(|message| {
        domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            format!("transaction command violates lifecycle: {message}"),
        )
    })?;
    if after != Some(expected) {
        return Err(domain(
            Code::LedgerEntryInvalid,
            source.display().to_string(),
            format!(
                "transaction command projects status {:?}, expected {:?}",
                after.map(Status::as_str),
                expected.as_str()
            ),
        ));
    }
    Ok(())
}

fn load_journal(run_dir: &Path) -> Result<JournalState, StoreError> {
    validate_projection_tree(run_dir)?;
    let journal_dir = run_dir.join("transactions");
    let mut files = match fs::symlink_metadata(&journal_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            let mut files = Vec::new();
            for entry in fs::read_dir(&journal_dir)?.take(JOURNAL_FILE_LIMIT + 1) {
                let path = entry?.path();
                let name = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                let name_bytes = name.as_bytes();
                let valid_name = name_bytes.len() == 25
                    && &name_bytes[20..] == b".json"
                    && name_bytes[..20].iter().all(u8::is_ascii_digit);
                if !valid_name {
                    return Err(domain(
                        Code::LedgerEntryInvalid,
                        path.display().to_string(),
                        "transaction journal contains an unexpected entry",
                    ));
                }
                let metadata = projection_metadata(&path)?.ok_or_else(|| {
                    domain(
                        Code::LedgerEntryInvalid,
                        path.display().to_string(),
                        "transaction journal entry disappeared",
                    )
                })?;
                if metadata.len() > TRANSACTION_BYTE_LIMIT {
                    return Err(domain(
                        Code::LedgerLimitExceeded,
                        path.display().to_string(),
                        "transaction exceeds its byte limit",
                    ));
                }
                files.push(path);
            }
            files.sort();
            files
        }
        Ok(_) => {
            return Err(domain(
                Code::LedgerEntryInvalid,
                journal_dir.display().to_string(),
                "transaction journal must be a real directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if files.len() > JOURNAL_FILE_LIMIT {
        return Err(domain(
            Code::LedgerLimitExceeded,
            journal_dir.display().to_string(),
            "transaction journal exceeds its revision limit",
        ));
    }
    if files.is_empty() && run_dir.join("run.json").is_file() {
        anchor_legacy_run(run_dir)?;
        files.push(journal_dir.join("00000000000000000000.json"));
    }

    let mut state = JournalState::empty();
    let first_revision: u64 = if files
        .first()
        .and_then(|path| path.file_stem())
        .and_then(|value| value.to_str())
        == Some("00000000000000000000")
    {
        0
    } else {
        1
    };
    for (offset, path) in files.into_iter().enumerate() {
        let expected_revision = first_revision.saturating_add(offset as u64);
        projection_metadata(&path)?;
        let record = read_json(&path)?;
        if record.get("kind").and_then(Value::as_str) != Some(core::TRANSACTION_KIND)
            || record.get("revision").and_then(value_u64) != Some(expected_revision)
        {
            return Err(domain(
                Code::LedgerEntryInvalid,
                path.display().to_string(),
                "transaction revision or kind is invalid",
            ));
        }
        let previous = record.get("previous_digest");
        let expected_previous = state
            .digest
            .as_ref()
            .map(|digest| Value::String(digest.clone()))
            .unwrap_or(Value::Null);
        if previous != Some(&expected_previous) {
            return Err(domain(
                Code::LedgerEntryInvalid,
                path.display().to_string(),
                "transaction digest chain is broken",
            ));
        }
        let mut body = record.clone();
        let digest = body
            .as_object_mut()
            .and_then(|map| map.remove("digest"))
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        if digest != transaction_digest(&body) {
            return Err(domain(
                Code::LedgerEntryInvalid,
                path.display().to_string(),
                "transaction digest is invalid",
            ));
        }
        let command = record.get("command").ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                path.display().to_string(),
                "transaction command is missing",
            )
        })?;
        let before_status = projected_status(&state, &path)?;
        let effects = record
            .get("effects")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                domain(
                    Code::LedgerEntryInvalid,
                    path.display().to_string(),
                    "transaction effects must be an array",
                )
            })?;
        for effect in effects {
            apply_effect(&mut state, effect, &path)?;
        }
        let after_status = projected_status(&state, &path)?;
        validate_replayed_command(
            command,
            expected_revision,
            before_status,
            after_status,
            &path,
        )?;
        let result = record.get("result").cloned().ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                path.display().to_string(),
                "transaction result is missing",
            )
        })?;
        ensure_value_limit("transaction result", &result, core::DOCUMENT_LIMIT)?;
        let key = command
            .get("idempotency_key")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !key.is_empty() {
            let kind = command
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let request = command
                .get("request_digest")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if state
                .identities
                .insert(
                    key.to_owned(),
                    (kind, request, expected_revision, digest.clone(), result),
                )
                .is_some()
            {
                return Err(domain(
                    Code::LedgerEntryInvalid,
                    path.display().to_string(),
                    "duplicate idempotency key in transaction journal",
                ));
            }
        }
        state.revision = expected_revision;
        state.digest = Some(digest);
    }
    repair_projections(run_dir, &state)?;
    Ok(state)
}

fn repair_projections(run_dir: &Path, state: &JournalState) -> Result<(), StoreError> {
    for relative in &state.tombstones {
        let path = run_dir.join(relative);
        if let Some(metadata) = projection_metadata(&path)? {
            if metadata.len() > RUN_BYTE_LIMIT {
                return Err(domain(
                    Code::LedgerLimitExceeded,
                    path.display().to_string(),
                    "ledger projection exceeds the run byte limit",
                ));
            }
            let actual = fs::read(&path)?;
            let actual_digest = sha256(&actual);
            if !state
                .history
                .get(relative)
                .is_some_and(|history| history.contains(&actual_digest))
            {
                return Err(domain(
                    Code::LedgerEntryInvalid,
                    path.display().to_string(),
                    "removed projection differs from every committed journal boundary",
                ));
            }
            fs::remove_file(&path)?;
            if let Some(parent) = path.parent()
                && let Ok(directory) = File::open(parent)
            {
                let _ = directory.sync_all();
            }
        }
    }
    for (relative, expected) in &state.projections {
        let path = run_dir.join(relative);
        let actual = match projection_metadata(&path)? {
            Some(metadata) => {
                if metadata.len() > RUN_BYTE_LIMIT {
                    return Err(domain(
                        Code::LedgerLimitExceeded,
                        path.display().to_string(),
                        "ledger projection exceeds the run byte limit",
                    ));
                }
                fs::read(&path)?
            }
            None => Vec::new(),
        };
        if actual == *expected {
            continue;
        }
        let actual_digest = sha256(&actual);
        if !state
            .history
            .get(relative)
            .is_some_and(|history| history.contains(&actual_digest))
        {
            return Err(domain(
                Code::LedgerEntryInvalid,
                path.display().to_string(),
                "projection differs from every committed journal boundary",
            ));
        }
        write_bytes_durable(&path, expected)?;
    }
    Ok(())
}

fn write_bytes_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ledger text is not UTF-8"))?;
    write_text_durable(path, text)
}

fn collect_legacy_files(run_dir: &Path) -> Result<Vec<(String, String)>, StoreError> {
    let mut found = Vec::new();
    let mut stack = vec![run_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(run_dir).unwrap_or(&path);
            if relative == Path::new(".lock")
                || relative
                    .components()
                    .next()
                    .is_some_and(|component| component.as_os_str() == "transactions")
                || relative.starts_with("artifacts/gate-runtime")
            {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let text = fs::read_to_string(&path).map_err(|err| {
                    domain(
                        Code::LedgerEntryInvalid,
                        path.display().to_string(),
                        format!("legacy ledger evidence is not bounded UTF-8 text: {err}"),
                    )
                })?;
                found.push((relative.to_string_lossy().into_owned(), text));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

fn anchor_legacy_run(run_dir: &Path) -> Result<(), StoreError> {
    let files = collect_legacy_files(run_dir)?;
    for (path, _) in &files {
        if !safe_projection_path(path) {
            return Err(domain(
                Code::LedgerEntryInvalid,
                run_dir.join(path).display().to_string(),
                format!("unsafe legacy projection path: {path:?}"),
            ));
        }
    }
    let effects: Vec<Value> = files
        .iter()
        .map(|(path, content)| effect_value(&ProjectionEffect::replace(path, content), &[]))
        .collect();
    let mut record = ledger_json::json!({
        "kind": core::TRANSACTION_KIND,
        "revision": 0,
        "previous_digest": null,
        "command": {
            "kind": "legacy_anchor",
            "idempotency_key": "",
            "request_digest": sha256(b"run-ledger-v1"),
        },
        "effects": effects,
        "result": null,
    });
    let digest = transaction_digest(&record);
    if let Some(map) = record.as_object_mut() {
        map.insert("digest".to_owned(), Value::String(digest));
    }
    let transaction_size = core::json_pretty(&record).len().saturating_add(1);
    if transaction_size > TRANSACTION_BYTE_LIMIT as usize {
        return Err(domain(
            Code::LedgerLimitExceeded,
            run_dir.display().to_string(),
            format!("legacy anchor is {transaction_size} bytes; limit is {TRANSACTION_BYTE_LIMIT}"),
        ));
    }
    publish_transaction(run_dir, 0, &record)?;
    Ok(())
}

fn auto_idempotency_key(kind: core::CommandKind, revision: u64) -> String {
    format!(
        "{}-{}-{}-{revision}",
        kind.as_str(),
        now_stamp(),
        token_hex(8)
    )
}

fn commit_command(
    run_dir: &Path,
    command: core::LedgerCommand,
    request: &Value,
    expected_revision: Option<u64>,
    idempotency_key: Option<&str>,
    build: impl FnOnce(&JournalState) -> Result<(Vec<ProjectionEffect>, Value), StoreError>,
) -> Result<CommitOutcome, StoreError> {
    let _lock = RunLock::acquire(run_dir)?;
    let mut state = load_journal(run_dir)?;
    if state.revision >= JOURNAL_FILE_LIMIT as u64 {
        return Err(domain(
            Code::LedgerLimitExceeded,
            run_dir.display().to_string(),
            "transaction journal is full",
        ));
    }
    if state.revision >= JOURNAL_ORDINARY_LIMIT
        && !matches!(
            command,
            core::LedgerCommand::Interrupt | core::LedgerCommand::Finalize(_)
        )
    {
        return Err(domain(
            Code::LedgerLimitExceeded,
            run_dir.display().to_string(),
            "ordinary transaction capacity is exhausted; interruption and finalization remain reserved",
        ));
    }
    let safe_request = core::redact_json(request);
    let request_limit = if matches!(
        command,
        core::LedgerCommand::Checkpoint | core::LedgerCommand::Finalize(_)
    ) {
        core::DOCUMENT_LIMIT
    } else {
        core::EVENT_LIMIT
    };
    ensure_value_limit(command.kind().as_str(), &safe_request, request_limit)?;
    let request_digest = sha256(core::json_line(&safe_request).as_bytes());
    let key = idempotency_key
        .map(str::to_owned)
        .unwrap_or_else(|| auto_idempotency_key(command.kind(), state.revision + 1));
    if key.is_empty() || key.len() > 200 || key.chars().any(|character| character.is_control()) {
        return Err(domain(
            Code::LedgerEntryInvalid,
            run_dir.display().to_string(),
            "idempotency operation identity must be 1-200 non-control bytes",
        ));
    }
    if let Some((kind, prior_request, revision, digest, result)) = state.identities.get(&key) {
        if kind == command.kind().as_str() && prior_request == &request_digest {
            return Ok(CommitOutcome {
                revision: *revision,
                digest: digest.clone(),
                replayed: true,
                result: result.clone(),
            });
        }
        return Err(domain(
            Code::LedgerEntryInvalid,
            run_dir.display().to_string(),
            format!("idempotency key {key:?} was reused with different command bytes"),
        ));
    }
    if expected_revision.is_some_and(|expected| expected != state.revision) {
        return Err(domain(
            Code::LedgerTransitionInvalid,
            run_dir.display().to_string(),
            format!(
                "expected revision {}, found {}",
                expected_revision.unwrap_or_default(),
                state.revision
            ),
        ));
    }
    let (effects, result) = build(&state)?;
    ensure_value_limit("transaction result", &result, core::DOCUMENT_LIMIT)?;
    let mut effect_values = Vec::with_capacity(effects.len());
    let mut changed = BTreeSet::new();
    for effect in &effects {
        if !safe_projection_path(&effect.path) {
            return Err(domain(
                Code::LedgerEntryInvalid,
                run_dir.display().to_string(),
                format!("unsafe projection path: {:?}", effect.path),
            ));
        }
        let before = state
            .projections
            .get(&effect.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let value = effect_value(effect, before);
        apply_effect(&mut state, &value, run_dir)?;
        effect_values.push(value);
        changed.insert(effect.path.clone());
    }
    let revision = state.revision + 1;
    let target_status = match command {
        core::LedgerCommand::Finalize(status) => Value::String(status.as_str().to_owned()),
        _ => Value::Null,
    };
    let mut record = ledger_json::json!({
        "kind": core::TRANSACTION_KIND,
        "revision": revision,
        "previous_digest": state.digest.clone().map(Value::String).unwrap_or(Value::Null),
        "command": {
            "kind": command.kind().as_str(),
            "idempotency_key": key,
            "request_digest": request_digest,
            "target_status": target_status,
        },
        "effects": effect_values,
        "result": result,
    });
    let digest = transaction_digest(&record);
    if let Some(map) = record.as_object_mut() {
        map.insert("digest".to_owned(), Value::String(digest.clone()));
    }
    if std::env::var_os("NOPAL_LEDGER_TEST_FAILPOINT").as_deref()
        == Some(std::ffi::OsStr::new("before_commit"))
    {
        return Err(io::Error::other("ledger failpoint before commit").into());
    }
    publish_transaction(run_dir, revision, &record)?;
    if std::env::var_os("NOPAL_LEDGER_TEST_FAILPOINT").as_deref()
        == Some(std::ffi::OsStr::new("after_commit"))
    {
        return Err(io::Error::other("ledger failpoint after commit").into());
    }
    for (index, relative) in changed.iter().enumerate() {
        let path = run_dir.join(relative);
        if let Some(content) = state.projections.get(relative) {
            write_bytes_durable(&path, content)?;
        } else if state.tombstones.contains(relative) {
            match fs::remove_file(&path) {
                Ok(()) => {
                    if let Some(parent) = path.parent()
                        && let Ok(directory) = File::open(parent)
                    {
                        let _ = directory.sync_all();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            return Err(domain(
                Code::LedgerEntryInvalid,
                run_dir.display().to_string(),
                "committed projection disappeared",
            ));
        }
        if index == 0
            && std::env::var_os("NOPAL_LEDGER_TEST_FAILPOINT").as_deref()
                == Some(std::ffi::OsStr::new("after_one_projection"))
        {
            return Err(io::Error::other("ledger failpoint after one projection").into());
        }
    }
    Ok(CommitOutcome {
        revision,
        digest,
        replayed: false,
        result,
    })
}

fn run_status(run: &Value, run_dir: &Path) -> Result<Status, StoreError> {
    run.get("status")
        .and_then(Value::as_str)
        .and_then(Status::parse)
        .ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                run_dir.join("run.json").display().to_string(),
                "run status is missing or invalid",
            )
        })
}

fn validate_transition(
    run: Option<&Value>,
    run_dir: &Path,
    command: core::LedgerCommand,
) -> Result<Status, StoreError> {
    let current = run.map(|value| run_status(value, run_dir)).transpose()?;
    core::transition(current, command).map_err(|message| {
        domain(
            Code::LedgerTransitionInvalid,
            run_dir.display().to_string(),
            message,
        )
    })
}

fn append_event_effects(
    state: &JournalState,
    run_dir: &Path,
    run: &mut Value,
    event: &Value,
    transcript_summary: Option<&str>,
) -> Result<Vec<ProjectionEffect>, StoreError> {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("event");
    let safe_payload = event.get("payload").cloned().unwrap_or(Value::Null);
    let summary = transcript_summary
        .map(|text| core::redact_text(text, core::TEXT_LIMIT))
        .unwrap_or_else(|| core::default_transcript_summary(&safe_payload));
    if let Some(map) = run.as_object_mut() {
        let count = map
            .get("events")
            .and_then(|events| events.get("count"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        match map.get_mut("events") {
            Some(Value::Object(events)) => {
                events.insert("count".to_owned(), Value::from(count + 1));
            }
            _ => {
                map.insert(
                    "events".to_owned(),
                    ledger_json::json!({"count": count + 1}),
                );
            }
        }
        map.insert(
            "updated_at".to_owned(),
            event.get("timestamp").cloned().unwrap_or(Value::Null),
        );
    }
    let mut event_line = core::json_line(event);
    event_line.push('\n');
    let transcript = core::transcript_section(event_type, &summary);
    let _ = state;
    let _ = run_dir;
    Ok(vec![
        ProjectionEffect::append("events.jsonl", event_line),
        ProjectionEffect::append("transcript.md", transcript),
        ProjectionEffect::replace(
            "run.json",
            String::from_utf8(json_bytes(run)).unwrap_or_default(),
        ),
    ])
}

fn apply_checkpoint_to_run(
    run: &mut Value,
    name: &str,
    checkpoint_path: &Path,
    resume_hint: Option<&str>,
    now: &str,
) {
    if let Some(map) = run.as_object_mut() {
        let path_text = checkpoint_path.display().to_string();
        let mut entry = ledger_json::json!({
            "name": name,
            "path": path_text,
            "timestamp": now,
        });
        if let Some(hint) = resume_hint.filter(|hint| !hint.is_empty()) {
            let redacted = core::redact_text(hint, core::HINT_LIMIT);
            if let Some(entry_map) = entry.as_object_mut() {
                entry_map.insert("resume_hint".to_owned(), Value::String(redacted.clone()));
            }
            map.insert("resume_hint".to_owned(), Value::String(redacted));
        }
        map.insert("latest_checkpoint".to_owned(), entry);
        map.insert(
            "last_checkpoint".to_owned(),
            Value::String(path_text.clone()),
        );
        map.insert("current_step".to_owned(), Value::String(name.to_owned()));
        match map.get_mut("checkpoints") {
            Some(Value::Array(list)) => list.push(Value::String(path_text)),
            _ => {
                map.insert("checkpoints".to_owned(), ledger_json::json!([path_text]));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Run discovery
// ---------------------------------------------------------------------------

/// Roots that may contain this repo's runs: one per flow, or one when the
/// flow is known. (Legacy flat `runs/<hash>` is deliberately not scanned.)
fn candidate_roots(env: &LedgerEnv, flow: Option<&str>) -> io::Result<Vec<PathBuf>> {
    if let Some(flow) = flow.filter(|f| !f.is_empty()) {
        return Ok(vec![env.run_root(&core::normalize_flow(Some(flow), None))]);
    }
    let runs_root = env.state_dir.join("runs");
    let mut roots = Vec::new();
    if runs_root.is_dir() {
        let mut flows: Vec<PathBuf> = fs::read_dir(&runs_root)?
            .take(FLOW_SCAN_LIMIT + 1)
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        if flows.len() > FLOW_SCAN_LIMIT {
            return Err(io::Error::other(format!(
                "run ledger flow scan exceeds {FLOW_SCAN_LIMIT} entries"
            )));
        }
        flows.sort();
        for flow_dir in flows {
            roots.push(flow_dir.join(&env.repo_hash));
        }
    }
    Ok(roots)
}

pub fn find_run_dir(
    env: &LedgerEnv,
    run_id: &str,
    flow: Option<&str>,
) -> Result<PathBuf, StoreError> {
    // Hardening beyond the Python tool (which validates only on init): a
    // traversal-shaped id would otherwise be joined into the search path and
    // could read or write outside the state dir. Ids that fail this check
    // could never have been created by init, so nothing legitimate is lost.
    if !core::identifier_valid(run_id) {
        return Err(domain(
            Code::RunIdInvalid,
            run_id,
            "invalid run id: use a single path-safe segment [A-Za-z0-9_.-]",
        ));
    }
    let mut matches = Vec::new();
    for root in candidate_roots(env, flow)? {
        let candidate = root.join(run_id);
        if candidate.join("run.json").is_file() {
            matches.push(candidate);
        }
    }
    match matches.len() {
        0 => Err(domain(
            Code::RunNotFound,
            run_id,
            format!("run not found: {run_id}"),
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(domain(
            Code::RunAmbiguous,
            run_id,
            format!("run id is ambiguous; pass --flow to disambiguate: {run_id}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub struct InitArgs<'a> {
    pub skill: &'a str,
    pub flow: Option<&'a str>,
    pub ticket_id: &'a str,
    pub ticket_title: &'a str,
    pub ticket_url: &'a str,
    pub branch: Option<&'a str>,
    pub run_id: Option<&'a str>,
}

pub struct InitOutcome {
    pub run_id: String,
    pub flow: String,
    pub run_dir: PathBuf,
}

pub fn init_run(env: &LedgerEnv, args: &InitArgs) -> Result<InitOutcome, StoreError> {
    let flow = core::normalize_flow(args.flow, Some(args.skill));
    let explicit = match args.run_id {
        Some(rid) => {
            if !core::identifier_valid(rid) {
                return Err(domain(
                    Code::RunIdInvalid,
                    rid,
                    "invalid run id: use a single path-safe segment [A-Za-z0-9_.-]",
                ));
            }
            Some(rid.to_owned())
        }
        None => None,
    };
    let root = env.run_root(&flow);
    ensure_directory_chain(&root)?;

    let mut rid = explicit
        .clone()
        .unwrap_or_else(|| core::new_run_id(&now_stamp(), &token_hex(3)));
    let mut suffix = 1u32;
    let run_dir = loop {
        let candidate = root.join(&rid);
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if explicit.is_some() {
                    return Err(domain(
                        Code::RunIdCollision,
                        candidate.display().to_string(),
                        format!("run id already exists: {}", candidate.display()),
                    ));
                }
                suffix += 1;
                rid = format!("{}-{suffix}", core::new_run_id(&now_stamp(), &token_hex(3)));
            }
            Err(err) => return Err(err.into()),
        }
    };
    for sub in [
        "artifacts",
        "artifacts/gates",
        "artifacts/reviews",
        "logs",
        "checkpoints",
    ] {
        ensure_directory_chain(&run_dir.join(sub))?;
    }

    let started = now_iso();
    let branch = core::redact_text(
        &match args.branch.filter(|b| !b.is_empty()) {
            Some(branch) => branch.to_owned(),
            None => current_branch(&env.repo),
        },
        core::TEXT_LIMIT,
    );
    let run_dir_text = core::redact_text(&run_dir.display().to_string(), core::TEXT_LIMIT);
    let repo_text = core::redact_text(&env.repo.display().to_string(), core::TEXT_LIMIT);
    let skill = core::redact_text(args.skill, core::TEXT_LIMIT);
    let ticket_id = core::redact_text(
        if args.ticket_id.is_empty() {
            "none"
        } else {
            args.ticket_id
        },
        core::TEXT_LIMIT,
    );
    let ticket_title = core::redact_text(
        if args.ticket_title.is_empty() {
            "none"
        } else {
            args.ticket_title
        },
        core::TEXT_LIMIT,
    );
    let ticket_url = core::redact_text(args.ticket_url, core::TEXT_LIMIT);
    let ctx = core::InitContext {
        run_id: &rid,
        flow: &flow,
        repo: &repo_text,
        repo_hash: &env.repo_hash,
        branch: &branch,
        skill: &skill,
        ticket_id: &ticket_id,
        ticket_title: &ticket_title,
        ticket_url: &ticket_url,
        started_at: &started,
        run_dir: &run_dir_text,
    };
    let initialized_payload = ledger_json::json!({
        "skill": skill,
        "flow": flow,
        "ticket": {"id": ticket_id, "title": ticket_title, "url": ticket_url},
        "branch": branch,
    });
    let initialized_event = core::event_value(
        "run_initialized",
        core::redact_json(&initialized_payload),
        &started,
    );
    let initial_run = core::new_run_entry(&ctx);
    let header = core::transcript_header(&ctx);
    commit_command(
        &run_dir,
        core::LedgerCommand::Initialize,
        &initialized_payload,
        Some(0),
        Some(&format!("initialize-{rid}")),
        |state| {
            validate_transition(None, &run_dir, core::LedgerCommand::Initialize)?;
            let mut run = initial_run.clone();
            let mut effects = vec![ProjectionEffect::replace("transcript.md", header.clone())];
            effects.extend(append_event_effects(
                state,
                &run_dir,
                &mut run,
                &initialized_event,
                None,
            )?);
            Ok((effects, ledger_json::json!({"run_id": rid})))
        },
    )?;
    Ok(InitOutcome {
        run_id: rid,
        flow,
        run_dir,
    })
}

/// Append one event: jsonl line, transcript section, and the run.json count
/// bump, all under a single lock hold.
pub fn append_event(
    run_dir: &Path,
    event_type: &str,
    payload: &Value,
    transcript_summary: Option<&str>,
) -> Result<Value, StoreError> {
    append_event_with_operation_id(run_dir, event_type, payload, transcript_summary, None)
}

pub fn append_event_with_operation_id(
    run_dir: &Path,
    event_type: &str,
    payload: &Value,
    transcript_summary: Option<&str>,
    operation_id: Option<&str>,
) -> Result<Value, StoreError> {
    ensure_value_limit(event_type, payload, core::EVENT_LIMIT)?;
    let safe_payload = core::redact_json(payload);
    let event = core::event_value(event_type, safe_payload, &now_iso());
    let request = ledger_json::json!({
        "type": event_type,
        "payload": payload,
        "summary": transcript_summary,
    });
    let outcome = commit_command(
        run_dir,
        core::LedgerCommand::Event,
        &request,
        None,
        operation_id,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Event)?;
            Ok((
                append_event_effects(state, run_dir, &mut run, &event, transcript_summary)?,
                event.clone(),
            ))
        },
    )?;
    Ok(outcome.result)
}

/// Write a checkpoint document and fold it into run.json under one lock.
pub fn record_checkpoint(
    run_dir: &Path,
    name: &str,
    payload: &Value,
    resume_hint: Option<&str>,
) -> Result<PathBuf, StoreError> {
    record_checkpoint_with_operation_id(run_dir, name, payload, resume_hint, None)
}

pub fn record_checkpoint_with_operation_id(
    run_dir: &Path,
    name: &str,
    payload: &Value,
    resume_hint: Option<&str>,
    operation_id: Option<&str>,
) -> Result<PathBuf, StoreError> {
    ensure_value_limit(name, payload, core::DOCUMENT_LIMIT)?;
    let now = now_iso();
    let checkpoint_relative = Path::new("checkpoints").join(core::checkpoint_file_name(name));
    let checkpoint_path = run_dir.join(&checkpoint_relative);
    let checkpoint_body = core::checkpoint_value(name, payload, resume_hint, &now);
    let mut event_payload = ledger_json::json!({
        "name": name,
        "path": checkpoint_path.display().to_string(),
        "resume_hint": resume_hint,
    });
    if let Some(map) = event_payload.as_object_mut() {
        map.insert("payload".to_owned(), payload.clone());
    }
    let event = core::event_value("checkpoint", core::redact_json(&event_payload), &now);
    let mut request = ledger_json::json!({
        "name": name,
        "resume_hint": resume_hint,
    });
    if let Some(map) = request.as_object_mut() {
        map.insert("payload".to_owned(), payload.clone());
    }
    commit_command(
        run_dir,
        core::LedgerCommand::Checkpoint,
        &request,
        None,
        operation_id,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Checkpoint)?;
            apply_checkpoint_to_run(&mut run, name, &checkpoint_path, resume_hint, &now);
            let mut effects = vec![ProjectionEffect::replace(
                checkpoint_relative.to_string_lossy(),
                String::from_utf8(json_bytes(&checkpoint_body)).unwrap_or_default(),
            )];
            effects.extend(append_event_effects(
                state, run_dir, &mut run, &event, None,
            )?);
            Ok((
                effects,
                ledger_json::json!({"checkpoint_path": checkpoint_path.display().to_string()}),
            ))
        },
    )?;
    Ok(checkpoint_path)
}

/// Allocate the next numeric attempt dir under `gate_root` (own lock hold).
pub fn next_attempt_dir(run_dir: &Path, gate_root: &Path) -> Result<PathBuf, StoreError> {
    let _lock = RunLock::acquire(run_dir)?;
    ensure_directory_chain(gate_root)?;
    let mut attempt = 1u32;
    loop {
        let candidate = gate_root.join(attempt.to_string());
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(err) => return Err(err.into()),
        }
    }
}

pub struct GateOutcome {
    pub envelope_path: PathBuf,
    pub checkpoint_path: PathBuf,
}

/// Record one gate attempt: envelope artifact, run.json artifact/log entries,
/// a gate checkpoint, and a `gate_result` event - four sequenced lock holds,
/// exactly like the Python tool.
pub fn record_gate(
    run_dir: &Path,
    name: &str,
    scope: Option<&str>,
    envelope: &Value,
    resume_hint: Option<&str>,
) -> Result<GateOutcome, StoreError> {
    record_gate_with_operation_id(run_dir, name, scope, envelope, resume_hint, None)
}

pub fn record_gate_with_operation_id(
    run_dir: &Path,
    name: &str,
    scope: Option<&str>,
    envelope: &Value,
    resume_hint: Option<&str>,
    operation_id: Option<&str>,
) -> Result<GateOutcome, StoreError> {
    ensure_value_limit(name, envelope, core::EVENT_LIMIT)?;
    let scope_source = scope
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            envelope
                .get("gate")
                .and_then(|gate| gate.get("scope"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "repo".to_owned());
    let scope = core::slug(&scope_source, "repo");
    let safe_name = core::slug(name, "gate");
    let hint = resume_hint
        .filter(|value| !value.is_empty())
        .unwrap_or("continue after reviewing gate result");
    let now = now_iso();
    let request = ledger_json::json!({
        "name": name,
        "scope": scope,
        "envelope": envelope,
        "resume_hint": hint,
    });
    let mut selected_paths: Option<(PathBuf, PathBuf)> = None;
    let outcome = commit_command(
        run_dir,
        core::LedgerCommand::Gate,
        &request,
        None,
        operation_id,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Gate)?;
            let mut attempt = 1u32;
            let (envelope_relative, envelope_path) = loop {
                let relative = Path::new("artifacts")
                    .join("gates")
                    .join(&scope)
                    .join(&safe_name)
                    .join(attempt.to_string())
                    .join("envelope.json");
                let key = relative.to_string_lossy().into_owned();
                if !state.projections.contains_key(&key) {
                    break (relative, run_dir.join(&key));
                }
                attempt += 1;
                if attempt > 4096 {
                    return Err(domain(
                        Code::LedgerLimitExceeded,
                        run_dir.display().to_string(),
                        "gate attempt limit exceeded",
                    ));
                }
            };
            let checkpoint_name = format!("gate-{scope}-{safe_name}");
            let checkpoint_relative =
                Path::new("checkpoints").join(core::checkpoint_file_name(&checkpoint_name));
            let checkpoint_path = run_dir.join(&checkpoint_relative);
            let checkpoint_payload = ledger_json::json!({
                "name": name,
                "scope": scope,
                "path": envelope_path.display().to_string(),
                "status": envelope.get("status").cloned().unwrap_or(Value::Null),
                "envelope": envelope,
            });
            let checkpoint_body =
                core::checkpoint_value(&checkpoint_name, &checkpoint_payload, Some(hint), &now);
            if let Some(map) = run.as_object_mut() {
                let artifact = ledger_json::json!({
                    "name": name,
                    "path": envelope_path.display().to_string(),
                    "kind": "gate",
                    "scope": scope,
                });
                for key in ["artifacts", "logs"] {
                    match map.get_mut(key) {
                        Some(Value::Array(list)) => list.push(artifact.clone()),
                        _ => {
                            map.insert(key.to_owned(), ledger_json::json!([artifact.clone()]));
                        }
                    }
                }
            }
            apply_checkpoint_to_run(
                &mut run,
                &checkpoint_name,
                &checkpoint_path,
                Some(hint),
                &now,
            );
            let event_payload = ledger_json::json!({
                "name": name,
                "scope": scope,
                "path": envelope_path.display().to_string(),
                "checkpoint": checkpoint_path.display().to_string(),
                "envelope": envelope,
            });
            let event = core::event_value("gate_result", core::redact_json(&event_payload), &now);
            let mut effects = vec![
                ProjectionEffect::replace(
                    envelope_relative.to_string_lossy(),
                    String::from_utf8(json_bytes(&core::redact_json(envelope))).unwrap_or_default(),
                ),
                ProjectionEffect::replace(
                    checkpoint_relative.to_string_lossy(),
                    String::from_utf8(json_bytes(&checkpoint_body)).unwrap_or_default(),
                ),
            ];
            effects.extend(append_event_effects(
                state, run_dir, &mut run, &event, None,
            )?);
            selected_paths = Some((envelope_path.clone(), checkpoint_path.clone()));
            Ok((
                effects,
                ledger_json::json!({
                    "envelope_path": envelope_path.display().to_string(),
                    "checkpoint_path": checkpoint_path.display().to_string(),
                }),
            ))
        },
    )?;
    let (envelope_path, checkpoint_path) = match selected_paths {
        Some(paths) => paths,
        None if outcome.replayed => {
            let envelope = outcome
                .result
                .get("envelope_path")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let checkpoint = outcome
                .result
                .get("checkpoint_path")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            match (envelope, checkpoint) {
                (Some(envelope), Some(checkpoint)) => (envelope, checkpoint),
                _ => {
                    return Err(domain(
                        Code::LedgerEntryInvalid,
                        run_dir.display().to_string(),
                        "replayed gate result is missing projection paths",
                    ));
                }
            }
        }
        None => {
            return Err(domain(
                Code::LedgerEntryInvalid,
                run_dir.display().to_string(),
                "gate commit did not allocate projection paths",
            ));
        }
    };
    Ok(GateOutcome {
        envelope_path,
        checkpoint_path,
    })
}

pub fn record_interrupt(
    run_dir: &Path,
    reason: &str,
    resume_hint: Option<&str>,
) -> Result<PathBuf, StoreError> {
    record_interrupt_with_operation_id(run_dir, reason, resume_hint, None)
}

pub fn record_interrupt_with_operation_id(
    run_dir: &Path,
    reason: &str,
    resume_hint: Option<&str>,
    operation_id: Option<&str>,
) -> Result<PathBuf, StoreError> {
    let now = now_iso();
    let checkpoint_relative = Path::new("checkpoints").join("interrupted.json");
    let checkpoint_path = run_dir.join(&checkpoint_relative);
    let payload = ledger_json::json!({"reason": reason});
    let checkpoint_body = core::checkpoint_value("interrupted", &payload, resume_hint, &now);
    let event_payload = ledger_json::json!({
        "reason": reason,
        "resume_hint": resume_hint,
        "checkpoint": checkpoint_path.display().to_string(),
    });
    let event = core::event_value("interrupted", core::redact_json(&event_payload), &now);
    let request = ledger_json::json!({
        "reason": reason,
        "resume_hint": resume_hint,
    });
    commit_command(
        run_dir,
        core::LedgerCommand::Interrupt,
        &request,
        None,
        operation_id,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Interrupt)?;
            apply_checkpoint_to_run(&mut run, "interrupted", &checkpoint_path, resume_hint, &now);
            if let Some(map) = run.as_object_mut() {
                map.insert(
                    "status".to_owned(),
                    Value::String(Status::Interrupted.as_str().to_owned()),
                );
                map.insert(
                    "interruption".to_owned(),
                    ledger_json::json!({
                        "timestamp": now,
                        "reason": core::redact_text(reason, core::TEXT_LIMIT),
                        "checkpoint": checkpoint_path.display().to_string(),
                    }),
                );
            }
            let mut effects = vec![ProjectionEffect::replace(
                checkpoint_relative.to_string_lossy(),
                String::from_utf8(json_bytes(&checkpoint_body)).unwrap_or_default(),
            )];
            effects.extend(append_event_effects(
                state, run_dir, &mut run, &event, None,
            )?);
            Ok((
                effects,
                ledger_json::json!({"checkpoint_path": checkpoint_path.display().to_string()}),
            ))
        },
    )?;
    Ok(checkpoint_path)
}

pub fn record_resume(run_dir: &Path) -> Result<u64, StoreError> {
    record_resume_with_operation_id(run_dir, None)
}

pub fn record_resume_with_operation_id(
    run_dir: &Path,
    operation_id: Option<&str>,
) -> Result<u64, StoreError> {
    let now = now_iso();
    let request = ledger_json::json!({"resume": true});
    let mut resumed_epoch = None;
    let outcome = commit_command(
        run_dir,
        core::LedgerCommand::Resume,
        &request,
        None,
        operation_id,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Resume)?;
            let epoch = run
                .get("resume_epoch")
                .and_then(Value::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(0)
                .saturating_add(1);
            if let Some(map) = run.as_object_mut() {
                map.insert(
                    "status".to_owned(),
                    Value::String(Status::Running.as_str().to_owned()),
                );
                map.insert("resume_epoch".to_owned(), Value::from(epoch));
                map.insert("resumed_at".to_owned(), Value::String(now.clone()));
            }
            let event = core::event_value(
                "resumed",
                ledger_json::json!({"resume_epoch": epoch, "must_reverify": true}),
                &now,
            );
            resumed_epoch = Some(epoch);
            Ok((
                append_event_effects(state, run_dir, &mut run, &event, None)?,
                ledger_json::json!({"resume_epoch": epoch}),
            ))
        },
    )?;
    resumed_epoch
        .or_else(|| outcome.result.get("resume_epoch").and_then(value_u64))
        .ok_or_else(|| {
            domain(
                Code::LedgerEntryInvalid,
                run_dir.display().to_string(),
                "resume commit did not produce an epoch",
            )
        })
}

pub struct FinalizeOutcome {
    pub final_report: Option<PathBuf>,
}

pub fn record_finalize(
    run_dir: &Path,
    status: &str,
    report_file: Option<&Path>,
) -> Result<FinalizeOutcome, StoreError> {
    record_finalize_with_operation_id(run_dir, status, report_file, None)
}

pub fn record_finalize_with_operation_id(
    run_dir: &Path,
    status: &str,
    report_file: Option<&Path>,
    operation_id: Option<&str>,
) -> Result<FinalizeOutcome, StoreError> {
    let status = match Status::parse(status) {
        Some(status) if Status::FINAL.contains(&status) => status,
        _ => {
            return Err(domain(
                Code::LedgerStatusInvalid,
                run_dir.display().to_string(),
                format!(
                    "invalid final status: {status}; expected one of {}",
                    Status::FINAL
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    };
    let report = match report_file {
        Some(source) => {
            let bytes = read_bounded_regular_file(source, core::DOCUMENT_LIMIT, "final report")
                .map_err(|error| {
                    let code = if error.kind() == io::ErrorKind::InvalidData {
                        Code::LedgerLimitExceeded
                    } else {
                        Code::LedgerEntryInvalid
                    };
                    domain(code, source.display().to_string(), error.to_string())
                })?;
            let text = String::from_utf8(bytes).map_err(|error| {
                domain(
                    Code::LedgerEntryInvalid,
                    source.display().to_string(),
                    format!("final report is not UTF-8: {error}"),
                )
            })?;
            Some(truncate_utf8_bytes(
                core::redact_text(&text, core::DOCUMENT_LIMIT),
                core::DOCUMENT_LIMIT,
            ))
        }
        None => None,
    };
    let now = now_iso();
    let report_path = report.as_ref().map(|_| run_dir.join("final-report.md"));
    let checkpoint_relative = Path::new("checkpoints").join("final-report.json");
    let checkpoint_path = run_dir.join(&checkpoint_relative);
    let report_value = report_path
        .as_ref()
        .map(|path| Value::String(path.display().to_string()))
        .unwrap_or(Value::Null);
    let checkpoint_payload =
        ledger_json::json!({"status": status.as_str(), "final_report": report_value});
    let hint = if status == Status::Completed {
        "run complete"
    } else {
        "inspect final status before resuming"
    };
    let checkpoint_body =
        core::checkpoint_value("final-report", &checkpoint_payload, Some(hint), &now);
    let event_payload = ledger_json::json!({
        "status": status.as_str(),
        "final_report": report_path
            .as_ref()
            .map(|path| Value::String(path.display().to_string()))
            .unwrap_or(Value::Null),
        "checkpoint": checkpoint_path.display().to_string(),
    });
    let event = core::event_value("finalized", core::redact_json(&event_payload), &now);
    let request = ledger_json::json!({
        "status": status.as_str(),
        "report": report.clone(),
    });
    commit_command(
        run_dir,
        core::LedgerCommand::Finalize(status),
        &request,
        None,
        operation_id,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Finalize(status))?;
            apply_checkpoint_to_run(&mut run, "final-report", &checkpoint_path, Some(hint), &now);
            if let Some(map) = run.as_object_mut() {
                map.insert(
                    "status".to_owned(),
                    Value::String(status.as_str().to_owned()),
                );
                map.insert("finalized_at".to_owned(), Value::String(now.clone()));
                if let Some(target) = &report_path
                    && let Some(Value::Object(paths)) = map.get_mut("paths")
                {
                    paths.insert(
                        "final_report".to_owned(),
                        Value::String(target.display().to_string()),
                    );
                }
            }
            let mut effects = Vec::new();
            if let (Some(target), Some(content)) = (&report_path, &report) {
                let relative = target.strip_prefix(run_dir).map_err(|_| {
                    domain(
                        Code::LedgerEntryInvalid,
                        target.display().to_string(),
                        "final report path escapes the run directory",
                    )
                })?;
                effects.push(ProjectionEffect::replace(
                    relative.to_string_lossy(),
                    content.clone(),
                ));
            }
            effects.push(ProjectionEffect::replace(
                checkpoint_relative.to_string_lossy(),
                String::from_utf8(json_bytes(&checkpoint_body)).unwrap_or_default(),
            ));
            effects.extend(append_event_effects(
                state, run_dir, &mut run, &event, None,
            )?);
            Ok((
                effects,
                ledger_json::json!({
                    "final_report": report_path
                        .as_ref()
                        .map(|path| Value::String(path.display().to_string()))
                        .unwrap_or(Value::Null),
                }),
            ))
        },
    )?;
    Ok(FinalizeOutcome {
        final_report: report_path,
    })
}

#[derive(Debug, Clone)]
pub enum DurableEffect {
    AppendEvent {
        event: String,
        payload: Value,
    },
    WriteJson {
        relative_path: PathBuf,
        payload: Value,
    },
    CreateJson {
        relative_path: PathBuf,
        payload: Value,
    },
    RemoveFile {
        relative_path: PathBuf,
        ignore_missing: bool,
    },
}

/// Commit one adapter-owned evidence directive as a single ledger revision.
///
/// The store knows only durable effect shapes. It does not decide policy,
/// select gates, authenticate receipts, or infer authority from event names.
pub fn commit_effect_batch(
    run_dir: &Path,
    effects: &[DurableEffect],
) -> Result<CommitOutcome, StoreError> {
    let request_effects = effects
        .iter()
        .map(|effect| match effect {
            DurableEffect::AppendEvent { event, payload } => ledger_json::json!({
                "effect": "append_event",
                "event": event,
                "payload": core::redact_json(payload),
            }),
            DurableEffect::WriteJson {
                relative_path,
                payload,
            } => ledger_json::json!({
                "effect": "write_json",
                "path": relative_path.display().to_string(),
                "payload": payload.clone(),
            }),
            DurableEffect::CreateJson {
                relative_path,
                payload,
            } => ledger_json::json!({
                "effect": "create_json",
                "path": relative_path.display().to_string(),
                "payload": payload.clone(),
            }),
            DurableEffect::RemoveFile {
                relative_path,
                ignore_missing,
            } => ledger_json::json!({
                "effect": "remove_file",
                "path": relative_path.display().to_string(),
                "ignore_missing": *ignore_missing,
            }),
        })
        .collect::<Vec<_>>();
    let request = ledger_json::json!({"effects": request_effects});
    let now = now_iso();
    commit_command(
        run_dir,
        core::LedgerCommand::Evidence,
        &request,
        None,
        None,
        |state| {
            let mut run = state.json("run.json", run_dir)?;
            validate_transition(Some(&run), run_dir, core::LedgerCommand::Evidence)?;
            let mut projections = Vec::new();
            let mut created = BTreeSet::new();
            for effect in effects {
                match effect {
                    DurableEffect::AppendEvent { event, payload } => {
                        let record = core::event_value(event, core::redact_json(payload), &now);
                        projections.extend(append_event_effects(
                            state, run_dir, &mut run, &record, None,
                        )?);
                    }
                    DurableEffect::WriteJson {
                        relative_path,
                        payload,
                    } => projections.push(ProjectionEffect::replace(
                        relative_path.to_string_lossy(),
                        String::from_utf8(json_bytes(payload)).unwrap_or_default(),
                    )),
                    DurableEffect::CreateJson {
                        relative_path,
                        payload,
                    } => {
                        let key = relative_path.to_string_lossy().into_owned();
                        if state.projections.contains_key(&key)
                            || state.history.contains_key(&key)
                            || !created.insert(key.clone())
                        {
                            return Err(domain(
                                Code::LedgerEntryInvalid,
                                run_dir.join(relative_path).display().to_string(),
                                "immutable evidence already exists",
                            ));
                        }
                        projections.push(ProjectionEffect::replace(
                            key,
                            String::from_utf8(json_bytes(payload)).unwrap_or_default(),
                        ));
                    }
                    DurableEffect::RemoveFile {
                        relative_path,
                        ignore_missing,
                    } => {
                        let key = relative_path.to_string_lossy().into_owned();
                        if state.projections.contains_key(&key) {
                            projections.push(ProjectionEffect::remove(key));
                        } else if !ignore_missing {
                            return Err(domain(
                                Code::LedgerEntryInvalid,
                                run_dir.join(relative_path).display().to_string(),
                                "evidence to remove does not exist",
                            ));
                        }
                    }
                }
            }
            Ok((projections, Value::Null))
        },
    )
}

// ---------------------------------------------------------------------------
// Read surfaces: scanning for resume and dashboard
// ---------------------------------------------------------------------------

pub struct RunSnapshot {
    pub entry: Value,
    pub revision: u64,
    pub transaction_digest: Option<String>,
}

/// Read one exact run and repair only already-journaled projections.
/// Legacy runs remain read-only and are not migrated by a continuation query.
pub fn read_run_snapshot(run_dir: &Path) -> Result<RunSnapshot, StoreError> {
    if run_dir.join("transactions").is_dir() {
        let _lock = RunLock::acquire(run_dir)?;
        let state = load_journal(run_dir)?;
        let entry = state.json("run.json", run_dir)?;
        Ok(RunSnapshot {
            entry,
            revision: state.revision,
            transaction_digest: state.digest,
        })
    } else {
        Ok(RunSnapshot {
            entry: read_json(&run_dir.join("run.json"))?,
            revision: 0,
            transaction_digest: None,
        })
    }
}

pub struct ScannedRun {
    pub entry: Value,
    pub run_dir: PathBuf,
}

pub struct Scan {
    pub runs: Vec<ScannedRun>,
    /// Unreadable or status-invalid runs surface here instead of vanishing.
    pub warnings: Vec<Diagnostic>,
}

/// Read every run for this repo (optionally one flow), keeping only runs
/// whose status parses and passes `allow`.
pub fn scan_runs(
    env: &LedgerEnv,
    flow: Option<&str>,
    allow: impl Fn(Status) -> bool,
) -> Result<Scan, StoreError> {
    let mut runs = Vec::new();
    let mut warnings = Vec::new();
    for root in candidate_roots(env, flow)? {
        if !root.is_dir() {
            continue;
        }
        let mut run_dirs: Vec<PathBuf> = fs::read_dir(&root)?
            .take(RUN_SCAN_LIMIT + 1)
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        if run_dirs.len() > RUN_SCAN_LIMIT {
            return Err(domain(
                Code::LedgerLimitExceeded,
                root.display().to_string(),
                format!("run ledger scan exceeds {RUN_SCAN_LIMIT} entries"),
            ));
        }
        run_dirs.sort();
        for run_dir in run_dirs {
            let run_file = run_dir.join("run.json");
            if !run_file.is_file() {
                continue;
            }
            let entry = match read_json(&run_file) {
                Ok(entry) => entry,
                Err(StoreError::Domain(diag)) => {
                    warnings.push(Diagnostic::warning(
                        Code::LedgerEntryInvalid,
                        run_file.display().to_string(),
                        format!("skipping unreadable run file: {}", diag.message),
                    ));
                    continue;
                }
                Err(StoreError::Io(err)) => {
                    warnings.push(Diagnostic::warning(
                        Code::LedgerEntryInvalid,
                        run_file.display().to_string(),
                        format!("skipping unreadable run file: {err}"),
                    ));
                    continue;
                }
            };
            let status_text = entry.get("status").and_then(Value::as_str).unwrap_or("");
            let Some(status) = Status::parse(status_text) else {
                warnings.push(Diagnostic::warning(
                    Code::LedgerStatusInvalid,
                    run_file.display().to_string(),
                    format!(
                        "skipping run with unknown status {status_text:?} (run-ledger-v1 statuses: running, interrupted, failed, completed)"
                    ),
                ));
                continue;
            };
            if allow(status) {
                runs.push(ScannedRun { entry, run_dir });
            }
        }
    }
    Ok(Scan { runs, warnings })
}

/// Latest attempt per gate for one run dir, newest first, capped at `limit`.
/// Mirrors Python `_collect_gate_history`.
pub fn collect_gate_history(run_dir: &Path, limit: usize) -> Vec<Value> {
    let gates_root = run_dir.join("artifacts").join("gates");
    if !gates_root.is_dir() {
        return Vec::new();
    }
    let mut keyed: Vec<(String, Value)> = Vec::new();
    for scope_dir in sorted_dirs(&gates_root) {
        let scope_name = dir_name(&scope_dir);
        for name_dir in sorted_dirs(&scope_dir) {
            let mut attempts = sorted_dirs(&name_dir);
            attempts.sort_by_key(|p| std::cmp::Reverse(dir_name(p).parse::<i64>().unwrap_or(0)));
            for attempt_dir in attempts {
                let envelope_path = attempt_dir.join("envelope.json");
                if !envelope_path.is_file() {
                    continue;
                }
                let Ok(envelope) = read_json(&envelope_path) else {
                    continue;
                };
                let gate_status = envelope
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let attempt_name = dir_name(&attempt_dir);
                let attempt_value = attempt_name
                    .parse::<i64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| Value::String(attempt_name));
                let mut entry = ledger_json::json!({
                    "name": envelope
                        .get("gate")
                        .and_then(|g| g.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or(&dir_name(&name_dir)),
                    "scope": scope_name,
                    "attempt": attempt_value,
                    "status": gate_status,
                    "path": envelope_path.display().to_string(),
                });
                if let Some(classification) = core::classify_gate(&envelope)
                    && let Some(map) = entry.as_object_mut()
                {
                    map.insert("classification".to_owned(), Value::String(classification));
                }
                let timestamp = envelope
                    .get("gate")
                    .and_then(|g| g.get("timestamp"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                keyed.push((timestamp, entry));
                break;
            }
        }
    }
    keyed.sort_by(|a, b| b.0.cmp(&a.0));
    keyed.into_iter().take(limit).map(|(_, e)| e).collect()
}

fn sorted_dirs(path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned()
}

/// Sort key used by resume (ascending) and dashboard (descending).
/// Python `r.get("updated_at") or r.get("started_at") or ""`: an empty
/// `updated_at` falls through to `started_at`, so filter on truthiness.
pub fn recency_key(entry: &Value) -> String {
    entry
        .get("updated_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .get("started_at")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env() -> (tempfile::TempDir, LedgerEnv) {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let root = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        let env = LedgerEnv {
            state_dir: root.join("state"),
            repo: root.join("repo"),
            repo_hash: "testhash0000".to_owned(),
        };
        (dir, env)
    }

    fn init_args<'a>() -> InitArgs<'a> {
        InitArgs {
            skill: "kickoff",
            flow: None,
            ticket_id: "TASK-19",
            ticket_title: "Port",
            ticket_url: "",
            branch: Some("feature/x"),
            run_id: None,
        }
    }

    fn test_transaction_path(run_dir: &Path, revision: u64) -> PathBuf {
        run_dir
            .join("transactions")
            .join(format!("{revision:020}.json"))
    }

    fn rewrite_transaction(run_dir: &Path, revision: u64, mutate: impl FnOnce(&mut Value)) {
        let path = test_transaction_path(run_dir, revision);
        let mut record = read_json(&path).unwrap_or_else(|_| panic!("read transaction"));
        mutate(&mut record);
        if let Some(map) = record.as_object_mut() {
            map.remove("digest");
        }
        let digest = transaction_digest(&record);
        if let Some(map) = record.as_object_mut() {
            map.insert("digest".to_owned(), Value::String(digest));
        }
        write_json_durable(&path, &record).unwrap_or_else(|_| panic!("rewrite transaction"));
    }

    #[test]
    fn init_creates_contract_tree_and_first_event() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|error| panic!("init: {error:?}"));
        assert_eq!(out.flow, "kickoff");
        let run_dir = &out.run_dir;
        for sub in [
            "artifacts/gates",
            "artifacts/reviews",
            "logs",
            "checkpoints",
        ] {
            assert!(run_dir.join(sub).is_dir(), "missing {sub}");
        }
        let run = read_json(&run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["kind"], "run-ledger-v1");
        assert_eq!(run["status"], "running");
        assert_eq!(run["events"]["count"], 1, "run_initialized counted");
        assert_eq!(run["branch"], "feature/x");
        let events = fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default();
        assert_eq!(events.lines().count(), 1);
        assert!(events.contains("\"type\": \"run_initialized\""));
        let transcript = fs::read_to_string(run_dir.join("transcript.md")).unwrap_or_default();
        assert!(transcript.starts_with("# Beislið run transcript\n"));
        assert_eq!(transcript.matches("\n## ").count(), 1);
    }

    #[test]
    fn explicit_run_id_collision_is_loud_and_auto_ids_retry() {
        let (_tmp, env) = temp_env();
        let mut args = init_args();
        args.run_id = Some("fixed-id");
        init_run(&env, &args).unwrap_or_else(|_| panic!("first init"));
        match init_run(&env, &args) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunIdCollision),
            other => panic!("expected collision, got {other:?}", other = other.is_ok()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn mutation_rejects_a_symlinked_run_ancestor_after_initialization() {
        use std::os::unix::fs::symlink;

        let (tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        let flow = env.state_dir.join("runs/kickoff");
        let moved = tmp.path().join("moved-flow");
        fs::rename(&flow, &moved).unwrap_or_else(|_| panic!("move flow"));
        symlink(&moved, &flow).unwrap_or_else(|_| panic!("link flow"));
        let before =
            fs::read(out.run_dir.join("events.jsonl")).unwrap_or_else(|_| panic!("read before"));

        assert!(append_event(&out.run_dir, "blocked", &ledger_json::json!({}), None).is_err());
        assert_eq!(
            fs::read(
                moved
                    .join(&env.repo_hash)
                    .join(out.run_dir.file_name().unwrap())
                    .join("events.jsonl")
            )
            .unwrap_or_else(|_| panic!("read after")),
            before
        );
    }

    #[test]
    fn invalid_explicit_run_id_is_rejected() {
        let (_tmp, env) = temp_env();
        let mut args = init_args();
        args.run_id = Some("../escape");
        match init_run(&env, &args) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunIdInvalid),
            _ => panic!("expected run_id_invalid"),
        }
    }

    #[test]
    fn append_event_keeps_count_lines_and_sections_in_step() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        for i in 0..3 {
            append_event(
                &out.run_dir,
                "step",
                &ledger_json::json!({"i": i, "token": "leak"}),
                None,
            )
            .unwrap_or_else(|_| panic!("event {i}"));
        }
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["events"]["count"], 4);
        let events = fs::read_to_string(out.run_dir.join("events.jsonl")).unwrap_or_default();
        assert_eq!(events.lines().count(), 4);
        assert!(events.contains("\"token\": \"[REDACTED]\""));
        let transcript = fs::read_to_string(out.run_dir.join("transcript.md")).unwrap_or_default();
        assert_eq!(transcript.matches("\n## ").count(), 4);
    }

    #[test]
    fn checkpoint_updates_run_entry_and_writes_document() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        let path = record_checkpoint(
            &out.run_dir,
            "ctx ready",
            &ledger_json::json!({"x": 1}),
            Some("resume here"),
        )
        .unwrap_or_else(|_| panic!("checkpoint"));
        assert!(path.ends_with("checkpoints/ctx-ready.json"));
        let body = read_json(&path).unwrap_or_else(|_| panic!("checkpoint body"));
        assert_eq!(body["kind"], "run-ledger-checkpoint-v1");
        assert_eq!(body["resume_hint"], "resume here");
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["current_step"], "ctx ready");
        assert_eq!(run["latest_checkpoint"]["name"], "ctx ready");
        assert_eq!(run["resume_hint"], "resume here");
        assert_eq!(run["checkpoints"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn gate_records_attempts_artifacts_checkpoint_and_event() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        let envelope = ledger_json::json!({
            "status": "fail",
            "environment_failure": false,
            "gate": {"name": "fmt", "scope": "Repo Wide", "timestamp": "T1"},
        });
        let first = record_gate(&out.run_dir, "fmt", None, &envelope, None)
            .unwrap_or_else(|_| panic!("gate 1"));
        let second = record_gate(&out.run_dir, "fmt", None, &envelope, None)
            .unwrap_or_else(|_| panic!("gate 2"));
        assert!(
            first
                .envelope_path
                .to_string_lossy()
                .contains("/Repo-Wide/fmt/1/")
        );
        assert!(
            second
                .envelope_path
                .to_string_lossy()
                .contains("/Repo-Wide/fmt/2/")
        );
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["artifacts"].as_array().map(Vec::len), Some(2));
        assert_eq!(run["logs"].as_array().map(Vec::len), Some(2));
        assert_eq!(run["current_step"], "gate-Repo-Wide-fmt");
        let history = collect_gate_history(&out.run_dir, 5);
        assert_eq!(history.len(), 1, "latest attempt per gate");
        assert_eq!(history[0]["attempt"], 2);
        assert_eq!(history[0]["classification"], "code_failure");
    }

    #[test]
    fn legacy_anchor_excludes_gate_runtime_without_poisoning_replay() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        fs::remove_dir_all(out.run_dir.join("transactions"))
            .unwrap_or_else(|_| panic!("remove journal"));
        let runtime = out.run_dir.join("artifacts/gate-runtime");
        fs::create_dir_all(&runtime).unwrap_or_else(|_| panic!("runtime dir"));
        fs::write(runtime.join("manifest.json"), b"runtime\n")
            .unwrap_or_else(|_| panic!("runtime manifest"));

        append_event(&out.run_dir, "continued", &ledger_json::json!({}), None)
            .unwrap_or_else(|_| panic!("append after anchor"));
        let state = load_journal(&out.run_dir).unwrap_or_else(|_| panic!("replay"));
        assert_eq!(state.revision, 1);
        assert_eq!(
            fs::read(runtime.join("manifest.json")).unwrap(),
            b"runtime\n"
        );
        let anchor =
            read_json(&test_transaction_path(&out.run_dir, 0)).unwrap_or_else(|_| panic!("anchor"));
        assert!(!core::json_line(&anchor).contains("gate-runtime"));
    }

    #[test]
    fn unsafe_legacy_projection_fails_before_anchor_publication() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        fs::remove_dir_all(out.run_dir.join("transactions"))
            .unwrap_or_else(|_| panic!("remove journal"));
        fs::write(out.run_dir.join("logs/.lock"), b"unsafe\n")
            .unwrap_or_else(|_| panic!("unsafe legacy file"));

        assert!(append_event(&out.run_dir, "blocked", &ledger_json::json!({}), None).is_err());
        assert!(!test_transaction_path(&out.run_dir, 0).exists());
    }

    #[test]
    fn replay_rejects_unknown_transaction_command_kind() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        rewrite_transaction(&out.run_dir, 1, |record| {
            record
                .as_object_mut()
                .and_then(|map| map.get_mut("command"))
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("command"))
                .insert("kind".to_owned(), Value::String("unknown".to_owned()));
        });
        assert!(load_journal(&out.run_dir).is_err());
    }

    #[test]
    fn replay_rejects_structurally_invalid_resume_sequence() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        record_interrupt(&out.run_dir, "pause", None).unwrap_or_else(|_| panic!("interrupt"));
        rewrite_transaction(&out.run_dir, 2, |record| {
            record
                .as_object_mut()
                .and_then(|map| map.get_mut("command"))
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("command"))
                .insert("kind".to_owned(), Value::String("resume".to_owned()));
        });
        assert!(load_journal(&out.run_dir).is_err());
    }

    #[test]
    fn replay_rejects_finalize_target_that_disagrees_with_projection() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        record_finalize(&out.run_dir, "completed", None).unwrap_or_else(|_| panic!("finalize"));
        rewrite_transaction(&out.run_dir, 2, |record| {
            record
                .as_object_mut()
                .and_then(|map| map.get_mut("command"))
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("command"))
                .insert(
                    "target_status".to_owned(),
                    Value::String("failed".to_owned()),
                );
        });
        assert!(load_journal(&out.run_dir).is_err());
    }

    #[test]
    fn interrupt_and_finalize_update_status() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        record_interrupt(&out.run_dir, "stop TOKEN=abc", Some("pick up at step 2"))
            .unwrap_or_else(|_| panic!("interrupt"));
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["status"], "interrupted");
        assert_eq!(run["interruption"]["reason"], "stop TOKEN=[REDACTED]");
        assert_eq!(run["resume_hint"], "pick up at step 2");

        match record_finalize(&out.run_dir, "running", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::LedgerStatusInvalid),
            _ => panic!("running must be rejected"),
        }
        record_finalize(&out.run_dir, "completed", None).unwrap_or_else(|_| panic!("finalize"));
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["status"], "completed");
        assert!(run.get("finalized_at").is_some());
    }

    #[test]
    fn find_run_dir_reports_missing_and_ambiguous() {
        let (_tmp, env) = temp_env();
        match find_run_dir(&env, "nope", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunNotFound),
            _ => panic!("expected run_not_found"),
        }
        let mut args = init_args();
        args.run_id = Some("same-id");
        init_run(&env, &args).unwrap_or_else(|_| panic!("init kickoff"));
        args.flow = Some("other");
        init_run(&env, &args).unwrap_or_else(|_| panic!("init other"));
        match find_run_dir(&env, "same-id", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunAmbiguous),
            _ => panic!("expected run_ambiguous"),
        }
        let found = find_run_dir(&env, "same-id", Some("other"))
            .unwrap_or_else(|_| panic!("flow disambiguation"));
        assert!(found.to_string_lossy().contains("/other/"));
    }

    #[test]
    fn scan_filters_status_and_warns_on_garbage() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        record_finalize(&out.run_dir, "completed", None).unwrap_or_else(|_| panic!("finalize"));
        let mut args = init_args();
        args.run_id = Some("second");
        init_run(&env, &args).unwrap_or_else(|_| panic!("init 2"));
        // A ghost-active run must be skipped with a warning, not accepted.
        let ghost_dir = env.run_root("kickoff").join("ghost");
        fs::create_dir_all(&ghost_dir).unwrap_or_else(|_| panic!("ghost dir"));
        fs::write(
            ghost_dir.join("run.json"),
            "{\"status\": \"active\", \"run_id\": \"ghost\"}\n",
        )
        .unwrap_or_else(|_| panic!("ghost run.json"));

        let scan = scan_runs(&env, None, Status::is_incomplete).unwrap_or_else(|_| panic!("scan"));
        let ids: Vec<&str> = scan
            .runs
            .iter()
            .filter_map(|r| r.entry.get("run_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["second"]);
        assert_eq!(scan.warnings.len(), 1);
        assert_eq!(scan.warnings[0].code, Code::LedgerStatusInvalid);

        let all = scan_runs(&env, None, |_| true).unwrap_or_else(|_| panic!("scan all"));
        assert_eq!(all.runs.len(), 2);
    }
}
