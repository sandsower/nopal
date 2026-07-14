//! `nopal.review_policy/v1` module parsing, validation, and the review-risk
//! verdict core.
//!
//! Nopal decides and explains; it never gates. Given changed files, diff
//! stats, and a few facts the CLI cannot derive, the seam returns three
//! verdicts as one versioned envelope: risk classification, fast-path
//! eligibility, and split-policy violation. Parsing is diagnostic-
//! accumulating like gates.rs: one pass reports every problem it can see,
//! and every section is optional (conservative defaults apply).

use std::io;
use std::path::Path;

use globset::GlobSet;
use serde::Serialize;

use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Severity};
use crate::discover;
use crate::gates::{GateStage, GatesConfig};
use crate::profile::Module;
use crate::selection;
use crate::toon::{self, Value};
use crate::validate;

pub const REVIEW_POLICY_KIND: &str = "nopal.review_policy/v1";

/// Default `fast_path.max_changed_lines` when the section or field is absent.
pub const DEFAULT_MAX_CHANGED_LINES: u64 = 100;

/// Closed ordered lattice: `low < medium < high`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Low,
    Medium,
    High,
}

impl RiskClass {
    pub fn parse(s: &str) -> Option<RiskClass> {
        match s {
            "low" => Some(RiskClass::Low),
            "medium" => Some(RiskClass::Medium),
            "high" => Some(RiskClass::High),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RiskClass::Low => "low",
            RiskClass::Medium => "medium",
            RiskClass::High => "high",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskConfig {
    pub max_auto_closeout_risk: RiskClass,
    pub high_risk_paths: Vec<String>,
    pub low_risk_paths: Vec<String>,
    pub high_risk_file_count: Option<u64>,
    pub high_risk_total_changes: Option<u64>,
    pub low_risk_file_count: Option<u64>,
    pub low_risk_total_changes: Option<u64>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        RiskConfig {
            max_auto_closeout_risk: RiskClass::Low,
            high_risk_paths: Vec::new(),
            low_risk_paths: Vec::new(),
            high_risk_file_count: None,
            high_risk_total_changes: None,
            low_risk_file_count: None,
            low_risk_total_changes: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FastPathConfig {
    pub max_changed_lines: u64,
}

impl Default for FastPathConfig {
    fn default() -> Self {
        FastPathConfig {
            max_changed_lines: DEFAULT_MAX_CHANGED_LINES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ReviewPolicyConfig {
    pub risk: RiskConfig,
    pub fast_path: FastPathConfig,
    /// Open vocabulary token; only `"exclusive"` carries semantics today.
    pub split_policy: Option<String>,
}

// ---------------------------------------------------------------------------
// Verdict core (pure)
// ---------------------------------------------------------------------------

/// External facts nopal cannot derive from config + changed files.
#[derive(Debug, Clone, Copy)]
pub struct FastPathFacts {
    pub base_fresh: bool,
    pub needs_merge: bool,
    pub existing_pr: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewRiskRequest<'a> {
    pub changed_files: &'a [String],
    pub total_changes: Option<u64>,
    pub facts: FastPathFacts,
    pub stage: GateStage,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskVerdict {
    pub class: RiskClass,
    pub max_auto_closeout_risk: RiskClass,
    pub agentic_reviewer_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FastPathVerdict {
    pub eligible: bool,
    /// Stable snake_case reason code for the first failing check, in open
    /// vocabulary; absent when eligible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub max_changed_lines: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_lines: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopesVerdict {
    /// `"selectors"` when gate selectors are configured, else `"repo_root"`.
    pub model: &'static str,
    /// Selector names whose paths matched at least one changed file, in
    /// selector declaration order.
    pub touched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    pub violation: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub risk: RiskVerdict,
    pub fast_path: FastPathVerdict,
    pub scopes: ScopesVerdict,
}

/// Evaluate all three verdicts from a loaded review-policy config, gates
/// config, and one request. Pure: no IO, no clock.
pub fn evaluate(
    policy: &ReviewPolicyConfig,
    gates: &GatesConfig,
    req: &ReviewRiskRequest,
) -> Verdict {
    let class = classify_risk(&policy.risk, req.changed_files, req.total_changes);
    let agentic_reviewer_required = class > policy.risk.max_auto_closeout_risk;

    let selection = selection::select(gates, req.stage.clone(), req.changed_files);
    let model = if gates.selectors.is_empty() {
        "repo_root"
    } else {
        "selectors"
    };
    let touched: Vec<String> = selection
        .selectors
        .iter()
        .filter(|s| s.matched)
        .map(|s| s.name.clone())
        .collect();
    let violation = policy.split_policy.as_deref() == Some("exclusive") && touched.len() >= 2;

    let reason = fast_path_reason(policy, &selection, &touched, violation, req);
    let fast_path = FastPathVerdict {
        eligible: reason.is_none(),
        reason,
        max_changed_lines: policy.fast_path.max_changed_lines,
        changed_lines: req.total_changes,
    };

    Verdict {
        risk: RiskVerdict {
            class,
            max_auto_closeout_risk: policy.risk.max_auto_closeout_risk,
            agentic_reviewer_required,
        },
        fast_path,
        scopes: ScopesVerdict {
            model,
            touched,
            policy: policy.split_policy.clone(),
            violation,
        },
    }
}

/// `high` when any high-risk signal fires; `low` only when every changed
/// file is low-risk (a file matching both globs counts high - high wins per
/// file) and every configured threshold holds and stats are known; else
/// `medium`. Unknown `total_changes` can never produce `low` and never
/// triggers the high-total-changes rule.
fn classify_risk(
    config: &RiskConfig,
    changed_files: &[String],
    total_changes: Option<u64>,
) -> RiskClass {
    let file_count = changed_files.len() as u64;
    let high_set = selection::glob_set(&config.high_risk_paths);
    let low_set = selection::glob_set(&config.low_risk_paths);
    let is_high_path = |file: &str| {
        high_set
            .as_ref()
            .is_some_and(|set: &GlobSet| set.is_match(file))
    };

    let high_path_match = changed_files.iter().any(|f| is_high_path(f));
    let high_file_count = config
        .high_risk_file_count
        .is_some_and(|max| file_count >= max);
    let high_total_changes = total_changes
        .zip(config.high_risk_total_changes)
        .is_some_and(|(total, max)| total >= max);

    if high_path_match || high_file_count || high_total_changes {
        return RiskClass::High;
    }

    let low_paths_configured = !config.low_risk_paths.is_empty();
    let all_files_low = low_paths_configured
        && low_set.as_ref().is_some_and(|low: &GlobSet| {
            changed_files
                .iter()
                .all(|f| !is_high_path(f) && low.is_match(f))
        });
    let file_count_ok = config
        .low_risk_file_count
        .is_none_or(|max| file_count <= max);
    let total_changes_known = total_changes.is_some();
    let total_changes_ok = config
        .low_risk_total_changes
        .is_none_or(|max| total_changes.is_some_and(|total| total <= max));

    if all_files_low && file_count_ok && total_changes_known && total_changes_ok {
        return RiskClass::Low;
    }

    RiskClass::Medium
}

/// First failing fast-path check, in the fixed order from the design;
/// `None` means eligible. Missing gate metadata (`parallel_safe`/`mutates`
/// absent) makes the multi-scope gate check conservatively fail.
fn fast_path_reason(
    policy: &ReviewPolicyConfig,
    selection: &selection::Selection,
    touched: &[String],
    split_violation: bool,
    req: &ReviewRiskRequest,
) -> Option<String> {
    if req.facts.existing_pr {
        return Some("existing_pr".to_owned());
    }
    let Some(total_changes) = req.total_changes else {
        return Some("changed_lines_unknown".to_owned());
    };
    if total_changes > policy.fast_path.max_changed_lines {
        return Some("changed_lines_exceed_max".to_owned());
    }
    if touched.len() > 1 {
        let all_parallel_safe = selection.selected.iter().all(|gate| {
            gate.parallel_safe == Some(true) && gate.mutates != Some(true) && gate.autofix.is_none()
        });
        if !all_parallel_safe {
            return Some("multi_scope_gates_not_parallel_safe".to_owned());
        }
    }
    if split_violation {
        return Some("split_policy_violation".to_owned());
    }
    if !req.facts.base_fresh {
        return Some("base_not_fresh".to_owned());
    }
    if req.facts.needs_merge {
        return Some("needs_merge".to_owned());
    }
    None
}

// ---------------------------------------------------------------------------
// Document parsing
// ---------------------------------------------------------------------------

/// Parse review_policy module text. `config` is `None` only when the file
/// itself did not parse as JSONC; schema problems still return a best-effort
/// config (defaults for anything unusable) alongside every diagnostic.
pub fn parse_review_policy(
    text: &str,
    path: &str,
) -> (Option<ReviewPolicyConfig>, Vec<Diagnostic>) {
    let root = match config::parse_jsonc(text, path, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let (config, diagnostics) = validate_document(&root, path);
    (Some(config), diagnostics)
}

/// Validate a parsed `.nopal/review_policy.jsonc` value against the
/// nopal.review_policy/v1 schema.
pub fn validate_document(
    root: &serde_json::Value,
    path: &str,
) -> (ReviewPolicyConfig, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();

    match root.get("version").and_then(|v| v.as_str()) {
        Some(REVIEW_POLICY_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported review_policy version {other:?}; expected {REVIEW_POLICY_KIND:?}"),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {REVIEW_POLICY_KIND:?}"),
        )),
    }

    let risk = parse_risk(root.get("risk"), path, &mut diagnostics);
    let fast_path = parse_fast_path(root.get("fast_path"), path, &mut diagnostics);
    let split_policy = parse_split_policy(root.get("split_policy"), path, &mut diagnostics);

    (
        ReviewPolicyConfig {
            risk,
            fast_path,
            split_policy,
        },
        diagnostics,
    )
}

fn parse_risk(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> RiskConfig {
    let mut risk = RiskConfig::default();
    let Some(value) = value else {
        return risk;
    };
    let Some(obj) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"risk\" must be an object",
        ));
        return risk;
    };

    if let Some(value) = obj.get("max_auto_closeout_risk") {
        match value.as_str().and_then(RiskClass::parse) {
            Some(class) => risk.max_auto_closeout_risk = class,
            None => diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!(
                    "risk.max_auto_closeout_risk must be one of \"low\", \"medium\", \"high\", got {value}"
                ),
            )),
        }
    }
    risk.high_risk_paths = str_list(
        obj.get("high_risk_paths"),
        "risk.high_risk_paths",
        path,
        diagnostics,
    );
    risk.low_risk_paths = str_list(
        obj.get("low_risk_paths"),
        "risk.low_risk_paths",
        path,
        diagnostics,
    );
    risk.high_risk_file_count = positive_int(
        obj.get("high_risk_file_count"),
        "risk.high_risk_file_count",
        path,
        diagnostics,
    );
    risk.high_risk_total_changes = positive_int(
        obj.get("high_risk_total_changes"),
        "risk.high_risk_total_changes",
        path,
        diagnostics,
    );
    risk.low_risk_file_count = positive_int(
        obj.get("low_risk_file_count"),
        "risk.low_risk_file_count",
        path,
        diagnostics,
    );
    risk.low_risk_total_changes = positive_int(
        obj.get("low_risk_total_changes"),
        "risk.low_risk_total_changes",
        path,
        diagnostics,
    );

    risk
}

fn parse_fast_path(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> FastPathConfig {
    let mut fast_path = FastPathConfig::default();
    let Some(value) = value else {
        return fast_path;
    };
    let Some(obj) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"fast_path\" must be an object",
        ));
        return fast_path;
    };
    if let Some(count) = positive_int(
        obj.get("max_changed_lines"),
        "fast_path.max_changed_lines",
        path,
        diagnostics,
    ) {
        fast_path.max_changed_lines = count;
    }
    fast_path
}

fn parse_split_policy(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    match value {
        None => None,
        Some(value) => match value.as_str() {
            Some(text) if !text.is_empty() => Some(text.to_owned()),
            _ => {
                diagnostics.push(Diagnostic::error(
                    Code::FieldInvalid,
                    path,
                    format!("\"split_policy\" must be a non-empty string, got {value}"),
                ));
                None
            }
        },
    }
}

fn str_list(
    value: Option<&serde_json::Value>,
    field: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let items: Option<Vec<String>> = value.as_array().and_then(|items| {
        items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect()
    });
    match items {
        Some(items) => items,
        None => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("\"{field}\" must be an array of strings"),
            ));
            Vec::new()
        }
    }
}

