//! Deterministic project distribution compilation and installed-state verification.
//!
//! The bundle names portable packages and package-relative Pi resources. The
//! lock binds that contract to exact package evidence. This module owns all
//! parsing, matching, path safety, and hashing so launch, sync, update, and
//! scaffold cannot grow subtly different definitions of "current". It reads
//! local evidence but never contacts a registry, executes a package manager,
//! or mutates the package store. Those effects belong to CLI adapters.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

pub use crate::bundle::{AmbientInherit, ResourceKind};
use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Severity};

pub const BUNDLE_KIND: &str = "nopal.bundle/v2";
pub const LOCK_KIND: &str = "nopal.lock/v1";
pub const BUNDLE_PATH: &str = ".nopal/bundle.jsonc";
pub const LOCK_PATH: &str = ".nopal/nopal.lock";
const MAX_CONTROL_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BundleDocument {
    pub version: String,
    #[serde(default)]
    pub inherit_ambient: serde_json::Value,
    #[serde(default)]
    pub packages: Vec<PackageRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PackageRequest {
    pub id: String,
    pub source: SourceSpec,
    pub requirement: String,
    #[serde(default)]
    pub resources: Vec<ResourceRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceSpec {
    Builtin { package: String },
    Workspace { package: String, root: String },
    Npm { package: String, registry: String },
}

impl SourceSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Builtin { .. } => "builtin",
            Self::Workspace { .. } => "workspace",
            Self::Npm { .. } => "npm",
        }
    }

    pub fn package(&self) -> &str {
        match self {
            Self::Builtin { package }
            | Self::Workspace { package, .. }
            | Self::Npm { package, .. } => package,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResourceRequest {
    pub kind: ResourceKind,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LockDocument {
    pub version: String,
    pub contract_digest: String,
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LockedPackage {
    pub id: String,
    pub source: SourceSpec,
    pub resolved: String,
    pub artifact_integrity: String,
    pub installed_tree_integrity: String,
    pub resources: Vec<LockedResource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LockedResource {
    pub kind: ResourceKind,
    pub path: String,
    pub tree_integrity: String,
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinDistribution<'a> {
    pub version: &'a str,
    /// Root of the built-in package, not the repository or release root.
    pub root: &'a Path,
}

#[derive(Debug, Clone, Copy)]
pub struct DistributionContext<'a> {
    pub project_root: &'a Path,
    pub store_root: &'a Path,
    pub builtin: BuiltinDistribution<'a>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedResource {
    pub package_id: String,
    pub kind: ResourceKind,
    pub package_path: String,
    pub resolved_path: PathBuf,
    pub integrity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPackage {
    pub id: String,
    pub source_kind: String,
    pub package: String,
    pub resolved: String,
    pub root: PathBuf,
    pub integrity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DistributionReport {
    pub kind: &'static str,
    pub ok: bool,
    pub inherit_ambient: AmbientInherit,
    pub packages: Vec<ResolvedPackage>,
    pub resources: Vec<ResolvedResource>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct NpmResolution {
    pub package_id: String,
    pub resolved: String,
    pub artifact_integrity: String,
    pub root: PathBuf,
}

/// Return normalized package requests after the same strict validation used
/// by lock construction. Effect adapters use this only to determine which
/// explicit sources need resolution; it does not grant authority to prose or
/// ambient package settings.
pub fn package_requests(bundle_text: &str) -> Result<Vec<PackageRequest>, Vec<Diagnostic>> {
    let (mut bundle, mut diagnostics) = parse_bundle(bundle_text, BUNDLE_PATH);
    let Some(mut bundle) = bundle.take() else {
        diagnostics::sort(&mut diagnostics);
        return Err(diagnostics);
    };
    normalize_bundle(&mut bundle);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        diagnostics::sort(&mut diagnostics);
        return Err(diagnostics);
    }
    Ok(bundle.packages)
}

/// Build a complete lock using only sources already present on the machine.
/// It is used by first-run scaffolding and workspace-only update. An npm
/// request is deliberately unresolved here: only the CLI npm adapter may turn
/// registry evidence into a resolved archive and then call the resolved seam.
pub fn build_lock_from_local_sources(
    project_root: &Path,
    bundle_text: &str,
    builtin: &BuiltinDistribution<'_>,
) -> Result<LockDocument, Vec<Diagnostic>> {
    build_lock(project_root, bundle_text, builtin, &[])
}

/// Build a deterministic lock from package trees already resolved and
/// integrity-verified by an effect adapter. Core re-hashes every tree and
/// exported resource; it never runs npm or contacts a registry itself.
pub fn build_lock_from_resolved_sources(
    project_root: &Path,
    bundle_text: &str,
    builtin: &BuiltinDistribution<'_>,
    npm: &[NpmResolution],
) -> Result<LockDocument, Vec<Diagnostic>> {
    build_lock(project_root, bundle_text, builtin, npm)
}

fn build_lock(
    project_root: &Path,
    bundle_text: &str,
    builtin: &BuiltinDistribution<'_>,
    npm: &[NpmResolution],
) -> Result<LockDocument, Vec<Diagnostic>> {
    let (mut bundle, mut diagnostics) = parse_bundle(bundle_text, BUNDLE_PATH);
    let Some(mut bundle) = bundle.take() else {
        diagnostics::sort(&mut diagnostics);
        return Err(diagnostics);
    };
    normalize_bundle(&mut bundle);
    let npm = npm
        .iter()
        .map(|resolution| (resolution.package_id.as_str(), resolution))
        .collect::<BTreeMap<_, _>>();

    let mut locked = Vec::new();
    let mut resource_owners = BTreeMap::new();
    for request in &bundle.packages {
        let (root, resolved, artifact_integrity) = match &request.source {
            SourceSpec::Builtin { package } if package == "nopal" => {
                if !requirement_accepts_exact(&request.requirement, builtin.version) {
                    diagnostics.push(package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "resolution",
                        format!(
                            "requirement {:?} does not admit executing Nopal distribution {}",
                            request.requirement, builtin.version
                        ),
                    ));
                    continue;
                }
                (builtin.root.to_path_buf(), builtin.version.to_owned(), None)
            }
            SourceSpec::Builtin { .. } => {
                diagnostics.push(package_error(
                    Code::DistributionSourceUnsupported,
                    request,
                    "resolution",
                    "this Nopal build provides only builtin package \"nopal\"",
                ));
                continue;
            }
            SourceSpec::Workspace { package, root } => {
                let package_root = match resolve_workspace_root(project_root, root) {
                    Ok(path) => path,
                    Err(message) => {
                        diagnostics.push(package_error(
                            Code::DistributionPackageInvalid,
                            request,
                            "workspace_root",
                            message,
                        ));
                        continue;
                    }
                };
                let resolved = match package_manifest_version(
                    &package_root,
                    package,
                    request,
                    "workspace_manifest",
                ) {
                    Ok(version) => version,
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        continue;
                    }
                };
                if !requirement_accepts_exact(&request.requirement, &resolved) {
                    diagnostics.push(package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "resolution",
                        format!(
                            "workspace manifest version {resolved:?} does not satisfy exact requirement {:?}",
                            request.requirement
                        ),
                    ));
                    continue;
                }
                (package_root, resolved, None)
            }
            SourceSpec::Npm { .. } => {
                let Some(resolution) = npm.get(request.id.as_str()) else {
                    diagnostics.push(package_error(
                        Code::DistributionSourceUnsupported,
                        request,
                        "resolution",
                        "npm packages require resolution evidence from `nopal update`",
                    ));
                    continue;
                };
                if !is_exact_version(&resolution.resolved)
                    || !is_sha512_sri(&resolution.artifact_integrity)
                {
                    diagnostics.push(package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "npm_integrity",
                        "resolved npm evidence requires an exact version and SHA-512 SRI",
                    ));
                    continue;
                }
                if !requirement_accepts_exact(&request.requirement, &resolution.resolved) {
                    diagnostics.push(package_error(
                        Code::DistributionLockDrift,
                        request,
                        "resolution",
                        format!(
                            "npm resolved version {:?} does not match exact requirement {:?}",
                            resolution.resolved, request.requirement
                        ),
                    ));
                    continue;
                }
                let manifest_version = match package_manifest_version(
                    &resolution.root,
                    request.source.package(),
                    request,
                    "resolved_manifest",
                ) {
                    Ok(version) => version,
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        continue;
                    }
                };
                if manifest_version != resolution.resolved {
                    diagnostics.push(package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "resolved_manifest",
                        format!(
                            "resolved manifest version {manifest_version:?} does not match adapter evidence {:?}",
                            resolution.resolved
                        ),
                    ));
                    continue;
                }
                (
                    resolution.root.clone(),
                    resolution.resolved.clone(),
                    Some(resolution.artifact_integrity.clone()),
                )
            }
        };

        let builtin_package = matches!(request.source, SourceSpec::Builtin { .. });
        match lock_local_package(request, &root, resolved, builtin_package) {
            Ok(mut package) => {
                if let Some(integrity) = artifact_integrity {
                    package.artifact_integrity = integrity;
                }
                for resource in &package.resources {
                    match canonical_resource_path(&root, &resource.path) {
                        Ok(path) => {
                            record_resource_owner(
                                &mut resource_owners,
                                path,
                                request,
                                resource.kind,
                                &resource.path,
                                &mut diagnostics,
                            );
                        }
                        Err(error) => diagnostics.push(package_error(
                            Code::DistributionPackageInvalid,
                            request,
                            "resource_export",
                            format!(
                                "cannot resolve canonical resource path {:?}: {error}",
                                resource.path
                            ),
                        )),
                    }
                }
                locked.push(package);
            }
            Err(mut package_diagnostics) => diagnostics.append(&mut package_diagnostics),
        }
    }

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        diagnostics::sort(&mut diagnostics);
        return Err(diagnostics);
    }
    locked.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(LockDocument {
        version: LOCK_KIND.to_owned(),
        contract_digest: contract_digest(&bundle),
        packages: locked,
    })
}

