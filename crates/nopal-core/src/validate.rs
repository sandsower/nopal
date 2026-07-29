//! Profile/module validation over a project root.
//!
//! `validate` is the deterministic heart of phase 1: filesystem paths in,
//! typed results and ordered diagnostics out. `Err` is reserved for genuine
//! IO failures (permissions, not UTF-8, ...); "file not found" is domain
//! knowledge and comes back as a diagnostic instead.

use std::io;
use std::path::Path;

use serde::Serialize;

use crate::config::{self, Manifest};
use crate::diagnostics::{self, Code, Diagnostic};
use crate::discover;
use crate::gates;
use crate::guidance;
use crate::policy;
use crate::profile::{Module, Profile};
use crate::review_policy;
use crate::roots;
use crate::workflow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleFileState {
    /// Present and parses as JSONC.
    Ok,
    /// Required by the profile but not present.
    Missing,
    /// Not required and not present.
    Absent,
    /// Present but does not parse.
    ParseError,
}

impl ModuleFileState {
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleFileState::Ok => "ok",
            ModuleFileState::Missing => "missing",
            ModuleFileState::Absent => "absent",
            ModuleFileState::ParseError => "parse_error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleState {
    pub module: Module,
    pub required: bool,
    pub state: ModuleFileState,
}

#[derive(Debug, Clone, Serialize)]
pub struct Validation {
    pub project_name: Option<String>,
    pub profile: Option<Profile>,
    pub modules: Vec<ModuleState>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Validation {
    /// Ready means: nothing at error severity.
    pub fn ok(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|d| d.severity != diagnostics::Severity::Error)
    }
}