fn positive_int(
    value: Option<&serde_json::Value>,
    field: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<u64> {
    let value = value?;
    match value.as_u64().filter(|n| *n > 0) {
        Some(n) => Some(n),
        None => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("\"{field}\" must be a positive integer, got {value}"),
            ));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Report envelope: nopal.review_risk/v1
// ---------------------------------------------------------------------------

pub const REVIEW_RISK_KIND: &str = "nopal.review_risk/v1";

#[derive(Debug, Clone, Serialize)]
pub struct ReviewRiskInputs {
    pub changed_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_changes: Option<u64>,
    pub base_fresh: bool,
    pub needs_merge: bool,
    pub existing_pr: bool,
    pub stage: GateStage,
}

/// One envelope per `nopal review-risk` call. `ok: false` means the
/// review_policy module was missing or invalid; verdict fields are then
/// absent and `diagnostics` says why. Exit codes track `ok` only - nopal
/// decides and explains, it does not gate.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewRiskReport {
    pub kind: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_path: Option<FastPathVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<ScopesVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<ReviewRiskInputs>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<Vec<String>>,
}

/// Load `.nopal/review_policy.jsonc` and (best-effort) `.nopal/gates.jsonc`,
/// evaluate the verdicts, and build the report. A missing or invalid
/// review_policy module is the only thing that flips `ok` to false; a
/// missing or broken gates module degrades conservatively to no selectors
/// (repo_root scope model) rather than failing the seam - gates has its own
/// dedicated commands for diagnosing itself.
pub fn run(root: &Path, req: &ReviewRiskRequest) -> io::Result<ReviewRiskReport> {
    let rel = discover::module_rel_path(Module::ReviewPolicy);
    let text = validate::read_optional(&discover::module_path(root, Module::ReviewPolicy))?;
    let (policy_opt, mut diagnostics) = match text {
        Some(text) => parse_review_policy(&text, &rel),
        None => (
            None,
            vec![Diagnostic::error(
                Code::ModuleMissing,
                rel.clone(),
                format!("review-risk requires {rel}"),
            )],
        ),
    };
    diagnostics::sort(&mut diagnostics);
    let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);

    let policy = match (policy_opt, has_errors) {
        (Some(policy), false) => policy,
        _ => {
            return Ok(ReviewRiskReport {
                kind: REVIEW_RISK_KIND,
                ok: false,
                risk: None,
                fast_path: None,
                scopes: None,
                inputs: None,
                diagnostics,
                explanation: None,
            });
        }
    };

    let gates = load_gates_best_effort(root);
    let verdict = evaluate(&policy, &gates, req);
    let explanation = build_explanation(&policy, req, &verdict);
    let inputs = ReviewRiskInputs {
        changed_files: req.changed_files.len(),
        total_changes: req.total_changes,
        base_fresh: req.facts.base_fresh,
        needs_merge: req.facts.needs_merge,
        existing_pr: req.facts.existing_pr,
        stage: req.stage.clone(),
    };

    Ok(ReviewRiskReport {
        kind: REVIEW_RISK_KIND,
        ok: true,
        risk: Some(verdict.risk),
        fast_path: Some(verdict.fast_path),
        scopes: Some(verdict.scopes),
        inputs: Some(inputs),
        diagnostics,
        explanation: Some(explanation),
    })
}

