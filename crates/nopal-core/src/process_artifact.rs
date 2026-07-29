//! Normalized Nopal process artifact export.
//!
//! `nopal.process_artifact/v1` is the cold, deterministic snapshot of the
//! `.nopal/` process source tree. It records source hashes, normalized parsed
//! module JSON, and validation diagnostics so consumers can detect drift
//! without reparsing JSONC or Beislið-era prompt prose.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Severity};
use crate::discover;
use crate::gates;
use crate::guidance;
use crate::policy;
use crate::profile::{Module, Profile};
use crate::review_policy;
use crate::roots;
use crate::toon::{self, Value};
use crate::validate;
use crate::workflow;

pub const PROCESS_ARTIFACT_KIND: &str = "nopal.process_artifact/v1";
pub const PROCESS_EXPORT_KIND: &str = "nopal.process_artifact.export/v1";
pub const PROCESS_CHECK_KIND: &str = "nopal.process_artifact.check/v1";

const DEFAULT_ARTIFACT_PATH: &str = ".nopal/process-artifact.json";

#[derive(Debug, Clone, Serialize)]
pub struct ProcessArtifact {
    pub kind: &'static str,
    pub project: Option<String>,
    pub profile: Option<Profile>,
    pub sources: Vec<SourceMeta>,
    pub modules: BTreeMap<String, serde_json::Value>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ProcessArtifact {
    pub fn ok(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceMeta {
    pub path: String,
    pub role: String,
    pub state: SourceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Ok,
    Missing,
    ParseError,
}

impl SourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceState::Ok => "ok",
            SourceState::Missing => "missing",
            SourceState::ParseError => "parse_error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    pub kind: &'static str,
    pub ok: bool,
    pub path: String,
    pub artifact_hash: String,
    pub sources: Vec<SourceMeta>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub kind: &'static str,
    pub ok: bool,
    pub path: String,
    pub expected_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_hash: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn default_artifact_path() -> &'static str {
    DEFAULT_ARTIFACT_PATH
}

pub fn build(root: &Path) -> io::Result<ProcessArtifact> {
    let mut sources = Vec::new();
    let mut modules = BTreeMap::new();
    let mut diagnostics = Vec::new();

    let manifest_rel = discover::manifest_rel_path();
    let manifest_path = discover::manifest_path(root);
    let manifest_text = validate::read_optional(&manifest_path)?;

    let (project, profile, required_modules) = match manifest_text {
        Some(text) => {
            let hash = stable_hash(&text);
            let bytes = text.len() as u64;
            let (manifest, manifest_diagnostics) = config::parse_manifest(&text, &manifest_rel);
            diagnostics.extend(manifest_diagnostics);
            match config::parse_jsonc(&text, &manifest_rel, Code::ManifestParseError) {
                Ok(value) => {
                    sources.push(source(
                        manifest_rel.clone(),
                        "manifest",
                        SourceState::Ok,
                        Some(hash),
                        Some(bytes),
                    ));
                    modules.insert(
                        "manifest".to_owned(),
                        normalize(&value, &manifest_rel, &mut diagnostics),
                    );
                }
                Err(diagnostic) => {
                    sources.push(source(
                        manifest_rel.clone(),
                        "manifest",
                        SourceState::ParseError,
                        Some(hash),
                        Some(bytes),
                    ));
                    if !diagnostics.iter().any(|existing| existing == &diagnostic) {
                        diagnostics.push(diagnostic);
                    }
                }
            }
            manifest.map_or((None, None, Vec::new()), |manifest| {
                (
                    manifest.project_name,
                    manifest.profile,
                    manifest.required_modules,
                )
            })
        }
        None => {
            diagnostics.push(Diagnostic::error(
                Code::ManifestMissing,
                manifest_rel.clone(),
                format!("no {manifest_rel} found; run `nopal validate` for setup guidance"),
            ));
            sources.push(source(
                manifest_rel.clone(),
                "manifest",
                SourceState::Missing,
                None,
                None,
            ));
            (None, None, Vec::new())
        }
    };

    for module in Module::ALL {
        let rel = discover::module_rel_path(module);
        let role = module.as_str();
        match validate::read_optional(&discover::module_path(root, module))? {
            Some(text) => {
                let hash = stable_hash(&text);
                let bytes = text.len() as u64;
                match config::parse_jsonc(&text, &rel, Code::ModuleParseError) {
                    Ok(value) => {
                        diagnostics.extend(validate_module(module, &value, &rel));
                        modules.insert(role.to_owned(), normalize(&value, &rel, &mut diagnostics));
                        sources.push(source(rel, role, SourceState::Ok, Some(hash), Some(bytes)));
                    }
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        sources.push(source(
                            rel,
                            role,
                            SourceState::ParseError,
                            Some(hash),
                            Some(bytes),
                        ));
                    }
                }
            }
            None if required_modules.contains(&module) => {
                diagnostics.push(Diagnostic::error(
                    Code::ModuleMissing,
                    rel.clone(),
                    format!(
                        "profile {:?} requires {rel}",
                        profile.as_ref().map_or("?", Profile::as_str)
                    ),
                ));
                sources.push(source(rel, role, SourceState::Missing, None, None));
            }
            None => {
                sources.push(source(rel, role, SourceState::Missing, None, None));
            }
        }
    }

    diagnostics::sort(&mut diagnostics);
    Ok(ProcessArtifact {
        kind: PROCESS_ARTIFACT_KIND,
        project,
        profile,
        sources,
        modules,
        diagnostics,
    })
}

pub fn artifact_json(artifact: &ProcessArtifact) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(artifact).map(|mut text| {
        text.push('\n');
        text
    })
}

