//! Side-effecting adapters for explicit distribution update and sync commands.
//!
//! Core owns what constitutes a valid contract, lock, and installed tree.
//! This module owns the narrow filesystem/process seam needed to produce or
//! materialize that evidence. Bare launch never calls into this module.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use nopal_core::diagnostics::{Diagnostic, Severity};
use nopal_core::distribution::{
    self, BuiltinDistribution, DistributionContext, DistributionReport, LockDocument,
};
use serde::Serialize;

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
    builtin: BuiltinDistribution<'_>,
    write: bool,
) -> io::Result<UpdateReport> {
    let bundle_text = match fs::read_to_string(project_root.join(distribution::BUNDLE_PATH)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(UpdateReport {
                kind: "nopal.update/v1",
                ok: false,
                wrote: false,
                lock_path: distribution::LOCK_PATH.to_owned(),
                packages: Vec::new(),
                diagnostics: vec![Diagnostic::error(
                    nopal_core::diagnostics::Code::BundleMissing,
                    distribution::BUNDLE_PATH,
                    "cannot update a missing distribution contract",
                )],
                proposal: None,
            });
        }
        Err(error) => return Err(error),
    };
    let lock =
        match distribution::build_lock_from_local_sources(project_root, &bundle_text, &builtin) {
            Ok(lock) => lock,
            Err(diagnostics) => {
                return Ok(UpdateReport {
                    kind: "nopal.update/v1",
                    ok: false,
                    wrote: false,
                    lock_path: distribution::LOCK_PATH.to_owned(),
                    packages: Vec::new(),
                    diagnostics,
                    proposal: None,
                });
            }
        };
    let packages = lock_summaries(&lock);
    if write {
        let text = distribution::lock_json(&lock).map_err(io::Error::other)?;
        write_text_durable(&project_root.join(distribution::LOCK_PATH), &text)?;
    }
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

/// Synchronization is verification-only for builtin and workspace packages:
/// both are already materialized by the Nopal installation or the repository.
/// npm execution is added by the npm adapter without changing this report
/// contract. Crucially, sync never writes the lock.
pub fn sync(context: DistributionContext<'_>) -> io::Result<SyncReport> {
    let report = distribution::inspect(context)?;
    Ok(sync_report(report))
}

fn sync_report(report: DistributionReport) -> SyncReport {
    SyncReport {
        kind: "nopal.sync/v1",
        ok: report.ok,
        changed: false,
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
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
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