pub fn lock_json(lock: &LockDocument) -> serde_json::Result<String> {
    serde_json::to_string_pretty(lock).map(|mut text| {
        text.push('\n');
        text
    })
}

/// Parse and structurally validate lock text through the same JSONC path used by launch.
/// Effect adapters reuse this seam so comments and diagnostics cannot drift by command.
pub fn parse_lock_text(lock_text: &str) -> Result<LockDocument, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let lock = parse_lock(lock_text, LOCK_PATH, &mut diagnostics);
    if let Some(lock) = &lock {
        validate_lock(lock, &mut diagnostics);
        if lock.version != LOCK_KIND {
            diagnostics.push(Diagnostic::error(
                Code::VersionUnsupported,
                LOCK_PATH,
                format!(
                    "unsupported distribution lock version {:?}; expected {LOCK_KIND:?}",
                    lock.version
                ),
            ));
        }
    }
    diagnostics::sort(&mut diagnostics);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        Err(diagnostics)
    } else if let Some(lock) = lock {
        Ok(lock)
    } else {
        Err(diagnostics)
    }
}

/// Resolve and verify every locked package and exported resource from local
/// evidence only. A failed report never carries partially trusted resources.
pub fn inspect(context: DistributionContext<'_>) -> io::Result<DistributionReport> {
    let bundle_text = match load_bundle_text(context.project_root)? {
        Ok(text) => text,
        Err(diagnostic) => return Ok(error_report(diagnostic)),
    };
    let lock_text = match load_lock_text(context.project_root)? {
        Ok(text) => text,
        Err(diagnostic) => return Ok(error_report(diagnostic)),
    };

    inspect_texts(context, &bundle_text, &lock_text)
}