pub fn export_report(
    path: impl Into<String>,
    artifact: &ProcessArtifact,
    json: &str,
) -> ExportReport {
    ExportReport {
        kind: PROCESS_EXPORT_KIND,
        ok: artifact.ok(),
        path: path.into(),
        artifact_hash: stable_hash(json),
        sources: artifact.sources.clone(),
        diagnostics: artifact.diagnostics.clone(),
    }
}

pub fn check_report(
    path: impl Into<String>,
    artifact: &ProcessArtifact,
    expected_json: &str,
    actual_text: Option<&str>,
) -> CheckReport {
    let path = path.into();
    let expected_hash = stable_hash(expected_json);
    let mut diagnostics = artifact.diagnostics.clone();
    let mut actual_hash = None;
    let mut matches_expected = false;

    match actual_text {
        Some(text) => {
            actual_hash = Some(stable_hash(text));
            match serde_json::from_str::<serde_json::Value>(text) {
                Ok(actual) => match serde_json::to_value(artifact) {
                    Ok(expected) => {
                        matches_expected = actual == expected;
                        if !matches_expected {
                            diagnostics.push(Diagnostic::error(
                                Code::ProcessArtifactDrift,
                                path.clone(),
                                "process artifact is stale; rerun `nopal export process --output <path>`",
                            ));
                        }
                    }
                    Err(err) => diagnostics.push(Diagnostic::error(
                        Code::ProcessArtifactDrift,
                        path.clone(),
                        format!("could not serialize expected process artifact: {err}"),
                    )),
                },
                Err(err) => diagnostics.push(Diagnostic::error(
                    Code::ProcessArtifactParseError,
                    path.clone(),
                    format!("process artifact is not valid JSON: {err}"),
                )),
            }
        }
        None => diagnostics.push(Diagnostic::error(
            Code::ProcessArtifactMissing,
            path.clone(),
            "process artifact is missing; run `nopal export process --output <path>`",
        )),
    }

    diagnostics::sort(&mut diagnostics);
    CheckReport {
        kind: PROCESS_CHECK_KIND,
        ok: matches_expected
            && diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error),
        path,
        expected_hash,
        actual_hash,
        diagnostics,
    }
}