fn load_gates_best_effort(root: &Path) -> GatesConfig {
    let rel = discover::module_rel_path(Module::Gates);
    match validate::read_optional(&discover::module_path(root, Module::Gates)) {
        Ok(Some(text)) => {
            let (config, diagnostics) = crate::gates::parse_gates(&text, &rel);
            let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);
            if has_errors {
                GatesConfig::default()
            } else {
                config.unwrap_or_default()
            }
        }
        _ => GatesConfig::default(),
    }
}

fn build_explanation(
    policy: &ReviewPolicyConfig,
    req: &ReviewRiskRequest,
    verdict: &Verdict,
) -> Vec<String> {
    vec![
        explain_risk(policy, req, verdict),
        explain_fast_path(verdict),
        explain_scopes(policy, verdict),
    ]
}

fn explain_risk(policy: &ReviewPolicyConfig, req: &ReviewRiskRequest, verdict: &Verdict) -> String {
    match verdict.risk.class {
        RiskClass::High => {
            let high_set = selection::glob_set(&policy.risk.high_risk_paths);
            if let Some(file) = req.changed_files.iter().find(|f| {
                high_set
                    .as_ref()
                    .is_some_and(|set: &GlobSet| set.is_match(f))
            }) {
                format!("risk high: {file} matches a high_risk_paths glob")
            } else if policy
                .risk
                .high_risk_file_count
                .is_some_and(|max| req.changed_files.len() as u64 >= max)
            {
                format!(
                    "risk high: {} changed files >= high_risk_file_count {}",
                    req.changed_files.len(),
                    policy.risk.high_risk_file_count.unwrap_or_default()
                )
            } else {
                "risk high: total_changes >= high_risk_total_changes".to_owned()
            }
        }
        RiskClass::Low => "risk low: every changed file matches low_risk_paths within the \
                            configured low thresholds"
            .to_owned(),
        RiskClass::Medium => "risk medium: no high-risk signal fired and low-risk conditions \
                               were not all satisfied"
            .to_owned(),
    }
}

