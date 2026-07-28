//! Evidence-backed first-run gate planning.
//!
//! One immutable plan owns detection, generated gate bytes, provenance, and
//! explanations. Callers may render or persist the plan, but Core never runs
//! detected tools and never searches outside the repository and its declared
//! workspaces.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::diagnostics::{Code, Diagnostic};

pub const PLAN_KIND: &str = "nopal.gate-scaffold/v1";
pub const GENERATED_GATES_KIND: &str = "nopal.gates/v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    NeedsConfiguration,
    Blocked,
}

impl Readiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeedsConfiguration => "needs_configuration",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Generated,
    ExplicitNopal,
    ExplicitBeislid,
}

impl Authority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::ExplicitNopal => "explicit_nopal",
            Self::ExplicitBeislid => "explicit_beislid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Selected,
    Skipped,
    Superseded,
    Ambiguous,
    Blocked,
}

impl DecisionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Skipped => "skipped",
            Self::Superseded => "superseded",
            Self::Ambiguous => "ambiguous",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionDecision {
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    pub outcome: DecisionOutcome,
    pub reason_code: String,
    pub evidence: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSelection {
    pub id: String,
    pub scope: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedGate {
    pub id: String,
    pub stage: String,
    pub argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub parallel_safe: bool,
    pub mutates: bool,
    pub template_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldProvenance {
    pub version: String,
    pub readiness: Readiness,
    pub authority: Authority,
    pub templates: Vec<TemplateSelection>,
    pub generated_gate_ids: Vec<String>,
    pub decisions: Vec<DetectionDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateScaffoldPlan {
    pub kind: &'static str,
    pub ok: bool,
    pub readiness: Readiness,
    pub authority: Authority,
    pub templates: Vec<TemplateSelection>,
    pub gates: Vec<GeneratedGate>,
    pub decisions: Vec<DetectionDecision>,
    pub diagnostics: Vec<Diagnostic>,
}

impl GateScaffoldPlan {
    pub fn selected_template_ids(&self) -> Vec<&str> {
        self.templates
            .iter()
            .map(|template| template.id.as_str())
            .collect()
    }

    /// Render the exact checked-in gate document without another repository
    /// read. This prevents preview and publication from observing different
    /// ecosystem evidence.
    pub fn gates_json(&self) -> Result<String, serde_json::Error> {
        let gates = self
            .gates
            .iter()
            .map(|gate| {
                let mut value = serde_json::json!({
                    "id": gate.id,
                    "stage": gate.stage,
                    "argv": gate.argv,
                    "parallel_safe": gate.parallel_safe,
                    "mutates": gate.mutates,
                });
                if let Some(cwd) = &gate.cwd {
                    value["cwd"] = serde_json::Value::String(cwd.clone());
                }
                value
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "version": GENERATED_GATES_KIND,
            "scaffold": {
                "version": PLAN_KIND,
                "readiness": self.readiness.as_str(),
                "authority": self.authority.as_str(),
                "templates": self.templates,
                "generated_gate_ids": self.gates.iter().map(|gate| gate.id.as_str()).collect::<Vec<_>>(),
                "decisions": self.decisions,
            },
            "preflights": [],
            "gates": gates,
            "gate_sets": {},
            "selectors": [],
        });
        serde_json::to_string_pretty(&value).map(|mut text| {
            text.push('\n');
            text
        })
    }
}

/// Inspect only root evidence. Declared workspace expansion is layered onto
/// this same planner so callers never need ecosystem-specific branches.
pub fn inspect(root: &Path) -> io::Result<GateScaffoldPlan> {
    let mut plan = GateScaffoldPlan {
        kind: PLAN_KIND,
        ok: true,
        readiness: Readiness::NeedsConfiguration,
        authority: Authority::Generated,
        templates: Vec::new(),
        gates: Vec::new(),
        decisions: Vec::new(),
        diagnostics: Vec::new(),
    };
    select_baseline(&mut plan);

    match evidence_file(root, "Cargo.toml") {
        Ok(true) => select_rust(&mut plan),
        Ok(false) => plan.decisions.push(DetectionDecision {
            scope: ".".to_owned(),
            template_id: Some("rust.cargo/v1".to_owned()),
            outcome: DecisionOutcome::Skipped,
            reason_code: "evidence_absent".to_owned(),
            evidence: Vec::new(),
            message: "Cargo.toml was not present at the repository root".to_owned(),
        }),
        Err(diagnostic) => {
            plan.ok = false;
            plan.readiness = Readiness::Blocked;
            plan.diagnostics.push(diagnostic);
        }
    }

    if plan.templates.len() > 1 && plan.readiness != Readiness::Blocked {
        plan.readiness = Readiness::Ready;
    } else if plan.readiness != Readiness::Blocked {
        plan.decisions.push(DetectionDecision {
            scope: ".".to_owned(),
            template_id: None,
            outcome: DecisionOutcome::Blocked,
            reason_code: "no_validation_evidence".to_owned(),
            evidence: Vec::new(),
            message: "no evidence-backed validation template was selected; add explicit gates before launch".to_owned(),
        });
    }
    crate::diagnostics::sort(&mut plan.diagnostics);
    Ok(plan)
}

fn select_baseline(plan: &mut GateScaffoldPlan) {
    plan.templates.push(TemplateSelection {
        id: "baseline.git/v1".to_owned(),
        scope: ".".to_owned(),
        evidence: vec![".git".to_owned()],
    });
    plan.gates.push(GeneratedGate {
        id: "detected.root.baseline.diff-check".to_owned(),
        stage: "pre_pr".to_owned(),
        argv: vec!["git".to_owned(), "diff".to_owned(), "--check".to_owned()],
        cwd: None,
        parallel_safe: true,
        mutates: false,
        template_id: "baseline.git/v1".to_owned(),
    });
    plan.decisions.push(DetectionDecision {
        scope: ".".to_owned(),
        template_id: Some("baseline.git/v1".to_owned()),
        outcome: DecisionOutcome::Selected,
        reason_code: "git_baseline".to_owned(),
        evidence: vec![".git".to_owned()],
        message: "selected the repository diff integrity baseline".to_owned(),
    });
}

fn select_rust(plan: &mut GateScaffoldPlan) {
    let template_id = "rust.cargo/v1";
    plan.templates.push(TemplateSelection {
        id: template_id.to_owned(),
        scope: ".".to_owned(),
        evidence: vec!["Cargo.toml".to_owned()],
    });
    for (purpose, argv) in [
        ("cargo-fmt", &["cargo", "fmt", "--all", "--check"][..]),
        (
            "cargo-clippy",
            &[
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ][..],
        ),
        ("cargo-test", &["cargo", "test", "--workspace"][..]),
    ] {
        plan.gates.push(GeneratedGate {
            id: format!("detected.root.rust.{purpose}"),
            stage: "pre_pr".to_owned(),
            argv: argv.iter().map(|item| (*item).to_owned()).collect(),
            cwd: None,
            parallel_safe: false,
            mutates: false,
            template_id: template_id.to_owned(),
        });
    }
    plan.decisions.push(DetectionDecision {
        scope: ".".to_owned(),
        template_id: Some(template_id.to_owned()),
        outcome: DecisionOutcome::Selected,
        reason_code: "manifest_present".to_owned(),
        evidence: vec!["Cargo.toml".to_owned()],
        message: "selected Cargo formatting, lint, and test validation".to_owned(),
    });
}

fn evidence_file(root: &Path, relative: &str) -> Result<bool, Diagnostic> {
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(Diagnostic::error(
                Code::GateScaffoldEvidenceInvalid,
                relative,
                format!("could not inspect ecosystem evidence: {error}"),
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(Diagnostic::error(
            Code::GateScaffoldEvidenceInvalid,
            relative,
            "ecosystem evidence must be a regular non-symlink file",
        ));
    }
    Ok(true)
}