pub fn validate(root: &Path) -> io::Result<Validation> {
    let manifest_path = discover::manifest_path(root);
    let manifest_rel = discover::manifest_rel_path();

    let text = match read_optional(&manifest_path)? {
        Some(text) => text,
        None => {
            return Ok(Validation {
                project_name: None,
                profile: None,
                modules: Vec::new(),
                diagnostics: vec![Diagnostic::error(
                    Code::ManifestMissing,
                    manifest_rel.clone(),
                    format!("no {manifest_rel} found; run `nopal validate` for setup guidance"),
                )],
            });
        }
    };

    let (manifest, mut all_diagnostics) = config::parse_manifest(&text, &manifest_rel);
    let (project_name, profile, required_modules) = match manifest {
        Some(Manifest {
            project_name,
            profile,
            required_modules,
        }) => (project_name, profile, required_modules),
        None => (None, None, Vec::new()),
    };

    let mut modules = Vec::new();
    for module in Module::ALL {
        let required = required_modules.contains(&module);
        let rel = discover::module_rel_path(module);
        let state = match read_optional(&discover::module_path(root, module))? {
            Some(text) => match config::parse_jsonc(&text, &rel, Code::ModuleParseError) {
                Ok(value) => {
                    // Deep per-module schemas. Schema problems keep state
                    // `ok` (the file parses) and surface as diagnostics,
                    // which is what flips readiness.
                    match module {
                        Module::Gates => {
                            let (_, gates_diagnostics) = gates::validate_document(&value, &rel);
                            all_diagnostics.extend(gates_diagnostics);
                        }
                        Module::Policy => {
                            let (_, policy_diagnostics) = policy::validate_document(&value, &rel);
                            all_diagnostics.extend(policy_diagnostics);
                        }
                        Module::Workflow => {
                            all_diagnostics.extend(workflow::validate_document(&value, &rel));
                        }
                        Module::Roots => {
                            all_diagnostics.extend(roots::validate_document(&value, &rel));
                        }
                        Module::Guidance => {
                            all_diagnostics.extend(guidance::validate_document(&value, &rel));
                        }
                        Module::ReviewPolicy => {
                            let (_, review_policy_diagnostics) =
                                review_policy::validate_document(&value, &rel);
                            all_diagnostics.extend(review_policy_diagnostics);
                        }
                    }
                    ModuleFileState::Ok
                }
                Err(diagnostic) => {
                    all_diagnostics.push(diagnostic);
                    ModuleFileState::ParseError
                }
            },
            None if required => {
                all_diagnostics.push(Diagnostic::error(
                    Code::ModuleMissing,
                    rel.clone(),
                    format!(
                        "profile {:?} requires {rel}",
                        profile.as_ref().map_or("?", Profile::as_str)
                    ),
                ));
                ModuleFileState::Missing
            }
            None => ModuleFileState::Absent,
        };
        modules.push(ModuleState {
            module,
            required,
            state,
        });
    }

    for (name, file_name) in [
        ("field", "field.jsonc"),
        ("plot", "plot.jsonc"),
        ("session", "session.jsonc"),
        ("rondo", "rondo.jsonc"),
        ("memento", "memento.jsonc"),
        ("herdr", "herdr.jsonc"),
        ("integrations", "integrations.jsonc"),
    ] {
        let path = root.join(".nopal").join(file_name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => all_diagnostics.push(Diagnostic::error(
                Code::ProductSurfaceRemoved,
                format!(".nopal/{file_name}"),
                format!(
                    "removed v0.2 module file is not active configuration in v0.3; {}",
                    config::removed_module_migration(name)
                        .unwrap_or("remove this obsolete module file")
                ),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    diagnostics::sort(&mut all_diagnostics);

    Ok(Validation {
        project_name,
        profile,
        modules,
        diagnostics: all_diagnostics,
    })
}

/// Read a file, mapping NotFound to `None` and every other failure to `Err`.
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn example(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples")
            .join(name)
    }

    fn codes(validation: &Validation) -> Vec<Code> {
        validation.diagnostics.iter().map(|d| d.code).collect()
    }

    fn module_state(validation: &Validation, module: Module) -> ModuleFileState {
        validation
            .modules
            .iter()
            .find(|m| m.module == module)
            .map(|m| m.state)
            .unwrap()
    }

    #[test]
    fn minimal_example_is_ok_with_no_required_modules() {
        let v = validate(&example("minimal")).unwrap();
        assert!(v.ok(), "diagnostics: {:?}", v.diagnostics);
        assert_eq!(v.project_name.as_deref(), Some("minimal-example"));
        assert_eq!(v.profile.as_ref().map(Profile::as_str), Some("minimal"));
        assert!(v.modules.iter().all(|m| !m.required));
        assert!(v.modules.iter().all(|m| m.state == ModuleFileState::Absent));
    }

    #[test]
    fn portable_example_is_ok_with_gates_and_policy_required() {
        let v = validate(&example("portable")).unwrap();
        assert!(v.ok(), "diagnostics: {:?}", v.diagnostics);
        assert_eq!(module_state(&v, Module::Gates), ModuleFileState::Ok);
        assert_eq!(module_state(&v, Module::Policy), ModuleFileState::Ok);
        assert_eq!(module_state(&v, Module::Workflow), ModuleFileState::Absent);
    }

    #[test]
    fn nopal_example_requires_and_finds_all_modules_except_review_policy() {
        let v = validate(&example("nopal")).unwrap();
        assert!(v.ok(), "diagnostics: {:?}", v.diagnostics);
        for module in &v.modules {
            if module.module == Module::ReviewPolicy {
                assert!(!module.required, "review_policy is optional everywhere");
            } else {
                assert!(module.required, "{:?}", module.module);
                assert_eq!(module.state, ModuleFileState::Ok, "{:?}", module.module);
            }
        }
    }

    #[test]
    fn missing_required_module_is_an_error() {
        let v = validate(&example("portable-missing-policy")).unwrap();
        assert!(!v.ok());
        assert_eq!(codes(&v), vec![Code::ModuleMissing]);
        assert_eq!(module_state(&v, Module::Policy), ModuleFileState::Missing);
        assert_eq!(
            v.diagnostics[0].path, ".nopal/policy.jsonc",
            "diagnostic points at the missing file"
        );
    }

    #[test]
    fn broken_module_is_an_error_with_position() {
        let v = validate(&example("portable-broken-gates")).unwrap();
        assert!(!v.ok());
        assert_eq!(codes(&v), vec![Code::ModuleParseError]);
        assert_eq!(module_state(&v, Module::Gates), ModuleFileState::ParseError);
        assert!(v.diagnostics[0].position.is_some());
    }

    #[test]
    fn schema_invalid_gates_flip_readiness_but_module_state_stays_ok() {
        let v = validate(&example("gates-invalid")).unwrap();
        assert!(!v.ok());
        assert_eq!(
            module_state(&v, Module::Gates),
            ModuleFileState::Ok,
            "the file parses; schema problems are diagnostics, not state"
        );
        let codes = codes(&v);
        for expected in [
            Code::VersionUnsupported,
            Code::StageUnknown,
            Code::DuplicateId,
            Code::GateRefUnknown,
            Code::GateSetUnknown,
        ] {
            assert!(codes.contains(&expected), "missing {expected:?}: {codes:?}");
        }
    }

    #[test]
    fn unsupported_version_is_an_error_but_modules_still_checked() {
        let v = validate(&example("bad-version")).unwrap();
        assert!(!v.ok());
        assert_eq!(codes(&v), vec![Code::VersionUnsupported]);
        assert_eq!(v.profile.as_ref().map(Profile::as_str), Some("minimal"));
        assert_eq!(v.modules.len(), Module::ALL.len());
    }

    #[test]
    fn broken_manifest_is_a_parse_error_with_position() {
        let v = validate(&example("bad-jsonc")).unwrap();
        assert!(!v.ok());
        assert_eq!(codes(&v), vec![Code::ManifestParseError]);
        assert!(v.diagnostics[0].position.is_some());
    }

    #[test]
    fn absent_project_reports_manifest_missing() {
        let v = validate(&example("no-such-example")).unwrap();
        assert!(!v.ok());
        assert_eq!(codes(&v), vec![Code::ManifestMissing]);
        assert!(v.modules.is_empty());
    }

    #[test]
    fn removed_module_files_fail_with_actionable_migration_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".nopal")).unwrap();
        std::fs::write(
            temp.path().join(".nopal/nopal.jsonc"),
            r#"{"version":"nopal.project/v1","profile":"minimal"}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join(".nopal/field.jsonc"), "{}").unwrap();
        std::fs::write(temp.path().join(".nopal/integrations.jsonc"), "{}").unwrap();

        let validation = validate(temp.path()).unwrap();
        assert!(!validation.ok());
        assert_eq!(
            codes(&validation),
            vec![Code::ProductSurfaceRemoved, Code::ProductSurfaceRemoved]
        );
        assert!(validation.diagnostics[0].message.contains("bare `nopal`"));
        assert!(
            validation.diagnostics[1]
                .message
                .contains(".beislid/workflow.md")
        );
    }

    #[test]
    fn invalid_workflow_module_reports_lifecycle_schema_errors() {
        let v = validate(&example("workflow-invalid")).unwrap();
        assert!(!v.ok());
        let codes = codes(&v);
        assert!(codes.contains(&Code::WorkflowEventUnknown), "{codes:?}");
        assert!(
            codes.contains(&Code::WorkflowActionTypeUnknown),
            "{codes:?}"
        );
        assert!(codes.contains(&Code::DuplicateId), "{codes:?}");
        assert!(codes.contains(&Code::FieldInvalid), "{codes:?}");
        assert_eq!(module_state(&v, Module::Workflow), ModuleFileState::Ok);
    }

    #[test]
    fn invalid_guidance_module_cannot_define_authoritative_surfaces() {
        let v = validate(&example("guidance-invalid")).unwrap();
        assert!(!v.ok());
        let codes = codes(&v);
        assert!(codes.contains(&Code::GuidanceAuthorityInvalid), "{codes:?}");
        assert_eq!(module_state(&v, Module::Guidance), ModuleFileState::Ok);
    }

    #[test]
    fn diagnostics_are_ordered_by_path_then_position_then_code() {
        let mut diagnostics = vec![
            Diagnostic::error(Code::ModuleMissing, "b.jsonc", "x"),
            Diagnostic::error(Code::ModuleParseError, "a.jsonc", "x").with_position(2, 1),
            Diagnostic::error(Code::ManifestParseError, "a.jsonc", "x").with_position(1, 5),
        ];
        crate::diagnostics::sort(&mut diagnostics);
        let order: Vec<(&str, Code)> = diagnostics
            .iter()
            .map(|d| (d.path.as_str(), d.code))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a.jsonc", Code::ManifestParseError),
                ("a.jsonc", Code::ModuleParseError),
                ("b.jsonc", Code::ModuleMissing),
            ]
        );
    }
}