pub fn load_bundle_text(root: &Path) -> io::Result<Result<String, Diagnostic>> {
    read_control_text(
        root,
        BUNDLE_PATH,
        Code::BundleMissing,
        "distribution contract",
    )
}

pub fn load_lock_text(root: &Path) -> io::Result<Result<String, Diagnostic>> {
    read_control_text(
        root,
        LOCK_PATH,
        Code::DistributionLockMissing,
        "distribution lock",
    )
}

fn read_control_text(
    root: &Path,
    relative: &str,
    missing_code: Code,
    label: &str,
) -> io::Result<Result<String, Diagnostic>> {
    let control_dir = root.join(".nopal");
    let path = root.join(relative);
    let directory_metadata = match fs::symlink_metadata(&control_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Err(Diagnostic::error(
                missing_code,
                relative,
                format!("{label} {relative} is missing"),
            )));
        }
        Err(error) => return Err(error),
    };
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
        return Ok(Err(Diagnostic::error(
            Code::DistributionPackageInvalid,
            relative,
            ".nopal control directory must be a real directory, not a link or special file",
        )));
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Err(Diagnostic::error(
                missing_code,
                relative,
                format!("{label} {relative} is missing"),
            )));
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(Err(Diagnostic::error(
            Code::DistributionPackageInvalid,
            relative,
            format!("{label} {relative} must be a real regular file"),
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Ok(Err(Diagnostic::error(
                Code::DistributionPackageInvalid,
                relative,
                format!("{label} {relative} must not have multiple hard links"),
            )));
        }
    }
    if metadata.len() > MAX_CONTROL_FILE_BYTES {
        return Ok(Err(Diagnostic::error(
            Code::DistributionPackageInvalid,
            relative,
            format!("{label} {relative} exceeds the {MAX_CONTROL_FILE_BYTES}-byte limit"),
        )));
    }
    let bytes = fs::read(&path)?;
    String::from_utf8(bytes).map_or_else(
        |_| {
            Ok(Err(Diagnostic::error(
                Code::DistributionPackageInvalid,
                relative,
                format!("{label} {relative} is not UTF-8"),
            )))
        },
        |text| Ok(Ok(text)),
    )
}