pub fn export_report_toon(report: &ExportReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        ("path".into(), Value::str(report.path.clone())),
        (
            "artifact_hash".into(),
            Value::str(report.artifact_hash.clone()),
        ),
        ("sources".into(), sources_table(&report.sources)),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

pub fn check_report_toon(report: &CheckReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        ("path".into(), Value::str(report.path.clone())),
        (
            "expected_hash".into(),
            Value::str(report.expected_hash.clone()),
        ),
        (
            "actual_hash".into(),
            Value::str(report.actual_hash.clone().unwrap_or_else(|| "-".to_owned())),
        ),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

fn validate_module(module: Module, value: &serde_json::Value, rel: &str) -> Vec<Diagnostic> {
    match module {
        Module::Gates => gates::validate_document(value, rel).1,
        Module::Policy => policy::validate_document(value, rel).1,
        Module::Workflow => workflow::validate_document(value, rel),
        Module::Roots => roots::validate_document(value, rel),
        Module::Guidance => guidance::validate_document(value, rel),
        Module::ReviewPolicy => review_policy::validate_document(value, rel).1,
    }
}

fn source(
    path: String,
    role: impl Into<String>,
    state: SourceState,
    hash: Option<String>,
    bytes: Option<u64>,
) -> SourceMeta {
    SourceMeta {
        path,
        role: role.into(),
        state,
        hash,
        bytes,
    }
}

fn sources_table(sources: &[SourceMeta]) -> Value {
    Value::Table {
        fields: vec![
            "path".into(),
            "role".into(),
            "state".into(),
            "hash".into(),
            "bytes".into(),
        ],
        rows: sources
            .iter()
            .map(|source| {
                vec![
                    Value::str(source.path.clone()),
                    Value::str(source.role.clone()),
                    Value::str(source.state.as_str()),
                    Value::str(source.hash.clone().unwrap_or_else(|| "-".to_owned())),
                    source
                        .bytes
                        .and_then(|bytes| i64::try_from(bytes).ok())
                        .map_or_else(|| Value::str("-"), Value::Int),
                ]
            })
            .collect(),
    }
}

fn normalize(
    value: &serde_json::Value,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> serde_json::Value {
    let mut redacted = 0usize;
    let normalized = normalize_inner(value, None, false, &mut redacted);
    if redacted > 0 {
        diagnostics.push(Diagnostic::warning(
            Code::ProcessArtifactRedacted,
            path,
            format!("process artifact redacted {redacted} secret-looking value(s)"),
        ));
    }
    normalized
}

fn normalize_inner(
    value: &serde_json::Value,
    key_hint: Option<&str>,
    force_redact: bool,
    redacted: &mut usize,
) -> serde_json::Value {
    let redact = force_redact || key_hint.is_some_and(secret_key);
    if redact {
        return match value {
            serde_json::Value::Object(obj) => {
                let mut out = serde_json::Map::new();
                let mut keys: Vec<&String> = obj.keys().collect();
                keys.sort();
                for key in keys {
                    if let Some(child) = obj.get(key) {
                        out.insert(
                            key.clone(),
                            normalize_inner(child, Some(key), true, redacted),
                        );
                    }
                }
                serde_json::Value::Object(out)
            }
            serde_json::Value::Array(items) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|item| normalize_inner(item, None, true, redacted))
                    .collect(),
            ),
            serde_json::Value::Null => serde_json::Value::Null,
            serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {
                *redacted += 1;
                serde_json::Value::String("<redacted>".to_owned())
            }
        };
    }

    match value {
        serde_json::Value::Object(obj) => {
            let mut out = serde_json::Map::new();
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(child) = obj.get(key) {
                    out.insert(
                        key.clone(),
                        normalize_inner(child, Some(key), false, redacted),
                    );
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| normalize_inner(item, None, false, redacted))
                .collect(),
        ),
        serde_json::Value::String(text) if secret_literal(text) => {
            *redacted += 1;
            serde_json::Value::String("<redacted>".to_owned())
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => value.clone(),
    }
}

fn secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', ' '], "_");
    normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("password")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("private_key")
        || normalized.contains("auth_header")
        || normalized.contains("bearer")
        || normalized.contains("credential")
}

fn secret_literal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("bearer ")
        || lower.contains("-----begin private key-----")
        || text.starts_with("sk-")
        || text.starts_with("ghp_")
        || text.starts_with("gho_")
        || text.starts_with("github_pat_")
}

fn stable_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_redacts_secret_keys_and_literals() {
        let value = serde_json::json!({
            "z": "last",
            "api_token": "plain-secret",
            "nested": { "auth_header": "Bearer abc", "a": "first" },
            "bearer": "plain credential",
            "literal": "github_pat_123"
        });

        let mut diagnostics = Vec::new();
        let normalized = normalize(&value, ".nopal/nopal.jsonc", &mut diagnostics);
        assert_eq!(normalized["api_token"], "<redacted>");
        assert_eq!(normalized["nested"]["auth_header"], "<redacted>");
        assert_eq!(normalized["bearer"], "<redacted>");
        assert_eq!(normalized["literal"], "<redacted>");
        assert_eq!(normalized["nested"]["a"], "first");
    }
}
