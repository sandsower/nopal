//! Side-effecting adapters for explicit distribution update and sync commands.
//!
//! Core owns what constitutes a valid contract, lock, and installed tree.
//! This module owns registry access, archive verification, and durable writes.
//! Bare launch never calls into this module and therefore remains offline.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use flate2::read::GzDecoder;
use nopal_core::diagnostics::{Code, Diagnostic, Severity};
use nopal_core::distribution::{
    self, BuiltinDistribution, DistributionContext, DistributionReport, LockDocument,
    LockedPackage, NpmResolution, PackageRequest, SourceSpec,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha512};
use tempfile::TempDir;

const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 20_000;
const MAX_NPM_STDOUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_NPM_STDERR_BYTES: usize = 64 * 1024;
const NPM_TIMEOUT: Duration = Duration::from_secs(120);

fn acquire_mutation_lock(store_root: &Path) -> io::Result<File> {
    fs::create_dir_all(store_root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(store_root.join(".distribution.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

#[derive(Debug, Clone, Serialize)]
pub struct LockedPackageSummary {
    pub id: String,
    pub source: String,
    pub package: String,
    pub resolved: String,
    pub integrity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateReport {
    pub kind: &'static str,
    pub ok: bool,
    pub wrote: bool,
    pub lock_path: String,
    pub packages: Vec<LockedPackageSummary>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<LockDocument>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub kind: &'static str,
    pub ok: bool,
    pub changed: bool,
    pub packages: Vec<LockedPackageSummary>,
    pub resources: usize,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn update(
    project_root: &Path,
    store_root: &Path,
    builtin: BuiltinDistribution<'_>,
    npm_program: &OsStr,
    write: bool,
) -> io::Result<UpdateReport> {
    let _mutation_lock = acquire_mutation_lock(store_root)?;
    let bundle_text = match distribution::load_bundle_text(project_root)? {
        Ok(text) => text,
        Err(diagnostic) => return Ok(update_failure(vec![diagnostic])),
    };
    let requests = match distribution::package_requests(&bundle_text) {
        Ok(requests) => requests,
        Err(diagnostics) => return Ok(update_failure(diagnostics)),
    };

    // Temporary extraction is intentional even for preview: Core must hash
    // the exact resolved package tree before it can propose an authoritative
    // lock. No preview bytes enter the durable package store.
    let mut temporary = Vec::new();
    let mut npm_resolutions = Vec::new();
    let mut npm_diagnostics = Vec::new();
    for request in requests
        .iter()
        .filter(|request| matches!(request.source, SourceSpec::Npm { .. }))
    {
        match resolve_npm(npm_program, request, None) {
            Ok(resolution) => {
                npm_resolutions.push(resolution.evidence);
                temporary.push(resolution.temporary);
            }
            Err(diagnostic) => npm_diagnostics.push(diagnostic),
        }
    }
    if !npm_diagnostics.is_empty() {
        return Ok(update_failure(npm_diagnostics));
    }

    let lock = match distribution::build_lock_from_resolved_sources(
        project_root,
        &bundle_text,
        &builtin,
        &npm_resolutions,
    ) {
        Ok(lock) => lock,
        Err(diagnostics) => return Ok(update_failure(diagnostics)),
    };
    let current_bundle_text = match distribution::load_bundle_text(project_root)? {
        Ok(text) => text,
        Err(diagnostic) => return Ok(update_failure(vec![diagnostic])),
    };
    if current_bundle_text != bundle_text {
        return Ok(update_failure(vec![Diagnostic::error(
            Code::DistributionLockDrift,
            distribution::BUNDLE_PATH,
            "distribution contract changed during update at control boundary update_transaction; no lock was written",
        )]));
    }
    let current_proposal = match distribution::build_lock_from_resolved_sources(
        project_root,
        &current_bundle_text,
        &builtin,
        &npm_resolutions,
    ) {
        Ok(proposal) => proposal,
        Err(diagnostics) => return Ok(update_failure(diagnostics)),
    };
    if current_proposal != lock {
        return Ok(update_failure(vec![Diagnostic::error(
            Code::DistributionLockDrift,
            distribution::LOCK_PATH,
            "distribution evidence changed during update at control boundary update_transaction; no lock was written",
        )]));
    }

    let packages = lock_summaries(&lock);
    if write {
        let text = distribution::lock_json(&lock).map_err(io::Error::other)?;
        write_text_durable(&project_root.join(distribution::LOCK_PATH), &text)?;
        let written_text = match distribution::load_lock_text(project_root)? {
            Ok(text) => text,
            Err(diagnostic) => return Ok(update_published_failure(&lock, vec![diagnostic])),
        };
        let written_lock = match distribution::parse_lock_text(&written_text) {
            Ok(written_lock) => written_lock,
            Err(diagnostics) => return Ok(update_published_failure(&lock, diagnostics)),
        };
        if written_lock != lock {
            return Ok(update_published_failure(
                &lock,
                vec![Diagnostic::error(
                    Code::DistributionLockDrift,
                    distribution::LOCK_PATH,
                    "written distribution lock differs from the verified proposal at control boundary update_transaction",
                )],
            ));
        }
        let post_write_bundle = match distribution::load_bundle_text(project_root)? {
            Ok(text) => text,
            Err(diagnostic) => return Ok(update_published_failure(&lock, vec![diagnostic])),
        };
        if post_write_bundle != bundle_text {
            return Ok(update_published_failure(
                &lock,
                vec![Diagnostic::error(
                    Code::DistributionLockDrift,
                    distribution::BUNDLE_PATH,
                    "distribution contract changed while the lock was published at control boundary update_transaction; update did not report stale authority as current",
                )],
            ));
        }
    }
    drop(temporary);
    Ok(UpdateReport {
        kind: "nopal.update/v1",
        ok: true,
        wrote: write,
        lock_path: distribution::LOCK_PATH.to_owned(),
        packages,
        diagnostics: Vec::new(),
        proposal: Some(lock),
    })
}

pub fn sync(context: DistributionContext<'_>, npm_program: &OsStr) -> io::Result<SyncReport> {
    let _mutation_lock = acquire_mutation_lock(context.store_root)?;
    let bundle_text = match distribution::load_bundle_text(context.project_root)? {
        Ok(text) => text,
        Err(diagnostic) => return Ok(sync_failure(vec![diagnostic])),
    };
    let lock_text = match distribution::load_lock_text(context.project_root)? {
        Ok(text) => text,
        Err(diagnostic) => return Ok(sync_failure(vec![diagnostic])),
    };
    let initial = distribution::inspect_texts(context, &bundle_text, &lock_text)?;
    if initial.ok {
        return Ok(sync_report(initial, false));
    }
    if initial.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code,
            Code::BundleMissing
                | Code::BundleParseError
                | Code::DistributionLockMissing
                | Code::DistributionLockParseError
                | Code::DistributionLockDrift
                | Code::VersionUnsupported
                | Code::DistributionSourceUnsupported
        )
    }) {
        return Ok(sync_report(initial, false));
    }

    let requests = match distribution::package_requests(&bundle_text) {
        Ok(requests) => requests,
        Err(diagnostics) => return Ok(sync_failure(diagnostics)),
    };
    let lock = match distribution::parse_lock_text(&lock_text) {
        Ok(lock) => lock,
        Err(diagnostics) => return Ok(sync_failure(diagnostics)),
    };

    let mut changed = false;
    for request in requests
        .iter()
        .filter(|request| matches!(request.source, SourceSpec::Npm { .. }))
    {
        let Some(locked) = lock
            .packages
            .iter()
            .find(|package| package.id == request.id)
        else {
            return Ok(sync_failure(vec![npm_diagnostic(
                request,
                Code::DistributionLockDrift,
                "contract_lock",
                "npm package is absent from the exact lock",
            )]));
        };
        let target = match secure_npm_install_target(
            context.store_root,
            request.source.package(),
            &locked.resolved,
            &locked.artifact_integrity,
        ) {
            Ok(target) => target,
            Err(error) => {
                return Ok(sync_failure(vec![npm_diagnostic(
                    request,
                    Code::DistributionPackageInvalid,
                    "installed_store",
                    error.to_string(),
                )]));
            }
        };
        if installed_package_is_exact(&target, locked) {
            continue;
        }

        let resolved = match resolve_npm(npm_program, request, Some(locked)) {
            Ok(resolved) => resolved,
            Err(diagnostic) => return Ok(sync_failure(vec![diagnostic])),
        };
        if let Err(diagnostic) = verify_locked_tree(request, locked, &resolved.evidence.root) {
            return Ok(sync_failure(vec![diagnostic]));
        }
        install_tree(&resolved.evidence.root, &target)?;
        changed = true;
    }

    let verified = distribution::inspect(context)?;
    Ok(sync_report(verified, changed))
}

fn installed_package_is_exact(root: &Path, locked: &LockedPackage) -> bool {
    distribution::hash_tree(root)
        .is_ok_and(|integrity| integrity == locked.installed_tree_integrity)
        && locked.resources.iter().all(|resource| {
            distribution::hash_tree(&root.join(&resource.path))
                .is_ok_and(|integrity| integrity == resource.tree_integrity)
        })
}

fn verify_locked_tree(
    request: &PackageRequest,
    locked: &LockedPackage,
    root: &Path,
) -> Result<(), Diagnostic> {
    let actual = distribution::hash_tree(root).map_err(|error| {
        npm_diagnostic(
            request,
            Code::DistributionPackageMissing,
            "archive_extraction",
            format!("cannot hash extracted package tree: {error}"),
        )
    })?;
    if actual != locked.installed_tree_integrity {
        return Err(npm_diagnostic(
            request,
            Code::DistributionIntegrityMismatch,
            "installed_integrity",
            format!(
                "exact archive produced tree {actual}, expected {}",
                locked.installed_tree_integrity
            ),
        ));
    }
    for resource in &locked.resources {
        let actual = distribution::hash_tree(&root.join(&resource.path)).map_err(|error| {
            npm_diagnostic(
                request,
                Code::DistributionPackageMissing,
                "resource_export",
                format!(
                    "locked resource {:?} is unavailable: {error}",
                    resource.path
                ),
            )
        })?;
        if actual != resource.tree_integrity {
            return Err(npm_diagnostic(
                request,
                Code::DistributionIntegrityMismatch,
                "resource_integrity",
                format!(
                    "locked resource {:?} has tree {actual}, expected {}",
                    resource.path, resource.tree_integrity
                ),
            ));
        }
    }
    Ok(())
}

struct TemporaryResolution {
    evidence: NpmResolution,
    temporary: TempDir,
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_exceeded: bool,
    stderr_exceeded: bool,
    timed_out: bool,
}

fn bounded_output(
    command: &mut Command,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> io::Result<BoundedOutput> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("npm stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("npm stderr pipe is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout, stdout_limit));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr, stderr_limit));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            child.kill()?;
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| io::Error::other("npm stdout reader panicked"))??;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| io::Error::other("npm stderr reader panicked"))??;
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
        stdout_exceeded,
        stderr_exceeded,
        timed_out,
    })
}