fn explain_fast_path(verdict: &Verdict) -> String {
    match &verdict.fast_path.reason {
        Some(reason) => format!("fast_path ineligible: {reason}"),
        None => "fast_path eligible: every check passed".to_owned(),
    }
}

fn explain_scopes(policy: &ReviewPolicyConfig, verdict: &Verdict) -> String {
    let base = format!(
        "scopes {}: touched {}",
        verdict.scopes.model,
        verdict.scopes.touched.len()
    );
    match policy.split_policy.as_deref() {
        Some(token) if token != "exclusive" => {
            format!("{base}; split_policy {token:?} carries no defined semantics")
        }
        _ => base,
    }
}

pub fn review_risk_toon(report: &ReviewRiskReport) -> String {
    let mut doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
    ];
    if let Some(risk) = &report.risk {
        doc.push((
            "risk".into(),
            Value::Obj(vec![
                ("class".into(), Value::str(risk.class.as_str())),
                (
                    "max_auto_closeout_risk".into(),
                    Value::str(risk.max_auto_closeout_risk.as_str()),
                ),
                (
                    "agentic_reviewer_required".into(),
                    Value::Bool(risk.agentic_reviewer_required),
                ),
            ]),
        ));
    }
    if let Some(fp) = &report.fast_path {
        doc.push((
            "fast_path".into(),
            Value::Obj(vec![
                ("eligible".into(), Value::Bool(fp.eligible)),
                (
                    "reason".into(),
                    Value::str(fp.reason.clone().unwrap_or_else(|| "-".to_owned())),
                ),
                (
                    "max_changed_lines".into(),
                    Value::Int(fp.max_changed_lines as i64),
                ),
                ("changed_lines".into(), opt_int(fp.changed_lines)),
            ]),
        ));
    }
    if let Some(scopes) = &report.scopes {
        doc.push((
            "scopes".into(),
            Value::Obj(vec![
                ("model".into(), Value::str(scopes.model)),
                (
                    "touched".into(),
                    Value::Arr(scopes.touched.iter().map(Value::str).collect()),
                ),
                (
                    "policy".into(),
                    Value::str(scopes.policy.clone().unwrap_or_else(|| "-".to_owned())),
                ),
                ("violation".into(), Value::Bool(scopes.violation)),
            ]),
        ));
    }
    if let Some(inputs) = &report.inputs {
        doc.push((
            "inputs".into(),
            Value::Obj(vec![
                (
                    "changed_files".into(),
                    Value::Int(inputs.changed_files as i64),
                ),
                ("total_changes".into(), opt_int(inputs.total_changes)),
                ("base_fresh".into(), Value::Bool(inputs.base_fresh)),
                ("needs_merge".into(), Value::Bool(inputs.needs_merge)),
                ("existing_pr".into(), Value::Bool(inputs.existing_pr)),
                ("stage".into(), Value::str(inputs.stage.as_str())),
            ]),
        ));
    }
    doc.push((
        "diagnostics".into(),
        diagnostics::toon_table(&report.diagnostics),
    ));
    if let Some(explanation) = &report.explanation {
        doc.push((
            "explanation".into(),
            Value::Arr(explanation.iter().map(Value::str).collect()),
        ));
    }
    toon::encode(&doc)
}