/// Inspect already-loaded contract and lock text through the identical path
/// used by on-disk launch. First-run dry-run and scaffold validation use this
/// seam so generated bytes do not receive a weaker proof path.
pub fn inspect_texts(
    context: DistributionContext<'_>,
    bundle_text: &str,
    lock_text: &str,
) -> io::Result<DistributionReport> {
    let (mut bundle, mut diagnostics) = parse_bundle(bundle_text, BUNDLE_PATH);
    let lock = match parse_lock_text(lock_text) {
        Ok(lock) => Some(lock),
        Err(mut lock_diagnostics) => {
            diagnostics.append(&mut lock_diagnostics);
            None
        }
    };
    let Some(mut bundle) = bundle.take() else {
        return Ok(report_with_diagnostics(diagnostics));
    };
    normalize_bundle(&mut bundle);
    let Some(lock) = lock else {
        return Ok(report_with_diagnostics(diagnostics));
    };

    let actual_contract_digest = contract_digest(&bundle);
    if lock.contract_digest != actual_contract_digest {
        diagnostics.push(Diagnostic::error(
            Code::DistributionLockDrift,
            LOCK_PATH,
            format!(
                "distribution contract changed since lock generation; expected contract digest {}, found {} at control boundary contract_lock",
                lock.contract_digest, actual_contract_digest
            ),
        ));
    }

    let requests = bundle
        .packages
        .iter()
        .map(|request| (request.id.as_str(), request))
        .collect::<BTreeMap<_, _>>();
    let locked = lock
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    for id in requests.keys() {
        if !locked.contains_key(id) {
            diagnostics.push(Diagnostic::error(
                Code::DistributionLockDrift,
                LOCK_PATH,
                format!("package {id:?} is absent from the lock at control boundary contract_lock"),
            ));
        }
    }
    for id in locked.keys() {
        if !requests.contains_key(id) {
            diagnostics.push(Diagnostic::error(
                Code::DistributionLockDrift,
                LOCK_PATH,
                format!(
                    "lock contains undeclared package {id:?} at control boundary contract_lock"
                ),
            ));
        }
    }

    let mut resolved_packages = Vec::new();
    let mut resolved_resources = Vec::new();
    let mut resource_owners = BTreeMap::new();
    for request in &bundle.packages {
        let Some(package) = locked.get(request.id.as_str()).copied() else {
            continue;
        };
        if package.source != request.source {
            diagnostics.push(locked_package_error(
                Code::DistributionLockDrift,
                package,
                "contract_lock",
                "locked source does not match the checked-in package source",
            ));
            continue;
        }
        if !requirement_accepts_exact(&request.requirement, &package.resolved) {
            diagnostics.push(locked_package_error(
                Code::DistributionLockDrift,
                package,
                "contract_lock",
                format!(
                    "locked version {:?} does not match exact requirement {:?}",
                    package.resolved, request.requirement
                ),
            ));
            continue;
        }
        let root = match installed_root(context, request, package) {
            Ok(root) => root,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                continue;
            }
        };
        if matches!(
            request.source,
            SourceSpec::Workspace { .. } | SourceSpec::Npm { .. }
        ) {
            let manifest_version = match package_manifest_version(
                &root,
                request.source.package(),
                request,
                "installed_manifest",
            ) {
                Ok(version) => version,
                Err(diagnostic) => {
                    diagnostics.push(diagnostic);
                    continue;
                }
            };
            if manifest_version != package.resolved {
                diagnostics.push(package_error(
                    Code::DistributionIntegrityMismatch,
                    request,
                    "installed_manifest",
                    format!(
                        "installed manifest version {manifest_version:?} does not match locked version {:?}",
                        package.resolved
                    ),
                ));
                continue;
            }
        }
        let tree_integrity = match if matches!(request.source, SourceSpec::Builtin { .. }) {
            hash_builtin_package(&root, request)
        } else {
            hash_tree(&root)
        } {
            Ok(integrity) => integrity,
            Err(error) => {
                diagnostics.push(package_error(
                    Code::DistributionPackageMissing,
                    request,
                    "installed_store",
                    format!(
                        "cannot read installed package root {}: {error}",
                        root.display()
                    ),
                ));
                continue;
            }
        };
        if tree_integrity != package.installed_tree_integrity {
            diagnostics.push(package_error(
                Code::DistributionIntegrityMismatch,
                request,
                "installed_integrity",
                format!(
                    "installed tree {} has integrity {tree_integrity}, expected {}",
                    root.display(),
                    package.installed_tree_integrity
                ),
            ));
            continue;
        }

        let request_resources = request
            .resources
            .iter()
            .map(|resource| ((resource.kind, resource.path.as_str()), resource))
            .collect::<BTreeMap<_, _>>();
        let locked_resources = package
            .resources
            .iter()
            .map(|resource| ((resource.kind, resource.path.as_str()), resource))
            .collect::<BTreeMap<_, _>>();
        if request_resources.keys().collect::<Vec<_>>()
            != locked_resources.keys().collect::<Vec<_>>()
        {
            diagnostics.push(locked_package_error(
                Code::DistributionLockDrift,
                package,
                "contract_lock",
                "locked resource exports do not match the checked-in package contract",
            ));
            continue;
        }

        let mut package_resources = Vec::new();
        for resource in &package.resources {
            let relative = match safe_relative_path(&resource.path) {
                Ok(path) => path,
                Err(message) => {
                    diagnostics.push(package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "resource_export",
                        message,
                    ));
                    continue;
                }
            };
            let resolved_path = root.join(relative);
            match hash_tree(&resolved_path) {
                Ok(integrity) if integrity == resource.tree_integrity => {
                    match canonical_resource_path(&root, &resource.path) {
                        Ok(canonical) => {
                            if record_resource_owner(
                                &mut resource_owners,
                                canonical,
                                request,
                                resource.kind,
                                &resource.path,
                                &mut diagnostics,
                            ) {
                                package_resources.push(ResolvedResource {
                                    package_id: package.id.clone(),
                                    kind: resource.kind,
                                    package_path: resource.path.clone(),
                                    resolved_path,
                                    integrity,
                                });
                            }
                        }
                        Err(error) => diagnostics.push(package_error(
                            Code::DistributionPackageInvalid,
                            request,
                            "resource_export",
                            format!(
                                "cannot resolve canonical resource path {:?}: {error}",
                                resource.path
                            ),
                        )),
                    }
                }
                Ok(integrity) => diagnostics.push(package_error(
                    Code::DistributionIntegrityMismatch,
                    request,
                    "resource_integrity",
                    format!(
                        "resource {:?} has integrity {integrity}, expected {}",
                        resource.path, resource.tree_integrity
                    ),
                )),
                Err(error) => diagnostics.push(package_error(
                    Code::DistributionPackageMissing,
                    request,
                    "resource_export",
                    format!("resource {:?} is unavailable: {error}", resource.path),
                )),
            }
        }
        resolved_packages.push(ResolvedPackage {
            id: package.id.clone(),
            source_kind: package.source.kind().to_owned(),
            package: package.source.package().to_owned(),
            resolved: package.resolved.clone(),
            root,
            integrity: package.installed_tree_integrity.clone(),
        });
        resolved_resources.extend(package_resources);
    }

    let inherit_ambient = parse_ambient(&bundle.inherit_ambient, &mut diagnostics);
    diagnostics::sort(&mut diagnostics);
    let ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);
    if !ok {
        resolved_packages.clear();
        resolved_resources.clear();
    }
    Ok(DistributionReport {
        kind: "nopal.distribution/v1",
        ok,
        inherit_ambient,
        packages: resolved_packages,
        resources: resolved_resources,
        diagnostics,
    })
}