fn read_bounded(mut reader: impl io::Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut exceeded = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(captured.len());
        let retained = read.min(remaining);
        captured.extend_from_slice(&buffer[..retained]);
        exceeded |= retained < read;
    }
    Ok((captured, exceeded))
}

#[derive(Debug, Deserialize)]
struct NpmPackResult {
    name: String,
    version: String,
    filename: String,
    integrity: String,
}

fn resolve_npm(
    npm_program: &OsStr,
    request: &PackageRequest,
    locked: Option<&LockedPackage>,
) -> Result<TemporaryResolution, Diagnostic> {
    let SourceSpec::Npm { package, registry } = &request.source else {
        unreachable!("resolve_npm is called only for npm sources");
    };
    let temporary = tempfile::Builder::new()
        .prefix("nopal-npm-")
        .tempdir()
        .map_err(|error| {
            npm_diagnostic(
                request,
                Code::DistributionPackageMissing,
                "npm_pack",
                format!("cannot create isolated npm staging directory: {error}"),
            )
        })?;
    let spec = locked.map_or_else(
        || format!("{package}@{}", request.requirement),
        |package_lock| format!("{package}@{}", package_lock.resolved),
    );
    let mut command = Command::new(npm_program);
    command
        .arg("pack")
        .arg(&spec)
        .arg("--json")
        .arg("--ignore-scripts")
        .arg("--pack-destination")
        .arg(temporary.path())
        .arg("--registry")
        .arg(registry)
        .arg("--audit=false")
        .arg("--fund=false")
        .arg("--update-notifier=false")
        .arg("--loglevel=error")
        .current_dir(temporary.path());
    let output = bounded_output(
        &mut command,
        MAX_NPM_STDOUT_BYTES,
        MAX_NPM_STDERR_BYTES,
        NPM_TIMEOUT,
    )
    .map_err(|error| {
        npm_diagnostic(
            request,
            Code::DistributionPackageMissing,
            "npm_pack",
            format!("cannot execute npm pack: {error}"),
        )
    })?;
    if output.timed_out {
        return Err(npm_diagnostic(
            request,
            Code::DistributionPackageMissing,
            "npm_pack",
            format!(
                "npm pack exceeded the {}-second deadline",
                NPM_TIMEOUT.as_secs()
            ),
        ));
    }
    if output.stdout_exceeded || output.stderr_exceeded {
        return Err(npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "npm_pack",
            format!(
                "npm pack output exceeded bounded capture limits (stdout {MAX_NPM_STDOUT_BYTES} bytes, stderr {MAX_NPM_STDERR_BYTES} bytes)"
            ),
        ));
    }
    if !output.status.success() {
        return Err(npm_diagnostic(
            request,
            Code::DistributionPackageMissing,
            "npm_pack",
            format!(
                "npm pack exited with {}; stderr: {}",
                output.status,
                bounded_utf8(&output.stderr)
            ),
        ));
    }
    let results: Vec<NpmPackResult> = serde_json::from_slice(&output.stdout).map_err(|error| {
        npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "npm_pack",
            format!("npm pack returned invalid JSON evidence: {error}"),
        )
    })?;
    let [result] = results.as_slice() else {
        return Err(npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "npm_pack",
            "npm pack must return exactly one resolved package",
        ));
    };
    if result.name != *package || result.version.trim().is_empty() {
        return Err(npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "npm_pack",
            format!(
                "npm resolved {}@{}, expected package {package:?}",
                result.name, result.version
            ),
        ));
    }
    if let Some(package_lock) = locked
        && (result.version != package_lock.resolved
            || result.integrity != package_lock.artifact_integrity)
    {
        return Err(npm_diagnostic(
            request,
            Code::DistributionIntegrityMismatch,
            "npm_registry_evidence",
            format!(
                "registry returned {} with {}, expected {} with {}",
                result.version,
                result.integrity,
                package_lock.resolved,
                package_lock.artifact_integrity
            ),
        ));
    }
    let filename = safe_filename(&result.filename).ok_or_else(|| {
        npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "npm_pack",
            format!("npm returned unsafe archive filename {:?}", result.filename),
        )
    })?;
    let archive = temporary.path().join(filename);
    verify_sri(&archive, &result.integrity).map_err(|message| {
        npm_diagnostic(
            request,
            Code::DistributionIntegrityMismatch,
            "npm_integrity",
            message,
        )
    })?;
    let root = temporary.path().join("extracted");
    extract_npm_archive(&archive, &root).map_err(|message| {
        npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "archive_extraction",
            message,
        )
    })?;
    verify_npm_manifest(&root, package, &result.version).map_err(|message| {
        npm_diagnostic(
            request,
            Code::DistributionPackageInvalid,
            "archive_identity",
            message,
        )
    })?;
    Ok(TemporaryResolution {
        evidence: NpmResolution {
            package_id: request.id.clone(),
            resolved: result.version.clone(),
            artifact_integrity: result.integrity.clone(),
            root,
        },
        temporary,
    })
}

