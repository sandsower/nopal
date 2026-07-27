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

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::bundle::{AmbientInherit, ResourceKind};
use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Severity};

pub const BUNDLE_KIND: &str = "nopal.bundle/v2";
pub const LOCK_KIND: &str = "nopal.lock/v1";
pub const BUNDLE_PATH: &str = ".nopal/bundle.jsonc";
pub const LOCK_PATH: &str = ".nopal/nopal.lock";

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

/// Build a complete lock using only sources already present on the machine.
/// It is used by first-run scaffolding and the workspace/builtin half of
/// update. An npm request is deliberately unresolved here: only the CLI npm
/// adapter may turn registry evidence into a locked package.
pub fn build_lock_from_local_sources(
    project_root: &Path,
    bundle_text: &str,
    builtin: &BuiltinDistribution<'_>,
) -> Result<LockDocument, Vec<Diagnostic>> {
    let (mut bundle, mut diagnostics) = parse_bundle(bundle_text, BUNDLE_PATH);
    let Some(mut bundle) = bundle.take() else {
        diagnostics::sort(&mut diagnostics);
        return Err(diagnostics);
    };
    normalize_bundle(&mut bundle);

    let mut locked = Vec::new();
    for request in &bundle.packages {
        let (root, resolved) = match &request.source {
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
                (builtin.root.to_path_buf(), builtin.version.to_owned())
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
            SourceSpec::Workspace { root, .. } => {
                let relative = match safe_relative_path(root) {
                    Ok(path) => path,
                    Err(message) => {
                        diagnostics.push(package_error(
                            Code::DistributionPackageInvalid,
                            request,
                            "contract",
                            message,
                        ));
                        continue;
                    }
                };
                (project_root.join(relative), request.requirement.clone())
            }
            SourceSpec::Npm { .. } => {
                diagnostics.push(package_error(
                    Code::DistributionSourceUnsupported,
                    request,
                    "resolution",
                    "npm packages require resolution evidence from `nopal update`",
                ));
                continue;
            }
        };

        match lock_local_package(request, &root, resolved) {
            Ok(package) => locked.push(package),
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

/// Resolve and verify every locked package and exported resource from local
/// evidence only. A failed report never carries partially trusted resources.
pub fn inspect(context: DistributionContext<'_>) -> io::Result<DistributionReport> {
    let bundle_text = match fs::read_to_string(context.project_root.join(BUNDLE_PATH)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(error_report(Diagnostic::error(
                Code::BundleMissing,
                BUNDLE_PATH,
                format!("distribution contract {BUNDLE_PATH} is missing"),
            )));
        }
        Err(error) => return Err(error),
    };
    let lock_text = match fs::read_to_string(context.project_root.join(LOCK_PATH)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(error_report(Diagnostic::error(
                Code::DistributionLockMissing,
                LOCK_PATH,
                format!("distribution lock {LOCK_PATH} is missing"),
            )));
        }
        Err(error) => return Err(error),
    };

    inspect_texts(context, &bundle_text, &lock_text)
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
    let lock = parse_lock(lock_text, LOCK_PATH, &mut diagnostics);
    let Some(mut bundle) = bundle.take() else {
        return Ok(report_with_diagnostics(diagnostics));
    };
    normalize_bundle(&mut bundle);
    let Some(lock) = lock else {
        return Ok(report_with_diagnostics(diagnostics));
    };

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
    for request in &bundle.packages {
        let Some(package) = locked.get(request.id.as_str()).copied() else {
            continue;
        };
        if package.source != request.source {
            diagnostics.push(package_error(
                Code::DistributionLockDrift,
                request,
                "contract_lock",
                "locked source does not match the checked-in package source",
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
        let tree_integrity = match hash_tree(&root) {
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
            diagnostics.push(package_error(
                Code::DistributionLockDrift,
                request,
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
                    package_resources.push(ResolvedResource {
                        package_id: package.id.clone(),
                        kind: resource.kind,
                        package_path: resource.path.clone(),
                        resolved_path,
                        integrity,
                    });
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
        inherit_ambient: parse_ambient(&bundle.inherit_ambient, &mut diagnostics),
        packages: resolved_packages,
        resources: resolved_resources,
        diagnostics,
    })
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
        SourceSpec::Workspace { root, .. } => safe_relative_path(root)
            .map(|relative| context.project_root.join(relative))
            .map_err(|message| {
                package_error(
                    Code::DistributionPackageInvalid,
                    request,
                    "contract",
                    message,
                )
            }),
        SourceSpec::Npm { package: name, .. } => Ok(npm_store_path(
            context.store_root,
            name,
            &package.resolved,
            &package.artifact_integrity,
        )),
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
) -> Result<LockedPackage, Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let installed_tree_integrity = match hash_tree(root) {
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

fn validate_bundle(bundle: &BundleDocument, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let mut ids = BTreeSet::new();
    for request in &bundle.packages {
        if request.id.trim().is_empty() || !is_portable_id(&request.id) {
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
        if request.requirement.trim().is_empty() {
            diagnostics.push(package_error(
                Code::DistributionPackageInvalid,
                request,
                "contract",
                "requirement must not be empty",
            ));
        }
        for resource in &request.resources {
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

fn requirement_accepts_exact(requirement: &str, version: &str) -> bool {
    requirement.trim() == version || requirement.trim() == format!("={version}")
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    if value.trim().is_empty() {
        return Err("package-relative path must not be empty".to_owned());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
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
    let relative_text = relative.to_string_lossy().replace('\\', "/");
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

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
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