fn canonical_resource_path(root: &Path, package_path: &str) -> io::Result<PathBuf> {
    let relative = safe_relative_path(package_path).map_err(io::Error::other)?;
    fs::canonicalize(root.join(relative))
}

fn record_resource_owner(
    owners: &mut BTreeMap<PathBuf, (String, ResourceKind, String)>,
    canonical: PathBuf,
    request: &PackageRequest,
    kind: ResourceKind,
    package_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if let Some((owner_id, owner_kind, owner_path)) = owners.get(&canonical) {
        diagnostics.push(package_error(
            Code::DistributionPackageInvalid,
            request,
            "resource_export",
            format!(
                "canonical resource {} ({} {package_path:?}) is already exported by package {owner_id:?} as {} {owner_path:?}",
                canonical.display(),
                kind.as_str(),
                owner_kind.as_str()
            ),
        ));
        false
    } else {
        owners.insert(
            canonical,
            (request.id.clone(), kind, package_path.to_owned()),
        );
        true
    }
}

fn installed_root(
    context: DistributionContext<'_>,
    request: &PackageRequest,
    package: &LockedPackage,
) -> Result<PathBuf, Diagnostic> {
    match &request.source {
        SourceSpec::Builtin { package: name } if name == "nopal" => {
            if package.resolved != context.builtin.version {
                return Err(package_error(
                    Code::DistributionPackageMissing,
                    request,
                    "builtin_distribution",
                    format!(
                        "lock requires Nopal {}, but the executing distribution is {}",
                        package.resolved, context.builtin.version
                    ),
                ));
            }
            Ok(context.builtin.root.to_path_buf())
        }
        SourceSpec::Workspace { root, .. } => resolve_workspace_root(context.project_root, root)
            .map_err(|message| {
                package_error(
                    Code::DistributionPackageInvalid,
                    request,
                    "workspace_root",
                    message,
                )
            }),
        SourceSpec::Npm { package: name, .. } => {
            let identity = digest_bytes(
                format!(
                    "{name}\0{}\0{}",
                    package.resolved, package.artifact_integrity
                )
                .as_bytes(),
            );
            let relative = Path::new("npm").join(identity.trim_start_matches("sha256:"));
            resolve_confined_directory(context.store_root, &relative, "npm store").map_err(
                |message| {
                    package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "installed_store",
                        message,
                    )
                },
            )
        }
        _ => Err(package_error(
            Code::DistributionSourceUnsupported,
            request,
            "resolution",
            "builtin package is unavailable in this Nopal distribution",
        )),
    }
}

pub fn npm_store_path(store_root: &Path, package: &str, version: &str, integrity: &str) -> PathBuf {
    let identity = digest_bytes(format!("{package}\0{version}\0{integrity}").as_bytes());
    store_root
        .join("npm")
        .join(identity.trim_start_matches("sha256:"))
}

fn lock_local_package(
    request: &PackageRequest,
    root: &Path,
    resolved: String,
    builtin: bool,
) -> Result<LockedPackage, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let installed_tree_integrity = match if builtin {
        hash_builtin_package(root, request)
    } else {
        hash_tree(root)
    } {
        Ok(integrity) => integrity,
        Err(error) => {
            diagnostics.push(package_error(
                Code::DistributionPackageMissing,
                request,
                "resolution",
                format!("package root {} is unavailable: {error}", root.display()),
            ));
            return Err(diagnostics);
        }
    };
    let mut resources = Vec::new();
    for resource in &request.resources {
        let relative = match safe_relative_path(&resource.path) {
            Ok(path) => path,
            Err(message) => {
                diagnostics.push(package_error(
                    Code::DistributionPackageInvalid,
                    request,
                    "resource_export",
                    message,
                ));
                continue;
            }
        };
        let path = root.join(relative);
        match hash_tree(&path) {
            Ok(tree_integrity) => resources.push(LockedResource {
                kind: resource.kind,
                path: resource.path.clone(),
                tree_integrity,
            }),
            Err(error) => diagnostics.push(package_error(
                Code::DistributionPackageMissing,
                request,
                "resource_export",
                format!("resource {:?} is unavailable: {error}", resource.path),
            )),
        }
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(diagnostics);
    }
    resources.sort_by(|left, right| {
        (left.kind.as_str(), left.path.as_str()).cmp(&(right.kind.as_str(), right.path.as_str()))
    });
    Ok(LockedPackage {
        id: request.id.clone(),
        source: request.source.clone(),
        resolved,
        artifact_integrity: installed_tree_integrity.clone(),
        installed_tree_integrity,
        resources,
    })
}

fn parse_bundle(text: &str, path: &str) -> (Option<BundleDocument>, Vec<Diagnostic>) {
    let value = match config::parse_jsonc(text, path, Code::BundleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let mut diagnostics = Vec::new();
    let bundle = match serde_json::from_value::<BundleDocument>(value) {
        Ok(bundle) => bundle,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                Code::BundleParseError,
                path,
                format!("invalid distribution contract: {error}"),
            ));
            return (None, diagnostics);
        }
    };
    if bundle.version != BUNDLE_KIND {
        diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!(
                "unsupported bundle version {:?}; expected {BUNDLE_KIND:?}",
                bundle.version
            ),
        ));
    }
    validate_bundle(&bundle, path, &mut diagnostics);
    (Some(bundle), diagnostics)
}

fn parse_lock(text: &str, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Option<LockDocument> {
    let value = match config::parse_jsonc(text, path, Code::DistributionLockParseError) {
        Ok(value) => value,
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return None;
        }
    };
    match serde_json::from_value(value) {
        Ok(lock) => Some(lock),
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                Code::DistributionLockParseError,
                path,
                format!("invalid distribution lock: {error}"),
            ));
            None
        }
    }
}