fn verify_npm_manifest(
    root: &Path,
    expected_name: &str,
    expected_version: &str,
) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Manifest {
        name: String,
        version: String,
    }

    let path = root.join("package.json");
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("extracted package manifest is unavailable: {error}"))?;
    if metadata.len() > 1024 * 1024 {
        return Err("extracted package manifest exceeds the 1 MiB limit".to_owned());
    }
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read extracted package manifest: {error}"))?;
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|error| format!("extracted package manifest is invalid: {error}"))?;
    if manifest.name != expected_name || manifest.version != expected_version {
        return Err(format!(
            "extracted package identifies as {}@{}, expected {expected_name}@{expected_version}",
            manifest.name, manifest.version
        ));
    }
    Ok(())
}

fn safe_filename(value: &str) -> Option<&str> {
    let path = Path::new(value);
    if path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)))
        && !value.contains(['/', '\\'])
    {
        Some(value)
    } else {
        None
    }
}

fn verify_sri(path: &Path, integrity: &str) -> Result<(), String> {
    let encoded = integrity
        .split_whitespace()
        .find_map(|token| token.strip_prefix("sha512-"))
        .ok_or_else(|| "npm artifact integrity must contain SHA-512 SRI".to_owned())?;
    let expected = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "npm artifact SHA-512 SRI is not valid base64".to_owned())?;
    if expected.len() != 64 {
        return Err("npm artifact SHA-512 SRI has the wrong digest length".to_owned());
    }
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect npm artifact {}: {error}", path.display()))?;
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "npm artifact exceeds the {MAX_ARCHIVE_BYTES}-byte compressed limit"
        ));
    }
    let mut file = File::open(path)
        .map_err(|error| format!("cannot open npm artifact {}: {error}", path.display()))?;
    let mut hasher = Sha512::new();
    io::copy(&mut file, &mut hasher)
        .map_err(|error| format!("cannot hash npm artifact: {error}"))?;
    let actual = hasher.finalize();
    if actual.as_slice() != expected.as_slice() {
        return Err("downloaded npm artifact does not match its SHA-512 SRI".to_owned());
    }
    Ok(())
}