fn opt_int(value: Option<u64>) -> Value {
    value.map_or_else(|| Value::str("-"), |n| Value::Int(n as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = ".nopal/review_policy.jsonc";

    fn parse(text: &str) -> (Option<ReviewPolicyConfig>, Vec<Diagnostic>) {
        parse_review_policy(text, PATH)
    }

    fn codes(diagnostics: &[Diagnostic]) -> Vec<Code> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    const VALID: &str = r#"{
        "version": "nopal.review_policy/v1",
        "risk": {
            "max_auto_closeout_risk": "low",
            "high_risk_paths": ["**/.github/workflows/**", "bin/**"],
            "low_risk_paths": ["docs/**", "**/*.md"],
            "high_risk_file_count": 12,
            "high_risk_total_changes": 500,
            "low_risk_file_count": 3,
            "low_risk_total_changes": 120
        },
        "fast_path": { "max_changed_lines": 100 },
        "split_policy": "exclusive"
    }"#;

    #[test]
    fn full_valid_config_parses_clean() {
        let (config, diagnostics) = parse(VALID);
        assert_eq!(diagnostics, vec![]);
        let config = config.expect("config parses");
        assert_eq!(config.risk.max_auto_closeout_risk, RiskClass::Low);
        assert_eq!(config.risk.high_risk_paths.len(), 2);
        assert_eq!(config.risk.high_risk_file_count, Some(12));
        assert_eq!(config.fast_path.max_changed_lines, 100);
        assert_eq!(config.split_policy.as_deref(), Some("exclusive"));
    }

    #[test]
    fn all_sections_optional_yields_conservative_defaults() {
        let (config, diagnostics) = parse(r#"{ "version": "nopal.review_policy/v1" }"#);
        assert_eq!(diagnostics, vec![]);
        let config = config.expect("config parses");
        assert_eq!(config.risk.max_auto_closeout_risk, RiskClass::Low);
        assert!(config.risk.high_risk_paths.is_empty());
        assert_eq!(config.risk.high_risk_file_count, None);
        assert_eq!(
            config.fast_path.max_changed_lines,
            DEFAULT_MAX_CHANGED_LINES
        );
        assert_eq!(config.split_policy, None);
    }

    #[test]
    fn missing_and_wrong_version_are_reported() {
        let (_, diagnostics) = parse(r#"{ "risk": {} }"#);
        assert_eq!(codes(&diagnostics), vec![Code::VersionUnsupported]);
        let (_, diagnostics) = parse(r#"{ "version": "nopal.review_policy/v2" }"#);
        assert_eq!(codes(&diagnostics), vec![Code::VersionUnsupported]);
    }

    #[test]
    fn unknown_risk_class_is_field_invalid_and_default_applies() {
        let (config, diagnostics) = parse(
            r#"{ "version": "nopal.review_policy/v1",
                 "risk": { "max_auto_closeout_risk": "extreme" } }"#,
        );
        assert_eq!(codes(&diagnostics), vec![Code::FieldInvalid]);
        assert_eq!(config.unwrap().risk.max_auto_closeout_risk, RiskClass::Low);
    }

    #[test]
    fn non_positive_and_wrong_type_counts_are_field_invalid() {
        for bad in ["0", "-1", "\"twelve\"", "1.5"] {
            let text = format!(
                r#"{{ "version": "nopal.review_policy/v1",
                     "risk": {{ "high_risk_file_count": {bad} }} }}"#
            );
            let (config, diagnostics) = parse(&text);
            assert_eq!(codes(&diagnostics), vec![Code::FieldInvalid], "bad: {bad}");
            assert_eq!(config.unwrap().risk.high_risk_file_count, None);
        }
    }

    #[test]
    fn wrong_shape_sections_are_field_invalid() {
        let (_, diagnostics) = parse(r#"{ "version": "nopal.review_policy/v1", "risk": [] }"#);
        assert_eq!(codes(&diagnostics), vec![Code::FieldInvalid]);
        let (_, diagnostics) = parse(r#"{ "version": "nopal.review_policy/v1", "fast_path": [] }"#);
        assert_eq!(codes(&diagnostics), vec![Code::FieldInvalid]);
        let (_, diagnostics) =
            parse(r#"{ "version": "nopal.review_policy/v1", "split_policy": 3 }"#);
        assert_eq!(codes(&diagnostics), vec![Code::FieldInvalid]);
    }

    #[test]
    fn unparseable_file_returns_no_config() {
        let (config, diagnostics) = parse("{ nope }");
        assert!(config.is_none());
        assert_eq!(codes(&diagnostics), vec![Code::ModuleParseError]);
    }

    #[test]
    fn unknown_split_policy_token_is_preserved_without_error() {
        let (config, diagnostics) =
            parse(r#"{ "version": "nopal.review_policy/v1", "split_policy": "advisory_only" }"#);
        assert_eq!(diagnostics, vec![]);
        assert_eq!(
            config.unwrap().split_policy.as_deref(),
            Some("advisory_only")
        );
    }

    // -----------------------------------------------------------------
    // Verdict core
    // -----------------------------------------------------------------

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn risk_config(text: &str) -> RiskConfig {
        let (config, diagnostics) = parse(text);
        assert_eq!(diagnostics, vec![], "fixture must validate clean");
        config.unwrap().risk
    }

    const RISK_THRESHOLDS: &str = r#"{
        "version": "nopal.review_policy/v1",
        "risk": {
            "high_risk_paths": ["bin/**"],
            "low_risk_paths": ["docs/**"],
            "high_risk_file_count": 5,
            "high_risk_total_changes": 500,
            "low_risk_file_count": 3,
            "low_risk_total_changes": 100
        }
    }"#;

    #[test]
    fn risk_is_high_by_path_match_alone() {
        let config = risk_config(RISK_THRESHOLDS);
        assert_eq!(
            classify_risk(&config, &files(&["bin/run.sh"]), Some(1)),
            RiskClass::High
        );
    }

    #[test]
    fn risk_is_high_by_file_count_alone() {
        let config = risk_config(RISK_THRESHOLDS);
        let changed = files(&["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
        assert_eq!(classify_risk(&config, &changed, Some(1)), RiskClass::High);
    }

    #[test]
    fn risk_is_high_by_total_changes_alone() {
        let config = risk_config(RISK_THRESHOLDS);
        assert_eq!(
            classify_risk(&config, &files(&["a.rs"]), Some(500)),
            RiskClass::High
        );
    }

    #[test]
    fn risk_low_happy_path() {
        let config = risk_config(RISK_THRESHOLDS);
        assert_eq!(
            classify_risk(&config, &files(&["docs/guide.md"]), Some(10)),
            RiskClass::Low
        );
    }

    #[test]
    fn file_matching_both_high_and_low_globs_counts_high() {
        let config = risk_config(
            r#"{ "version": "nopal.review_policy/v1",
                 "risk": { "high_risk_paths": ["**/*.rs"], "low_risk_paths": ["**/*.rs"] } }"#,
        );
        assert_eq!(
            classify_risk(&config, &files(&["a.rs"]), Some(1)),
            RiskClass::High
        );
    }

    #[test]
    fn unknown_stats_never_produce_low_and_never_trigger_high_total_changes() {
        let config = risk_config(RISK_THRESHOLDS);
        // Below every high threshold, all files low-risk, but stats unknown.
        assert_eq!(
            classify_risk(&config, &files(&["docs/a.md"]), None),
            RiskClass::Medium
        );
    }

    #[test]
    fn empty_low_risk_paths_never_produces_low() {
        let config = risk_config(r#"{ "version": "nopal.review_policy/v1", "risk": {} }"#);
        assert_eq!(
            classify_risk(&config, &files(&["docs/a.md"]), Some(1)),
            RiskClass::Medium
        );
    }

    #[test]
    fn absent_thresholds_vacuously_hold_for_low() {
        let config = risk_config(
            r#"{ "version": "nopal.review_policy/v1",
                 "risk": { "low_risk_paths": ["**"] } }"#,
        );
        assert_eq!(
            classify_risk(&config, &files(&["anything.rs"]), Some(999_999)),
            RiskClass::Low
        );
    }

    #[test]
    fn medium_is_the_fallback() {
        let config = risk_config(RISK_THRESHOLDS);
        assert_eq!(
            classify_risk(&config, &files(&["src/lib.rs"]), Some(50)),
            RiskClass::Medium
        );
    }

    fn gates_config(text: &str) -> GatesConfig {
        let (config, diagnostics) = crate::gates::parse_gates(text, ".nopal/gates.jsonc");
        assert_eq!(diagnostics, vec![], "fixture must validate clean");
        config.unwrap()
    }

    fn facts(base_fresh: bool, needs_merge: bool, existing_pr: bool) -> FastPathFacts {
        FastPathFacts {
            base_fresh,
            needs_merge,
            existing_pr,
        }
    }

    fn eligible_request(changed_files: &[String]) -> ReviewRiskRequest<'_> {
        ReviewRiskRequest {
            changed_files,
            total_changes: Some(1),
            facts: facts(true, false, false),
            stage: GateStage::PrePr,
        }
    }

    const NO_SELECTORS_GATES: &str = r#"{
        "version": "nopal.gates/v1",
        "gates": [ { "id": "fmt", "stage": "pre_pr", "command": "x" } ]
    }"#;

    const TWO_SELECTOR_GATES: &str = r#"{
        "version": "nopal.gates/v1",
        "gates": [
            { "id": "fmt", "stage": "pre_pr", "command": "x" },
            { "id": "docs", "stage": "pre_pr", "command": "y" }
        ],
        "gate_sets": {
            "rust": { "gates": ["fmt"] },
            "docs": { "gates": ["docs"] }
        },
        "selectors": [
            { "name": "rust-files", "paths": ["**/*.rs"], "gate_sets": ["rust"] },
            { "name": "doc-files", "paths": ["**/*.md"], "gate_sets": ["docs"] }
        ]
    }"#;

    #[test]
    fn fast_path_reason_codes_in_check_order() {
        let (policy, _) = parse(
            r#"{ "version": "nopal.review_policy/v1",
                                      "fast_path": { "max_changed_lines": 10 } }"#,
        );
        let policy = policy.unwrap();
        let gates = gates_config(NO_SELECTORS_GATES);

        let mut req = eligible_request(&[]);
        req.facts.existing_pr = true;
        assert_eq!(
            evaluate(&policy, &gates, &req).fast_path.reason.as_deref(),
            Some("existing_pr")
        );

        let mut req = eligible_request(&[]);
        req.total_changes = None;
        assert_eq!(
            evaluate(&policy, &gates, &req).fast_path.reason.as_deref(),
            Some("changed_lines_unknown")
        );

        let mut req = eligible_request(&[]);
        req.total_changes = Some(11);
        assert_eq!(
            evaluate(&policy, &gates, &req).fast_path.reason.as_deref(),
            Some("changed_lines_exceed_max")
        );

        let mut req = eligible_request(&[]);
        req.facts.base_fresh = false;
        assert_eq!(
            evaluate(&policy, &gates, &req).fast_path.reason.as_deref(),
            Some("base_not_fresh")
        );

        let mut req = eligible_request(&[]);
        req.facts.needs_merge = true;
        assert_eq!(
            evaluate(&policy, &gates, &req).fast_path.reason.as_deref(),
            Some("needs_merge")
        );

        let req = eligible_request(&[]);
        assert_eq!(evaluate(&policy, &gates, &req).fast_path.reason, None);
    }

    #[test]
    fn multi_scope_gates_without_metadata_conservatively_fail() {
        let (policy, _) = parse(r#"{ "version": "nopal.review_policy/v1" }"#);
        let policy = policy.unwrap();
        let gates = gates_config(TWO_SELECTOR_GATES);
        let changed = files(&["a.rs", "docs/a.md"]);
        let req = eligible_request(&changed);
        let verdict = evaluate(&policy, &gates, &req);
        assert_eq!(verdict.scopes.touched, vec!["rust-files", "doc-files"]);
        assert_eq!(
            verdict.fast_path.reason.as_deref(),
            Some("multi_scope_gates_not_parallel_safe")
        );
    }

    #[test]
    fn multi_scope_gates_all_parallel_safe_pass_the_check() {
        let (policy, _) = parse(r#"{ "version": "nopal.review_policy/v1" }"#);
        let policy = policy.unwrap();
        let gates = gates_config(
            r#"{
                "version": "nopal.gates/v1",
                "gates": [
                    { "id": "fmt", "stage": "pre_pr", "command": "x", "parallel_safe": true },
                    { "id": "docs", "stage": "pre_pr", "command": "y", "parallel_safe": true }
                ],
                "gate_sets": {
                    "rust": { "gates": ["fmt"] },
                    "docs": { "gates": ["docs"] }
                },
                "selectors": [
                    { "name": "rust-files", "paths": ["**/*.rs"], "gate_sets": ["rust"] },
                    { "name": "doc-files", "paths": ["**/*.md"], "gate_sets": ["docs"] }
                ]
            }"#,
        );
        let changed = files(&["a.rs", "docs/a.md"]);
        let req = eligible_request(&changed);
        let verdict = evaluate(&policy, &gates, &req);
        assert_eq!(verdict.fast_path.reason, None);
        assert!(verdict.fast_path.eligible);
    }

    #[test]
    fn repo_root_single_scope_passes_without_gate_metadata() {
        let (policy, _) = parse(r#"{ "version": "nopal.review_policy/v1" }"#);
        let policy = policy.unwrap();
        let gates = gates_config(NO_SELECTORS_GATES);
        let changed = files(&["anything.rs"]);
        let req = eligible_request(&changed);
        let verdict = evaluate(&policy, &gates, &req);
        assert_eq!(verdict.scopes.model, "repo_root");
        assert_eq!(verdict.scopes.touched, Vec::<String>::new());
        assert!(verdict.fast_path.eligible);
    }

    #[test]
    fn split_violation_requires_two_or_more_touched_selectors() {
        let (policy, _) =
            parse(r#"{ "version": "nopal.review_policy/v1", "split_policy": "exclusive" }"#);
        let policy = policy.unwrap();
        let gates = gates_config(TWO_SELECTOR_GATES);

        let zero_changed = files(&["x.txt"]);
        let zero_touched = evaluate(&policy, &gates, &eligible_request(&zero_changed));
        assert_eq!(zero_touched.scopes.touched, Vec::<String>::new());
        assert!(!zero_touched.scopes.violation);

        let one_changed = files(&["a.rs"]);
        let one_touched = evaluate(&policy, &gates, &eligible_request(&one_changed));
        assert_eq!(one_touched.scopes.touched, vec!["rust-files"]);
        assert!(!one_touched.scopes.violation);

        let two_changed = files(&["a.rs", "docs/a.md"]);
        let two_touched = evaluate(&policy, &gates, &eligible_request(&two_changed));
        assert_eq!(two_touched.scopes.touched, vec!["rust-files", "doc-files"]);
        assert!(two_touched.scopes.violation);
    }

    #[test]
    fn unknown_split_policy_token_never_produces_a_violation() {
        let (policy, _) =
            parse(r#"{ "version": "nopal.review_policy/v1", "split_policy": "advisory_only" }"#);
        let policy = policy.unwrap();
        let gates = gates_config(TWO_SELECTOR_GATES);
        let changed = files(&["a.rs", "docs/a.md"]);
        let verdict = evaluate(&policy, &gates, &eligible_request(&changed));
        assert_eq!(verdict.scopes.policy.as_deref(), Some("advisory_only"));
        assert!(!verdict.scopes.violation);
    }

    #[test]
    fn changed_file_order_does_not_affect_any_verdict() {
        let config = risk_config(RISK_THRESHOLDS);
        let forward = files(&["docs/a.md", "bin/x.sh"]);
        let reverse = files(&["bin/x.sh", "docs/a.md"]);
        assert_eq!(
            classify_risk(&config, &forward, Some(1)),
            classify_risk(&config, &reverse, Some(1))
        );

        let (policy, _) =
            parse(r#"{ "version": "nopal.review_policy/v1", "split_policy": "exclusive" }"#);
        let policy = policy.unwrap();
        let gates = gates_config(TWO_SELECTOR_GATES);
        let forward = files(&["a.rs", "docs/a.md"]);
        let reverse = files(&["docs/a.md", "a.rs"]);
        let v1 = evaluate(&policy, &gates, &eligible_request(&forward));
        let v2 = evaluate(&policy, &gates, &eligible_request(&reverse));
        assert_eq!(v1.scopes.touched, v2.scopes.touched);
        assert_eq!(v1.risk.class, v2.risk.class);
        assert_eq!(v1.fast_path.reason, v2.fast_path.reason);
    }

    // -----------------------------------------------------------------
    // Report envelope / run()
    // -----------------------------------------------------------------

    fn example(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples")
            .join(name)
    }

    fn write_project(dir: &std::path::Path, review_policy: &str, gates: &str) {
        std::fs::create_dir_all(dir.join(".nopal")).unwrap();
        std::fs::write(
            dir.join(".nopal/nopal.jsonc"),
            r#"{ "version": "nopal.project/v1", "project": { "name": "review-risk-fixture" }, "profile": "minimal" }"#,
        )
        .unwrap();
        if !review_policy.is_empty() {
            std::fs::write(dir.join(".nopal/review_policy.jsonc"), review_policy).unwrap();
        }
        if !gates.is_empty() {
            std::fs::write(dir.join(".nopal/gates.jsonc"), gates).unwrap();
        }
    }

    fn base_request(changed_files: &[String]) -> ReviewRiskRequest<'_> {
        ReviewRiskRequest {
            changed_files,
            total_changes: Some(342),
            facts: FastPathFacts {
                base_fresh: true,
                needs_merge: false,
                existing_pr: false,
            },
            stage: GateStage::PrePr,
        }
    }

    #[test]
    fn missing_review_policy_module_reports_ok_false_and_module_missing() {
        let req = base_request(&[]);
        let report = run(&example("minimal"), &req).unwrap();
        assert!(!report.ok);
        assert!(report.risk.is_none());
        assert!(report.fast_path.is_none());
        assert!(report.scopes.is_none());
        assert!(report.inputs.is_none());
        assert_eq!(report.diagnostics[0].code, Code::ModuleMissing);
    }

    #[test]
    fn full_multi_verdict_run_matches_expected_shape() {
        let temp = tempfile::tempdir().unwrap();
        write_project(
            temp.path(),
            r#"{ "version": "nopal.review_policy/v1",
                 "risk": { "high_risk_paths": ["**/*.rs"] },
                 "fast_path": { "max_changed_lines": 1000 },
                 "split_policy": "exclusive" }"#,
            r#"{
                "version": "nopal.gates/v1",
                "gates": [
                    { "id": "fmt", "stage": "pre_pr", "command": "x" },
                    { "id": "docs", "stage": "pre_pr", "command": "y" }
                ],
                "gate_sets": {
                    "rust": { "gates": ["fmt"] },
                    "docs": { "gates": ["docs"] }
                },
                "selectors": [
                    { "name": "rust-files", "paths": ["**/*.rs"], "gate_sets": ["rust"] },
                    { "name": "doc-files", "paths": ["**/*.md"], "gate_sets": ["docs"] }
                ]
            }"#,
        );
        let changed = files(&["crates/x.rs", "README.md"]);
        let req = base_request(&changed);
        let report = run(temp.path(), &req).unwrap();
        assert!(report.ok);
        assert_eq!(report.risk.as_ref().unwrap().class, RiskClass::High);
        assert!(!report.fast_path.as_ref().unwrap().eligible);
        assert_eq!(
            report.fast_path.as_ref().unwrap().reason.as_deref(),
            Some("multi_scope_gates_not_parallel_safe")
        );
        assert!(report.scopes.as_ref().unwrap().violation);
        assert_eq!(report.inputs.as_ref().unwrap().changed_files, 2);
        assert_eq!(report.diagnostics, vec![]);
        assert_eq!(report.explanation.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn missing_gates_module_degrades_to_repo_root_conservatively() {
        let temp = tempfile::tempdir().unwrap();
        write_project(
            temp.path(),
            r#"{ "version": "nopal.review_policy/v1" }"#,
            "",
        );
        let changed = files(&["a.rs"]);
        let req = ReviewRiskRequest {
            total_changes: Some(10),
            ..base_request(&changed)
        };
        let report = run(temp.path(), &req).unwrap();
        assert!(report.ok);
        assert_eq!(report.scopes.as_ref().unwrap().model, "repo_root");
        assert!(report.fast_path.as_ref().unwrap().eligible);
    }

    #[test]
    fn toon_and_json_come_from_the_same_report() {
        let temp = tempfile::tempdir().unwrap();
        write_project(
            temp.path(),
            r#"{ "version": "nopal.review_policy/v1", "split_policy": "exclusive" }"#,
            "",
        );
        let changed = files(&["a.rs"]);
        let req = base_request(&changed);
        let report = run(temp.path(), &req).unwrap();
        let toon = review_risk_toon(&report);
        let json = serde_json::to_value(&report).unwrap();
        assert!(toon.contains("kind: nopal.review_risk/v1"), "{toon}");
        assert_eq!(json["kind"], REVIEW_RISK_KIND);
        assert_eq!(json["ok"], report.ok);
        assert_eq!(json["risk"]["class"], "medium");
        assert!(toon.contains("class: medium"), "{toon}");
    }
}
