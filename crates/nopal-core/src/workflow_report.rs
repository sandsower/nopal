//! Output envelope for `nopal workflow show` (WS-CORE).
//!
//! Same contract as every other nopal command: one envelope, one builder per
//! output flavor, kind `nopal.workflow.show/v1`. Defaults for `handoff` and
//! `babysit` are applied here and only here - the single defaulting
//! authority the design calls for - so the extension side never needs its
//! own fenced-block parsers or default tables.

use std::io;
use std::path::Path;

use serde::Serialize;

use crate::diagnostics::{Diagnostic, Severity};
use crate::discover;
use crate::profile::Module;
use crate::toon::{self, Value};
use crate::workflow;

pub const WORKFLOW_SHOW_KIND: &str = "nopal.workflow.show/v1";

/// Today's `DEFAULT_AUTO_HANDOFF_EXCLUDED_EVENTS` from the beislid
/// extension: the three planning boundaries auto-handoff must not cross
/// unless config explicitly says `"exclude": []`.
pub const DEFAULT_AUTO_HANDOFF_EXCLUDED_EVENTS: &[&str] =
    &["break_spec_approved", "spec_approved", "blueprint_approved"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffConfig {
    pub auto: bool,
    /// Empty means all events are eligible.
    pub events: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for HandoffConfig {
    fn default() -> Self {
        HandoffConfig {
            auto: false,
            events: Vec::new(),
            exclude: default_exclude(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct BabysitConfig {
    pub token_budget: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct EstablishmentConfig {
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowShowReport {
    pub kind: &'static str,
    pub ok: bool,
    pub handoff: HandoffConfig,
    pub babysit: BabysitConfig,
    pub establishment: EstablishmentConfig,
    pub diagnostics: Vec<Diagnostic>,
}

fn default_exclude() -> Vec<String> {
    DEFAULT_AUTO_HANDOFF_EXCLUDED_EVENTS
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Load `.nopal/workflow.jsonc`, validate it, and apply handoff/babysit
/// defaults. A missing file is not an error: it emits defaults with `ok:
/// true`. A config with error-severity diagnostics also renders defaults
/// (nopal does not report half-understood config), same idiom as
/// `gates_report::usable`.
pub fn workflow_show(root: &Path) -> io::Result<WorkflowShowReport> {
    let rel = discover::module_rel_path(Module::Workflow);
    match crate::validate::read_optional(&discover::module_path(root, Module::Workflow))? {
        None => Ok(WorkflowShowReport {
            kind: WORKFLOW_SHOW_KIND,
            ok: true,
            handoff: HandoffConfig::default(),
            babysit: BabysitConfig::default(),
            establishment: EstablishmentConfig::default(),
            diagnostics: Vec::new(),
        }),
        Some(text) => {
            let (value, diagnostics) = workflow::parse_workflow(&text, &rel);
            let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);
            let usable = if has_errors { None } else { value.as_ref() };
            let (handoff, babysit, establishment) = build_config(usable);
            Ok(WorkflowShowReport {
                kind: WORKFLOW_SHOW_KIND,
                ok: !has_errors,
                handoff,
                babysit,
                establishment,
                diagnostics,
            })
        }
    }
}

fn build_config(
    root: Option<&serde_json::Value>,
) -> (HandoffConfig, BabysitConfig, EstablishmentConfig) {
    let handoff = root
        .and_then(|r| r.get("handoff"))
        .and_then(serde_json::Value::as_object)
        .map(|obj| HandoffConfig {
            auto: obj
                .get("auto")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            events: string_array(obj.get("events")),
            exclude: match obj.get("exclude") {
                Some(exclude) => string_array(Some(exclude)),
                None => default_exclude(),
            },
        })
        .unwrap_or_default();

    let babysit = root
        .and_then(|r| r.get("babysit"))
        .and_then(serde_json::Value::as_object)
        .map(|obj| BabysitConfig {
            token_budget: obj.get("token_budget").and_then(serde_json::Value::as_u64),
        })
        .unwrap_or_default();

    let establishment = EstablishmentConfig {
        events: root
            .map(workflow::establishment_events)
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };

    (handoff, babysit, establishment)
}

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn workflow_show_toon(report: &WorkflowShowReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        (
            "handoff".into(),
            Value::Obj(vec![
                ("auto".into(), Value::Bool(report.handoff.auto)),
                (
                    "events".into(),
                    Value::Arr(report.handoff.events.iter().map(Value::str).collect()),
                ),
                (
                    "exclude".into(),
                    Value::Arr(report.handoff.exclude.iter().map(Value::str).collect()),
                ),
            ]),
        ),
        (
            "babysit".into(),
            Value::Obj(vec![(
                "token_budget".into(),
                token_budget_cell(report.babysit.token_budget),
            )]),
        ),
        (
            "establishment".into(),
            Value::Obj(vec![(
                "events".into(),
                Value::Arr(report.establishment.events.iter().map(Value::str).collect()),
            )]),
        ),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

fn token_budget_cell(value: Option<u64>) -> Value {
    match value {
        Some(budget) => Value::Int(i64::try_from(budget).unwrap_or(i64::MAX)),
        None => Value::str("-"),
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

    #[test]
    fn missing_workflow_module_is_ok_with_defaults() {
        let report = workflow_show(&example("minimal")).unwrap();
        assert!(report.ok);
        assert_eq!(report.handoff, HandoffConfig::default());
        assert_eq!(report.babysit, BabysitConfig::default());
        assert_eq!(report.establishment, EstablishmentConfig::default());
        assert_eq!(report.diagnostics, vec![]);
    }

    #[test]
    fn present_workflow_module_without_handoff_or_babysit_falls_back_to_defaults() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".nopal")).unwrap();
        std::fs::write(
            temp.path().join(".nopal/workflow.jsonc"),
            r#"{ "version": "nopal.workflow/v1" }"#,
        )
        .unwrap();
        let report = workflow_show(temp.path()).unwrap();
        assert!(report.ok);
        assert_eq!(report.handoff, HandoffConfig::default());
        assert_eq!(report.babysit.token_budget, None);
    }

    #[test]
    fn explicit_config_overrides_defaults() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".nopal")).unwrap();
        std::fs::write(
            temp.path().join(".nopal/workflow.jsonc"),
            r#"{
                "version": "nopal.workflow/v1",
                "handoff": { "auto": true, "events": ["kickoff_context_ready"], "exclude": [] },
                "babysit": { "token_budget": 400000 },
                "establishment": { "events": ["kickoff_context_ready"] }
            }"#,
        )
        .unwrap();
        let report = workflow_show(temp.path()).unwrap();
        assert!(report.ok);
        assert!(report.handoff.auto);
        assert_eq!(report.handoff.events, vec!["kickoff_context_ready"]);
        assert_eq!(report.handoff.exclude, Vec::<String>::new());
        assert_eq!(report.babysit.token_budget, Some(400000));
        assert_eq!(report.establishment.events, vec!["kickoff_context_ready"]);
    }

    #[test]
    fn invalid_workflow_module_reports_diagnostics_and_defaults() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".nopal")).unwrap();
        std::fs::write(
            temp.path().join(".nopal/workflow.jsonc"),
            r#"{ "version": "nopal.workflow/v1", "handoff": { "auto": "nope" } }"#,
        )
        .unwrap();
        let report = workflow_show(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(report.handoff, HandoffConfig::default());
        assert_eq!(
            report.diagnostics[0].code,
            crate::diagnostics::Code::FieldInvalid
        );
    }

    #[test]
    fn toon_and_json_come_from_the_same_report() {
        let report = workflow_show(&example("minimal")).unwrap();
        let toon = workflow_show_toon(&report);
        let json = serde_json::to_value(&report).unwrap();
        assert!(toon.contains("kind: nopal.workflow.show/v1"));
        assert_eq!(json["kind"], "nopal.workflow.show/v1");
        assert_eq!(json["babysit"]["token_budget"], serde_json::Value::Null);
    }
}