fn extract_npm_archive(archive_path: &Path, root: &Path) -> Result<(), String> {
    fs::create_dir(root).map_err(|error| format!("cannot create extraction root: {error}"))?;
    let file = File::open(archive_path).map_err(|error| format!("cannot open archive: {error}"))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let entries = archive
        .entries()
        .map_err(|error| format!("cannot read gzip/tar archive: {error}"))?;
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0_u64;
    let mut count = 0_usize;
    for entry in entries {
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry limit"
            ));
        }
        let mut entry = entry.map_err(|error| format!("invalid tar entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("invalid tar path: {error}"))?;
        let relative = npm_archive_relative(&path)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !seen.insert(relative.clone()) {
            return Err(format!("archive repeats path {}", relative.display()));
        }
        let kind = entry.header().entry_type();
        let destination = root.join(&relative);
        if kind.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("cannot create archive directory: {error}"))?;
            continue;
        }
        if !kind.is_file() {
            return Err(format!(
                "archive path {} uses unsupported entry type; links and special files are forbidden",
                relative.display()
            ));
        }
        let size = entry.size();
        if size > MAX_ENTRY_BYTES || total_bytes.saturating_add(size) > MAX_ARCHIVE_BYTES {
            return Err("archive exceeds the expanded byte limit".to_owned());
        }
        total_bytes += size;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create archive parent: {error}"))?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| format!("cannot create extracted file: {error}"))?;
        #[cfg(unix)]
        let mode = entry.header().mode().ok();
        io::copy(&mut (&mut entry).take(MAX_ENTRY_BYTES + 1), &mut output)
            .map_err(|error| format!("cannot extract archive file: {error}"))?;
        output
            .sync_all()
            .map_err(|error| format!("cannot sync extracted file: {error}"))?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt as _;
            let executable = mode & 0o111;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o644 | executable))
                .map_err(|error| format!("cannot set extracted file mode: {error}"))?;
        }
    }
    Ok(())
}