fn validate_lock(lock: &LockDocument, diagnostics: &mut Vec<Diagnostic>) {
    let mut ids = BTreeSet::new();
    for package in &lock.packages {
        if !is_portable_id(&package.id) || package.id.is_empty() {
            diagnostics.push(locked_package_error(
                Code::DistributionLockParseError,
                package,
                "contract_lock",
                "locked package id is not portable",
            ));
        }
        if !ids.insert(package.id.as_str()) {
            diagnostics.push(locked_package_error(
                Code::DuplicateId,
                package,
                "contract_lock",
                "duplicate locked package id",
            ));
        }
        if !is_exact_version(&package.resolved) {
            diagnostics.push(locked_package_error(
                Code::DistributionLockParseError,
                package,
                "contract_lock",
                "locked package has no valid exact semantic version",
            ));
        }
        if !is_sha256_integrity(&package.installed_tree_integrity) {
            diagnostics.push(locked_package_error(
                Code::DistributionLockParseError,
                package,
                "installed_integrity",
                "locked package has invalid installed tree integrity",
            ));
        }
        match &package.source {
            SourceSpec::Npm { .. } if !is_sha512_sri(&package.artifact_integrity) => {
                diagnostics.push(locked_package_error(
                    Code::DistributionLockParseError,
                    package,
                    "npm_integrity",
                    "locked npm package requires valid SHA-512 artifact SRI",
                ));
            }
            SourceSpec::Npm { .. } => {}
            _ if package.artifact_integrity != package.installed_tree_integrity => {
                diagnostics.push(locked_package_error(
                    Code::DistributionLockDrift,
                    package,
                    "installed_integrity",
                    "locked local artifact and installed-tree integrity differ",
                ));
            }
            _ => {}
        }
        let mut resources = BTreeSet::new();
        for resource in &package.resources {
            if !resources.insert((resource.kind, resource.path.as_str())) {
                diagnostics.push(locked_package_error(
                    Code::DuplicateId,
                    package,
                    "resource_export",
                    format!(
                        "lock repeats {:?} resource {:?}",
                        resource.kind, resource.path
                    ),
                ));
            }
            if safe_relative_path(&resource.path).is_err()
                || !is_sha256_integrity(&resource.tree_integrity)
            {
                diagnostics.push(locked_package_error(
                    Code::DistributionLockParseError,
                    package,
                    "resource_integrity",
                    format!("lock has invalid resource evidence for {:?}", resource.path),
                ));
            }
        }
    }
}

fn is_sha256_integrity(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn is_sha512_sri(value: &str) -> bool {
    let Some(encoded) = value.strip_prefix("sha512-") else {
        return false;
    };
    !encoded.is_empty()
        && !encoded.chars().any(char::is_whitespace)
        && base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .is_ok_and(|digest| digest.len() == 64)
}

fn is_safe_registry(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn validate_bundle(bundle: &BundleDocument, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let mut ids = BTreeSet::new();
    for request in &bundle.packages {
        if request.id.trim().is_empty() || request.id.len() > 128 || !is_portable_id(&request.id) {
            diagnostics.push(Diagnostic::error(
                Code::DistributionPackageInvalid,
                path,
                format!(
                    "package id {:?} must contain only ASCII letters, digits, '.', '_' or '-' at control boundary contract",
                    request.id
                ),
            ));
        }
        if !ids.insert(request.id.as_str()) {
            diagnostics.push(Diagnostic::error(
                Code::DuplicateId,
                path,
                format!("duplicate distribution package id {:?}", request.id),
            ));
        }
        if !is_package_identity(request.source.package()) {
            diagnostics.push(package_error(
                Code::DistributionPackageInvalid,
                request,
                "contract",
                "package source identity must be a bounded lowercase npm-style name",
            ));
        }
        if exact_requirement(&request.requirement).is_none() {
            diagnostics.push(package_error(
                Code::DistributionPackageInvalid,
                request,
                "contract",
                "v0.3 package requirements must be exact semantic versions, optionally prefixed by '='",
            ));
        }
        match &request.source {
            SourceSpec::Workspace { root, .. } => {
                if let Err(message) = safe_relative_path(root) {
                    diagnostics.push(package_error(
                        Code::DistributionPackageInvalid,
                        request,
                        "contract",
                        message,
                    ));
                }
            }
            SourceSpec::Npm { registry, .. } if !is_safe_registry(registry) => {
                diagnostics.push(package_error(
                    Code::DistributionPackageInvalid,
                    request,
                    "contract",
                    "npm registry must be a credential-free HTTPS origin without query or fragment",
                ));
            }
            _ => {}
        }
        let mut resources = BTreeSet::new();
        for resource in &request.resources {
            if !resources.insert((resource.kind, resource.path.as_str())) {
                diagnostics.push(package_error(
                    Code::DistributionPackageInvalid,
                    request,
                    "resource_export",
                    format!(
                        "duplicate {:?} resource path {:?}",
                        resource.kind, resource.path
                    ),
                ));
            }
            if resource.kind == ResourceKind::Extension
                && !matches!(
                    &request.source,
                    SourceSpec::Builtin { package } if package == "nopal"
                )
            {
                diagnostics.push(package_error(
                    Code::DistributionSourceUnsupported,
                    request,
                    "launch_adapter",
                    "third-party executable Pi extensions are not supported by the enforced v0.3 profile",
                ));
            }
            if let Err(message) = safe_relative_path(&resource.path) {
                diagnostics.push(package_error(
                    Code::DistributionPackageInvalid,
                    request,
                    "resource_export",
                    message,
                ));
            }
        }
    }
    let _ = parse_ambient(&bundle.inherit_ambient, diagnostics);
}

fn parse_ambient(value: &serde_json::Value, diagnostics: &mut Vec<Diagnostic>) -> AmbientInherit {
    if value.is_null() {
        return AmbientInherit::NONE;
    }
    if let Some(enabled) = value.as_bool() {
        return if enabled {
            AmbientInherit::ALL
        } else {
            AmbientInherit::NONE
        };
    }
    let Some(items) = value.as_array() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            BUNDLE_PATH,
            "inherit_ambient must be a boolean or an array of resource kind names",
        ));
        return AmbientInherit::NONE;
    };
    let mut inherit = AmbientInherit::NONE;
    for item in items {
        match item.as_str() {
            Some("extensions") => inherit.extensions = true,
            Some("skills") => inherit.skills = true,
            Some("prompt_templates") => inherit.prompt_templates = true,
            Some("themes") => inherit.themes = true,
            Some(other) => diagnostics.push(Diagnostic::warning(
                Code::BundleAmbientKindUnknown,
                BUNDLE_PATH,
                format!("unknown ambient resource kind {other:?}; not inherited"),
            )),
            None => diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                BUNDLE_PATH,
                "inherit_ambient entries must be strings",
            )),
        }
    }
    inherit
}

