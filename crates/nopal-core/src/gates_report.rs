//! Output envelopes for the gates commands.
//!
//! Same contract as `status`: the CLI renders exactly what these builders
//! produce - one envelope per command, one builder per output flavor - so
//! TOON and `--json` can never drift apart. A config with error-severity
//! diagnostics yields an empty listing/selection: nopal does not report
//! half-understood gates.

use std::io;
use std::path::Path;

use serde::Serialize;

use crate::diagnostics::{self, Code, Diagnostic, Severity};
use crate::discover;
use crate::gates::{self, GateStage, GatesConfig};
use crate::profile::Module;
use crate::selection::{self, SelectedGate, Selection, SelectorMatch, SkippedGate};
use crate::toon::{self, Value};

pub const PREFLIGHTS_LIST_KIND: &str = "nopal.preflights.list/v1";
pub const GATES_LIST_KIND: &str = "nopal.gates.list/v1";
pub const GATES_SELECT_KIND: &str = "nopal.gates.select/v1";

#[derive(Debug, Clone, Serialize)]
pub struct PreflightsListReport {
    pub kind: &'static str,
    pub ok: bool,
    pub preflights: Vec<gates::Preflight>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateSetEntry {
    pub name: String,
    pub gates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatesListReport {
    pub kind: &'static str,
    pub ok: bool,
    pub gates: Vec<gates::Gate>,
    pub gate_sets: Vec<GateSetEntry>,
    pub selectors: Vec<gates::Selector>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatesSelectReport {
    pub kind: &'static str,
    pub ok: bool,
    pub stage: GateStage,
    pub changed_files: Vec<String>,
    pub selectors: Vec<SelectorMatch>,
    pub selected: Vec<SelectedGate>,
    pub skipped: Vec<SkippedGate>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Load and validate `.nopal/gates.jsonc`. A missing file is a
/// `module_missing` diagnostic: the gates commands are explicit asks for
/// gates config, unlike profile validation where absence can be fine.
fn load(root: &Path) -> io::Result<(Option<GatesConfig>, Vec<Diagnostic>)> {
    let rel = discover::module_rel_path(Module::Gates);
    match crate::validate::read_optional(&discover::module_path(root, Module::Gates))? {
        Some(text) => Ok(gates::parse_gates(&text, &rel)),
        None => Ok((
            None,
            vec![Diagnostic::error(
                crate::diagnostics::Code::ModuleMissing,
                rel.clone(),
                format!("no {rel} found; the gates commands need one"),
            )],
        )),
    }
}

fn usable(config: Option<GatesConfig>, diagnostics: &[Diagnostic]) -> Option<GatesConfig> {
    let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);
    if has_errors { None } else { config }
}

pub fn preflights_list(root: &Path) -> io::Result<PreflightsListReport> {
    let (config, diagnostics) = load(root)?;
    let preflights = usable(config, &diagnostics).map_or(Vec::new(), |c| c.preflights);
    Ok(PreflightsListReport {
        kind: PREFLIGHTS_LIST_KIND,
        ok: diagnostics.iter().all(|d| d.severity != Severity::Error),
        preflights,
        diagnostics,
    })
}

pub fn gates_list(root: &Path) -> io::Result<GatesListReport> {
    let (config, diagnostics) = load(root)?;
    let config = usable(config, &diagnostics);
    let (gates, gate_sets, selectors) = config.map_or((Vec::new(), Vec::new(), Vec::new()), |c| {
        (
            c.gates,
            c.gate_sets
                .into_iter()
                .map(|(name, set)| GateSetEntry {
                    name,
                    gates: set.gates,
                })
                .collect(),
            c.selectors,
        )
    });
    Ok(GatesListReport {
        kind: GATES_LIST_KIND,
        ok: diagnostics.iter().all(|d| d.severity != Severity::Error),
        gates,
        gate_sets,
        selectors,
        diagnostics,
    })
}

pub fn gates_select(
    root: &Path,
    stage: GateStage,
    changed_files: &[String],
) -> io::Result<GatesSelectReport> {
    let (config, mut diagnostics) = load(root)?;
    if let GateStage::Unknown(stage_text) = &stage {
        diagnostics.push(Diagnostic::warning(
            Code::StageUnknown,
            "<cli>",
            format!(
                "requested gate stage {stage_text:?} is unknown; selection will conservatively return no stage matches"
            ),
        ));
    }
    let selection = usable(config, &diagnostics)
        .map(|c| selection::select(&c, stage.clone(), changed_files))
        .unwrap_or_else(|| empty_selection(stage, changed_files));
    Ok(GatesSelectReport {
        kind: GATES_SELECT_KIND,
        ok: diagnostics.iter().all(|d| d.severity != Severity::Error),
        stage: selection.stage,
        changed_files: selection.changed_files,
        selectors: selection.selectors,
        selected: selection.selected,
        skipped: selection.skipped,
        diagnostics,
    })
}

fn empty_selection(stage: GateStage, changed_files: &[String]) -> Selection {
    let mut files = changed_files.to_vec();
    files.sort();
    files.dedup();
    Selection {
        stage,
        changed_files: files,
        selectors: Vec::new(),
        selected: Vec::new(),
        skipped: Vec::new(),
    }
}

pub fn preflights_list_toon(report: &PreflightsListReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        (
            "preflights".into(),
            Value::Table {
                fields: str_fields(&["id", "stage", "run", "cwd"]),
                rows: report
                    .preflights
                    .iter()
                    .map(|p| {
                        vec![
                            Value::str(p.id.clone()),
                            Value::str(p.stage.as_str()),
                            Value::str(p.run.display()),
                            opt_cell(&p.cwd),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

pub fn gates_list_toon(report: &GatesListReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        (
            "gates".into(),
            Value::Table {
                fields: str_fields(&["id", "stage", "run", "cwd", "autofix"]),
                rows: report
                    .gates
                    .iter()
                    .map(|g| {
                        vec![
                            Value::str(g.id.clone()),
                            Value::str(g.stage.as_str()),
                            Value::str(g.run.display()),
                            opt_cell(&g.cwd),
                            opt_cell(&g.autofix),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "gate_sets".into(),
            Value::Table {
                fields: str_fields(&["name", "gates"]),
                rows: report
                    .gate_sets
                    .iter()
                    .map(|s| vec![Value::str(s.name.clone()), joined_cell(&s.gates)])
                    .collect(),
            },
        ),
        (
            "selectors".into(),
            Value::Table {
                fields: str_fields(&["name", "paths", "exclude", "gate_sets"]),
                rows: report
                    .selectors
                    .iter()
                    .map(|s| {
                        vec![
                            Value::str(s.name.clone()),
                            joined_cell(&s.paths),
                            joined_cell(&s.exclude),
                            joined_cell(&s.gate_sets),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

pub fn gates_select_toon(report: &GatesSelectReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        ("stage".into(), Value::str(report.stage.as_str())),
        (
            "changed_files".into(),
            Value::Arr(report.changed_files.iter().map(Value::str).collect()),
        ),
        (
            "selectors".into(),
            Value::Table {
                fields: str_fields(&["name", "matched", "matched_files"]),
                rows: report
                    .selectors
                    .iter()
                    .map(|s| {
                        vec![
                            Value::str(s.name.clone()),
                            Value::Bool(s.matched),
                            joined_cell(&s.matched_files),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "selected".into(),
            Value::Table {
                fields: str_fields(&["id", "stage", "run", "parallel_safe", "mutates", "via"]),
                rows: report
                    .selected
                    .iter()
                    .map(|g| {
                        vec![
                            Value::str(g.id.clone()),
                            Value::str(g.stage.as_str()),
                            Value::str(g.run.display()),
                            opt_bool_cell(g.parallel_safe),
                            opt_bool_cell(g.mutates),
                            Value::str(g.via.display()),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "skipped".into(),
            Value::Table {
                fields: str_fields(&["id", "stage", "reason", "via"]),
                rows: report
                    .skipped
                    .iter()
                    .map(|g| {
                        vec![
                            Value::str(g.id.clone()),
                            Value::str(g.stage.as_str()),
                            Value::str(g.reason.as_str()),
                            Value::str(g.via.as_ref().map_or("-".to_owned(), |v| v.display())),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

fn str_fields(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn opt_cell(value: &Option<String>) -> Value {
    Value::str(value.clone().unwrap_or_else(|| "-".to_owned()))
}

fn opt_bool_cell(value: Option<bool>) -> Value {
    value.map_or_else(|| Value::str("-"), Value::Bool)
}

/// Empty lists render as `-` so table rows keep a stable cell count.
fn joined_cell(items: &[String]) -> Value {
    if items.is_empty() {
        Value::str("-")
    } else {
        Value::str(items.join(" "))
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
    fn missing_gates_file_reports_module_missing_everywhere() {
        let report = gates_list(&example("minimal")).unwrap();
        assert!(!report.ok);
        assert_eq!(report.gates.len(), 0);
        assert_eq!(
            report.diagnostics[0].code,
            crate::diagnostics::Code::ModuleMissing
        );

        let preflights = preflights_list(&example("minimal")).unwrap();
        assert!(!preflights.ok);

        let selection = gates_select(&example("minimal"), GateStage::PrePr, &[]).unwrap();
        assert!(!selection.ok);
        assert_eq!(selection.selected.len(), 0);
    }

    #[test]
    fn broken_gates_config_selects_nothing() {
        let report = gates_select(
            &example("portable-broken-gates"),
            GateStage::PrePr,
            &["x.rs".to_owned()],
        )
        .unwrap();
        assert!(!report.ok);
        assert_eq!(report.selected.len(), 0);
        assert_eq!(report.changed_files, vec!["x.rs".to_owned()]);
    }

    #[test]
    fn gate_metadata_renders_when_present_and_dash_when_absent() {
        let (config, diagnostics) = gates::parse_gates(
            r#"{
                "version": "nopal.gates/v1",
                "gates": [
                    { "id": "fmt", "stage": "pre_pr", "command": "x", "parallel_safe": true, "mutates": false },
                    { "id": "clippy", "stage": "pre_pr", "command": "y" }
                ]
            }"#,
            ".nopal/gates.jsonc",
        );
        assert_eq!(diagnostics, vec![]);
        let selection = selection::select(&config.unwrap(), GateStage::PrePr, &[]);
        let report = GatesSelectReport {
            kind: GATES_SELECT_KIND,
            ok: true,
            stage: selection.stage,
            changed_files: selection.changed_files,
            selectors: selection.selectors,
            selected: selection.selected,
            skipped: selection.skipped,
            diagnostics: Vec::new(),
        };
        let toon = gates_select_toon(&report);
        assert!(toon.contains("fmt,pre_pr,x,true,false,default"), "{toon}");
        assert!(toon.contains("clippy,pre_pr,y,-,-,default"), "{toon}");
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["selected"][0]["parallel_safe"], true);
        assert_eq!(json["selected"][0]["mutates"], false);
        assert!(json["selected"][1].get("parallel_safe").is_none());
    }

    #[test]
    fn toon_and_json_come_from_the_same_report() {
        let report = gates_list(&example("portable")).unwrap();
        let toon = gates_list_toon(&report);
        let json = serde_json::to_value(&report).unwrap();
        assert!(toon.contains("kind: nopal.gates.list/v1"));
        assert_eq!(json["kind"], "nopal.gates.list/v1");
        assert_eq!(json["ok"], report.ok);
    }
}