fn npm_archive_relative(path: &Path) -> Result<PathBuf, String> {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(prefix)) if prefix == "package") {
        return Err(format!(
            "archive path {} is outside the required package/ root",
            path.display()
        ));
    }
    let mut result = PathBuf::new();
    for component in components {
        let Component::Normal(part) = component else {
            return Err(format!("archive path {} is not portable", path.display()));
        };
        let text = part
            .to_str()
            .ok_or_else(|| format!("archive path {} is not UTF-8", path.display()))?;
        if text.is_empty() || text.contains(['\\', ':']) {
            return Err(format!("archive path {} is not portable", path.display()));
        }
        result.push(part);
    }
    Ok(result)
}

fn secure_npm_install_target(
    store_root: &Path,
    package: &str,
    version: &str,
    integrity: &str,
) -> io::Result<PathBuf> {
    fs::create_dir_all(store_root)?;
    let store = store_root.canonicalize()?;
    let npm = store.join("npm");
    match fs::symlink_metadata(&npm) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::other(format!(
                "npm store parent is not a real directory: {}",
                npm.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&npm)?,
        Err(error) => return Err(error),
    }
    let resolved_npm = npm.canonicalize()?;
    if !resolved_npm.starts_with(&store) {
        return Err(io::Error::other(format!(
            "npm store parent {} escapes configured store {}",
            resolved_npm.display(),
            store.display()
        )));
    }
    Ok(distribution::npm_store_path(
        &store, package, version, integrity,
    ))
}