fn normalize_bundle(bundle: &mut BundleDocument) {
    bundle
        .packages
        .sort_by(|left, right| left.id.cmp(&right.id));
    for package in &mut bundle.packages {
        package.resources.sort_by(|left, right| {
            (left.kind.as_str(), left.path.as_str())
                .cmp(&(right.kind.as_str(), right.path.as_str()))
        });
    }
}

fn contract_digest(bundle: &BundleDocument) -> String {
    digest_bytes(&serde_json::to_vec(bundle).unwrap_or_default())
}

fn exact_requirement(requirement: &str) -> Option<&str> {
    let value = requirement.trim();
    let version = value.strip_prefix('=').unwrap_or(value);
    (value == requirement
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && is_exact_version(version))
    .then_some(version)
}

fn requirement_accepts_exact(requirement: &str, version: &str) -> bool {
    exact_requirement(requirement) == Some(version)
}

fn resolve_workspace_root(project_root: &Path, value: &str) -> Result<PathBuf, String> {
    let relative = safe_relative_path(value)?;
    resolve_confined_directory(project_root, &relative, "workspace root")
}

fn resolve_confined_directory(
    boundary_root: &Path,
    relative: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let boundary = boundary_root.canonicalize().map_err(|error| {
        format!(
            "cannot canonicalize {label} boundary {}: {error}",
            boundary_root.display()
        )
    })?;
    let mut current = boundary.clone();
    for component in relative.components() {
        if component == Component::CurDir {
            continue;
        }
        let Component::Normal(part) = component else {
            return Err(format!(
                "{label} path {} is not portable",
                relative.display()
            ));
        };
        current.push(part);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "{label} component {} is unavailable: {error}",
                current.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{label} component {} is a symbolic link",
                current.display()
            ));
        }
        if !metadata.is_dir() {
            return Err(format!(
                "{label} component {} is not a directory",
                current.display()
            ));
        }
    }
    let resolved = current
        .canonicalize()
        .map_err(|error| format!("cannot canonicalize {label} {}: {error}", current.display()))?;
    if !resolved.starts_with(&boundary) {
        return Err(format!(
            "{label} {} escapes boundary {}",
            resolved.display(),
            boundary.display()
        ));
    }
    Ok(resolved)
}

fn package_manifest_version(
    root: &Path,
    expected_name: &str,
    request: &PackageRequest,
    boundary: &str,
) -> Result<String, Diagnostic> {
    #[derive(Deserialize)]
    struct Manifest {
        name: String,
        version: String,
    }

    let path = root.join("package.json");
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        package_error(
            Code::DistributionPackageMissing,
            request,
            boundary,
            format!("cannot inspect {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(package_error(
            Code::DistributionPackageInvalid,
            request,
            boundary,
            "package.json must be a regular file no larger than 1 MiB",
        ));
    }
    let text = fs::read_to_string(&path).map_err(|error| {
        package_error(
            Code::DistributionPackageInvalid,
            request,
            boundary,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| {
        package_error(
            Code::DistributionPackageInvalid,
            request,
            boundary,
            format!("invalid {}: {error}", path.display()),
        )
    })?;
    if manifest.name != expected_name {
        return Err(package_error(
            Code::DistributionPackageInvalid,
            request,
            boundary,
            format!(
                "manifest package name {:?} does not match source identity {expected_name:?}",
                manifest.name
            ),
        ));
    }
    if !is_exact_version(&manifest.version) {
        return Err(package_error(
            Code::DistributionPackageInvalid,
            request,
            boundary,
            "manifest version must be an exact semantic version",
        ));
    }
    Ok(manifest.version)
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    if value.trim().is_empty() {
        return Err("package-relative path must not be empty".to_owned());
    }
    if value != "."
        && (value.contains(['\\', ':', '\0'])
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | "..")))
    {
        return Err(format!(
            "package-relative path {value:?} is not one canonical portable spelling"
        ));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)) && value != ".")
    {
        return Err(format!(
            "package-relative path {value:?} must not be absolute or contain traversal"
        ));
    }
    Ok(path.to_path_buf())
}

