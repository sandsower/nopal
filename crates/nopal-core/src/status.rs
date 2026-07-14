//! Readiness status and output envelopes.
//!
//! The CLI renders exactly what this module builds - one envelope per
//! command, one builder per output flavor - so TOON and `--json` can never
//! drift apart.

use std::io;
use std::path::Path;

use serde::Serialize;

use crate::diagnostics::{self, Code, Diagnostic};
use crate::profile::Profile;
use crate::toon::{self, Value};
use crate::validate::{self, ModuleFileState, Validation};

pub const STATUS_KIND: &str = "nopal.status/v1";
pub const VALIDATION_KIND: &str = "nopal.validation/v1";

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub kind: &'static str,
    pub project: Option<String>,
    pub profile: Option<Profile>,
    pub ready: bool,
    pub modules: Vec<validate::ModuleState>,
    pub diagnostics: Vec<Diagnostic>,
    pub help: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub kind: &'static str,
    pub ok: bool,
    pub project: Option<String>,
    pub profile: Option<Profile>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn status(root: &Path) -> io::Result<Status> {
    let validation = validate::validate(root)?;
    let help = derive_help(&validation);
    Ok(Status {
        kind: STATUS_KIND,
        ready: validation.ok(),
        project: validation.project_name,
        profile: validation.profile,
        modules: validation.modules,
        diagnostics: validation.diagnostics,
        help,
    })
}

pub fn validation_report(root: &Path) -> io::Result<ValidationReport> {
    let validation = validate::validate(root)?;
    Ok(ValidationReport {
        kind: VALIDATION_KIND,
        ok: validation.ok(),
        project: validation.project_name,
        profile: validation.profile,
        diagnostics: validation.diagnostics,
    })
}

/// Deterministic next steps, most fundamental problem first.
fn derive_help(validation: &Validation) -> Vec<String> {
    let mut help = Vec::new();

    if has_code(validation, Code::ManifestMissing) {
        help.push(format!(
            "create .nopal/nopal.jsonc with \"version\": \"{}\" and a built-in or manifest-defined profile",
            crate::config::MANIFEST_KIND
        ));
        return help;
    }
    if has_code(validation, Code::ManifestParseError) {
        help.push("fix the JSONC syntax error in .nopal/nopal.jsonc".to_owned());
        return help;
    }
    if has_code(validation, Code::VersionUnsupported) {
        help.push(format!(
            "set \"version\": \"{}\" in .nopal/nopal.jsonc",
            crate::config::MANIFEST_KIND
        ));
    }
    if has_code(validation, Code::ProfileUnknown) {
        help.push("set \"profile\" to a built-in profile (minimal or portable) or declare profiles.<name>.required_modules in .nopal/nopal.jsonc".to_owned());
    }
    for module in &validation.modules {
        match module.state {
            ModuleFileState::Missing => help.push(format!(
                "create {}",
                crate::discover::module_rel_path(module.module)
            )),
            ModuleFileState::ParseError => help.push(format!(
                "fix the JSONC syntax error in {}",
                crate::discover::module_rel_path(module.module)
            )),
            ModuleFileState::Ok | ModuleFileState::Absent => {}
        }
    }
    if help.is_empty() {
        help.push("ready; run nopal validate in CI to keep it that way".to_owned());
    }
    help
}

fn has_code(validation: &Validation, code: Code) -> bool {
    validation.diagnostics.iter().any(|d| d.code == code)
}

pub fn status_toon(status: &Status) -> String {
    let mut doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(status.kind)),
        ("project".into(), opt_str(&status.project)),
        ("profile".into(), profile_value(status.profile.as_ref())),
        ("ready".into(), Value::Bool(status.ready)),
        ("modules".into(), modules_table(&status.modules)),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&status.diagnostics),
        ),
    ];
    doc.push((
        "help".into(),
        Value::Arr(status.help.iter().map(Value::str).collect()),
    ));
    toon::encode(&doc)
}

pub fn validation_toon(report: &ValidationReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        ("project".into(), opt_str(&report.project)),
        ("profile".into(), profile_value(report.profile.as_ref())),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

fn opt_str(value: &Option<String>) -> Value {
    match value {
        Some(s) => Value::str(s.clone()),
        None => Value::str("-"),
    }
}

fn profile_value(profile: Option<&Profile>) -> Value {
    Value::str(profile.map_or("-", Profile::as_str))
}

fn modules_table(modules: &[validate::ModuleState]) -> Value {
    Value::Table {
        fields: vec!["name".into(), "required".into(), "state".into()],
        rows: modules
            .iter()
            .map(|m| {
                vec![
                    Value::str(m.module.as_str()),
                    Value::Bool(m.required),
                    Value::str(m.state.as_str()),
                ]
            })
            .collect(),
    }
}