fn install_tree(source: &Path, target: &Path) -> io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".staging.{}.{}",
        std::process::id(),
        unique_nonce()?
    ));
    copy_tree(source, &staging)?;
    let source_integrity = distribution::hash_tree(source)?;
    let staging_integrity = distribution::hash_tree(&staging)?;
    if source_integrity != staging_integrity {
        fs::remove_dir_all(&staging)?;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "staged package tree does not match the verified source tree",
        ));
    }
    let old = parent.join(format!(".old.{}.{}", std::process::id(), unique_nonce()?));
    let had_old = target.exists();
    if had_old {
        fs::rename(target, &old)?;
    }
    if let Err(error) = fs::rename(&staging, target) {
        if had_old {
            let _ = fs::rename(&old, target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if had_old {
        fs::remove_dir_all(old)?;
    }
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir(target)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resolved package tree contains a symbolic link",
            ));
        }
        if file_type.is_dir() {
            copy_tree(&source_path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path)?;
            File::open(&target_path)?.sync_all()?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resolved package tree contains a special file",
            ));
        }
    }
    if let Ok(directory) = File::open(target) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn update_failure(mut diagnostics: Vec<Diagnostic>) -> UpdateReport {
    nopal_core::diagnostics::sort(&mut diagnostics);
    UpdateReport {
        kind: "nopal.update/v1",
        ok: false,
        wrote: false,
        lock_path: distribution::LOCK_PATH.to_owned(),
        packages: Vec::new(),
        diagnostics,
        proposal: None,
    }
}

fn update_published_failure(lock: &LockDocument, mut diagnostics: Vec<Diagnostic>) -> UpdateReport {
    nopal_core::diagnostics::sort(&mut diagnostics);
    UpdateReport {
        kind: "nopal.update/v1",
        ok: false,
        wrote: true,
        lock_path: distribution::LOCK_PATH.to_owned(),
        packages: lock_summaries(lock),
        diagnostics,
        proposal: Some(lock.clone()),
    }
}

fn sync_failure(mut diagnostics: Vec<Diagnostic>) -> SyncReport {
    nopal_core::diagnostics::sort(&mut diagnostics);
    SyncReport {
        kind: "nopal.sync/v1",
        ok: false,
        changed: false,
        packages: Vec::new(),
        resources: 0,
        diagnostics,
    }
}

fn sync_report(report: DistributionReport, changed: bool) -> SyncReport {
    SyncReport {
        kind: "nopal.sync/v1",
        ok: report.ok,
        changed,
        packages: report
            .packages
            .iter()
            .map(|package| LockedPackageSummary {
                id: package.id.clone(),
                source: package.source_kind.clone(),
                package: package.package.clone(),
                resolved: package.resolved.clone(),
                integrity: package.integrity.clone(),
            })
            .collect(),
        resources: report.resources.len(),
        diagnostics: report.diagnostics,
    }
}

fn lock_summaries(lock: &LockDocument) -> Vec<LockedPackageSummary> {
    lock.packages
        .iter()
        .map(|package| LockedPackageSummary {
            id: package.id.clone(),
            source: package.source.kind().to_owned(),
            package: package.source.package().to_owned(),
            resolved: package.resolved.clone(),
            integrity: package.installed_tree_integrity.clone(),
        })
        .collect()
}

fn npm_diagnostic(
    request: &PackageRequest,
    code: Code,
    boundary: &str,
    message: impl AsRef<str>,
) -> Diagnostic {
    Diagnostic::error(
        code,
        distribution::BUNDLE_PATH,
        format!(
            "package {:?} from npm source {:?} failed at control boundary {boundary}: {}",
            request.id,
            request.source.package(),
            message.as_ref()
        ),
    )
}

fn bounded_utf8(bytes: &[u8]) -> String {
    const LIMIT: usize = 2_048;
    let value = String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)]);
    if bytes.len() > LIMIT {
        format!("{value}…[truncated]")
    } else {
        value.into_owned()
    }
}

/// Atomic replace is correct for `update --write`: unlike first-run scaffold,
/// replacement is the explicitly requested operation. A failed write leaves
/// either the previous complete lock or the new complete lock.
fn write_text_durable(path: &Path, text: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("nopal.lock");
    let temporary = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        unique_nonce()?
    ));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn unique_nonce() -> io::Result<u128> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(io::Error::other)
}

pub fn npm_program() -> OsString {
    OsString::from("npm")
}

pub fn human_update(report: &UpdateReport) -> String {
    if !report.ok {
        return report
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code.as_str(), diagnostic.message))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let verb = if report.wrote { "wrote" } else { "proposed" };
    format!(
        "nopal: {verb} {} with {} exact packages\n",
        report.lock_path,
        report.packages.len()
    )
}

pub fn human_sync(report: &SyncReport) -> String {
    if report.ok {
        return format!(
            "nopal: distribution current; {} packages, {} resources\n",
            report.packages.len(),
            report.resources
        );
    }
    report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| format!("{}: {}", diagnostic.code.as_str(), diagnostic.message))
        .collect::<Vec<_>>()
        .join("\n")
}