fn is_portable_id(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_exact_version(value: &str) -> bool {
    let (without_build, build) = value
        .split_once('+')
        .map_or((value, None), |(version, build)| (version, Some(build)));
    if build.is_some_and(|identifiers| !valid_version_identifiers(identifiers, false)) {
        return false;
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    if prerelease.is_some_and(|identifiers| !valid_version_identifiers(identifiers, true)) {
        return false;
    }
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == &"0" || !part.starts_with('0'))
        })
}

fn valid_version_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_numeric_leading_zero
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || identifier == "0"
                    || !identifier.starts_with('0'))
        })
}

fn is_package_identity(value: &str) -> bool {
    if value.is_empty() || value.len() > 214 {
        return false;
    }
    let segment_ok = |segment: &str| {
        !segment.is_empty()
            && segment.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
            && segment
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
    };
    if let Some(scoped) = value.strip_prefix('@') {
        let Some((scope, name)) = scoped.split_once('/') else {
            return false;
        };
        !name.contains('/') && segment_ok(scope) && segment_ok(name)
    } else {
        !value.contains(['/', '@']) && segment_ok(value)
    }
}

fn hash_builtin_package(root: &Path, request: &PackageRequest) -> io::Result<String> {
    if request
        .resources
        .iter()
        .any(|resource| resource.path == ".")
    {
        return hash_tree(root);
    }
    let mut selected = request
        .resources
        .iter()
        .map(|resource| resource.path.clone())
        .collect::<BTreeSet<_>>();
    for resource in &request.resources {
        let path = Path::new(&resource.path);
        if resource.kind == ResourceKind::Extension
            && path.file_name().is_some_and(|name| name == "index.ts")
        {
            let parent = path.parent().unwrap_or_else(|| Path::new(""));
            for support in ["classifier.ts", "nopal-cli.ts"] {
                selected.insert(parent.join(support).to_string_lossy().into_owned());
            }
        }
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "builtin package root is not a real directory: {}",
            root.display()
        )));
    }
    let mut hasher = Sha256::new();
    hasher.update(b"dir\0.\0");
    for relative in selected {
        let relative_path = safe_relative_path(&relative)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
        hash_entry(&root.join(&relative_path), &relative_path, &mut hasher)?;
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn hash_tree(root: &Path) -> io::Result<String> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "symlink package evidence is not portable: {}",
            root.display()
        )));
    }
    let mut hasher = Sha256::new();
    hash_entry(root, Path::new("."), &mut hasher)?;
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn hash_entry(path: &Path, relative: &Path, hasher: &mut Sha256) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "symlink package evidence is not portable: {}",
            path.display()
        )));
    }
    let relative_text = portable_tree_path(relative)?;
    if metadata.is_file() {
        hasher.update(b"file\0");
        hasher.update(relative_text.as_bytes());
        hasher.update(b"\0");
        hasher.update(fs::read(path)?);
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "unsupported package entry type: {}",
            path.display()
        )));
    }
    hasher.update(b"dir\0");
    hasher.update(relative_text.as_bytes());
    hasher.update(b"\0");
    let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        hash_entry(&child.path(), &relative.join(child.file_name()), hasher)?;
    }
    Ok(())
}

fn portable_tree_path(relative: &Path) -> io::Result<String> {
    if relative == Path::new(".") {
        return Ok(".".to_owned());
    }
    let mut parts = Vec::new();
    for component in relative.components() {
        if component == Component::CurDir {
            continue;
        }
        let Component::Normal(part) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("package tree path {} is not relative", relative.display()),
            ));
        };
        let text = part.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("package tree path {} is not UTF-8", relative.display()),
            )
        })?;
        if text.is_empty() || text.contains(['\\', ':', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "package tree path {} is not portable across supported platforms",
                    relative.display()
                ),
            ));
        }
        parts.push(text);
    }
    Ok(parts.join("/"))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn locked_package_error(
    code: Code,
    package: &LockedPackage,
    boundary: &str,
    detail: impl AsRef<str>,
) -> Diagnostic {
    Diagnostic::error(
        code,
        LOCK_PATH,
        format!(
            "package {:?} from {} source {:?} failed at control boundary {boundary}: {}",
            package.id,
            package.source.kind(),
            package.source.package(),
            detail.as_ref()
        ),
    )
}

fn package_error(
    code: Code,
    request: &PackageRequest,
    boundary: &str,
    detail: impl AsRef<str>,
) -> Diagnostic {
    Diagnostic::error(
        code,
        BUNDLE_PATH,
        format!(
            "package {:?} from {} source {:?} failed at control boundary {boundary}: {}",
            request.id,
            request.source.kind(),
            request.source.package(),
            detail.as_ref()
        ),
    )
}

fn error_report(diagnostic: Diagnostic) -> DistributionReport {
    report_with_diagnostics(vec![diagnostic])
}

fn report_with_diagnostics(mut diagnostics: Vec<Diagnostic>) -> DistributionReport {
    diagnostics::sort(&mut diagnostics);
    DistributionReport {
        kind: "nopal.distribution/v1",
        ok: false,
        inherit_ambient: AmbientInherit::NONE,
        packages: Vec::new(),
        resources: Vec::new(),
        diagnostics,
    }
}
