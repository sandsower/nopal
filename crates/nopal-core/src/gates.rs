//! `nopal.gates/v1` explicit and `nopal.gates/v2` generated module validation.
//!
//! The gates module declares *what* checks exist and *when* they apply;
//! nopal selects, decides, and explains but never executes them.
//! Parsing is diagnostic-accumulating like the manifest: one pass reports
//! every problem it can see, and `config` is `None` only when the file
//! itself did not parse.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::config;
use crate::diagnostics::{Code, Diagnostic};
use crate::gate_scaffold::{self, ScaffoldProvenance};

pub const GATES_KIND: &str = "nopal.gates/v1";
pub const GENERATED_GATES_KIND: &str = "nopal.gates/v2";

/// Placeholder names an executor is expected to substitute. v1 keeps this
/// deliberately small; growing it is additive and cheap.
pub const KNOWN_PLACEHOLDERS: [&str; 1] = ["changed_files"];

/// When a preflight runs. Preflights are readiness checks before work
/// starts; beislid's `stage: preflight` gates map onto this list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightStage {
    SessionStart,
    RunStart,
    Unknown(String),
}

/// Stages serialize as their string form so `Unknown` stays a plain string
/// in `--json` output instead of becoming an object.
impl Serialize for PreflightStage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl PreflightStage {
    pub const ALL: [PreflightStage; 2] = [PreflightStage::SessionStart, PreflightStage::RunStart];

    pub fn parse(s: &str) -> PreflightStage {
        match s {
            "session_start" => PreflightStage::SessionStart,
            "run_start" => PreflightStage::RunStart,
            other => PreflightStage::Unknown(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            PreflightStage::SessionStart => "session_start",
            PreflightStage::RunStart => "run_start",
            PreflightStage::Unknown(stage) => stage.as_str(),
        }
    }
}

/// When a gate applies. This is beislid's stage vocabulary in snake_case,
/// minus `preflight`, which nopal models as the separate `preflights` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateStage {
    PerEdit,
    PreCommit,
    PrePr,
    PostPr,
    Continuous,
    HumanInterrupt,
    Unknown(String),
}

/// Stages serialize as their string form so `Unknown` stays a plain string
/// in `--json` output instead of becoming an object.
impl Serialize for GateStage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl GateStage {
    pub const ALL: [GateStage; 6] = [
        GateStage::PerEdit,
        GateStage::PreCommit,
        GateStage::PrePr,
        GateStage::PostPr,
        GateStage::Continuous,
        GateStage::HumanInterrupt,
    ];

    pub fn parse(s: &str) -> GateStage {
        match s {
            "per_edit" => GateStage::PerEdit,
            "pre_commit" => GateStage::PreCommit,
            "pre_pr" => GateStage::PrePr,
            "post_pr" => GateStage::PostPr,
            "continuous" => GateStage::Continuous,
            "human_interrupt" => GateStage::HumanInterrupt,
            other => GateStage::Unknown(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            GateStage::PerEdit => "per_edit",
            GateStage::PreCommit => "pre_commit",
            GateStage::PrePr => "pre_pr",
            GateStage::PostPr => "post_pr",
            GateStage::Continuous => "continuous",
            GateStage::HumanInterrupt => "human_interrupt",
            GateStage::Unknown(stage) => stage.as_str(),
        }
    }
}

/// Exactly one of `command` (shell text) or `argv` (already-split words).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Run {
    Command(String),
    Argv(Vec<String>),
}

impl Run {
    /// One-line display form for tables.
    pub fn display(&self) -> String {
        match self {
            Run::Command(command) => command.clone(),
            Run::Argv(argv) => argv.join(" "),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Preflight {
    pub id: String,
    pub stage: PreflightStage,
    pub run: Run,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub id: String,
    pub stage: GateStage,
    pub run: Run,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<String>,
    /// Whether this gate is safe to run concurrently with other selected
    /// gates. The review-risk seam's fast-path check needs this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_safe: Option<bool>,
    /// Whether this gate mutates the workspace (e.g. an autofix-only run).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutates: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateSet {
    /// Gate ids in declaration order; every id resolves to a `gates` entry.
    pub gates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Selector {
    pub name: String,
    pub paths: Vec<String>,
    pub exclude: Vec<String>,
    pub gate_sets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GatesConfig {
    /// Present only on generated v2 documents. V1 remains explicit checked-in
    /// authority and cannot carry launch-readiness metadata that old readers
    /// would ignore.
    pub scaffold: Option<ScaffoldProvenance>,
    pub preflights: Vec<Preflight>,
    pub gates: Vec<Gate>,
    /// Set name -> set, iterated in name order (BTreeMap) for determinism;
    /// selection order comes from selectors, not from this map.
    pub gate_sets: BTreeMap<String, GateSet>,
    pub selectors: Vec<Selector>,
}

impl GatesConfig {
    pub fn generated_gate_ids(&self) -> BTreeSet<&str> {
        self.scaffold
            .as_ref()
            .map(|provenance| {
                provenance
                    .generated_gate_ids
                    .iter()
                    .map(String::as_str)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn has_explicit_gates(&self) -> bool {
        let generated = self.generated_gate_ids();
        if self.scaffold.is_none() {
            return !self.gates.is_empty();
        }
        self.gates
            .iter()
            .any(|gate| !generated.contains(gate.id.as_str()))
    }

    pub fn has_explicit_gates_for_stage(&self, stage: &GateStage) -> bool {
        let generated = self.generated_gate_ids();
        self.gates.iter().any(|gate| {
            &gate.stage == stage
                && (self.scaffold.is_none() || !generated.contains(gate.id.as_str()))
        })
    }
}

/// Parse and validate gates module text. Entries with a usable `id` are kept
/// even when they carry errors, so listings can show what the file declares;
/// `config` is `None` only when the file did not parse as JSONC.
pub fn parse_gates(text: &str, path: &str) -> (Option<GatesConfig>, Vec<Diagnostic>) {
    let root = match config::parse_jsonc(text, path, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let (config, diagnostics) = validate_document(&root, path);
    (Some(config), diagnostics)
}

/// Validate a parsed `.nopal/gates.jsonc` value against the explicit v1 or
/// generated v2 schema. Diagnostic-accumulating like the policy validator: everything
/// understandable comes back as a best-effort config alongside every
/// problem found.
pub fn validate_document(root: &serde_json::Value, path: &str) -> (GatesConfig, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();

    let version = root.get("version").and_then(|value| value.as_str());
    match version {
        Some(GATES_KIND | GENERATED_GATES_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!(
                "unsupported gates version {other:?}; expected {GATES_KIND:?} or {GENERATED_GATES_KIND:?}"
            ),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!(
                "missing string field \"version\"; expected {GATES_KIND:?} or {GENERATED_GATES_KIND:?}"
            ),
        )),
    }

    let mut seen_ids: Vec<String> = Vec::new();

    let preflights = entry_list(root, "preflights", path, &mut diagnostics)
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            parse_preflight(&entry, index, path, &mut seen_ids, &mut diagnostics)
        })
        .collect();

    let gates: Vec<Gate> = entry_list(root, "gates", path, &mut diagnostics)
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            parse_gate(&entry, index, path, &mut seen_ids, &mut diagnostics)
        })
        .collect();

    let gate_sets = parse_gate_sets(root, &gates, path, &mut diagnostics);
    let selectors = parse_selectors(root, &gate_sets, path, &mut diagnostics);
    let scaffold = parse_scaffold_provenance(root, version, &gates, path, &mut diagnostics);

    (
        GatesConfig {
            scaffold,
            preflights,
            gates,
            gate_sets,
            selectors,
        },
        diagnostics,
    )
}

fn parse_scaffold_provenance(
    root: &serde_json::Value,
    version: Option<&str>,
    gates: &[Gate],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ScaffoldProvenance> {
    if version == Some(GATES_KIND) {
        if root.get("scaffold").is_some() {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "nopal.gates/v1 cannot carry scaffold readiness metadata; use nopal.gates/v2",
            ));
        }
        return None;
    }
    if version != Some(GENERATED_GATES_KIND) {
        return None;
    }
    let Some(value) = root.get("scaffold") else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "nopal.gates/v2 requires an object field \"scaffold\"",
        ));
        return None;
    };
    let provenance: ScaffoldProvenance = match serde_json::from_value(value.clone()) {
        Ok(provenance) => provenance,
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("invalid scaffold provenance: {error}"),
            ));
            return None;
        }
    };
    if provenance.version != gate_scaffold::PLAN_KIND {
        diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!(
                "unsupported scaffold provenance version {:?}; expected {:?}",
                provenance.version,
                gate_scaffold::PLAN_KIND
            ),
        ));
    }
    if provenance.templates.is_empty() {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "scaffold provenance must identify at least the baseline template",
        ));
    }
    if provenance.authority != gate_scaffold::Authority::Generated {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "nopal.gates/v2 provenance authority must be \"generated\"; add explicit gates as ordinary gate entries",
        ));
    }
    for template in &provenance.templates {
        if !gate_scaffold::known_template_id(&template.id) {
            diagnostics.push(Diagnostic::error(
                Code::ScaffoldTemplateInvalid,
                path,
                format!("unknown generated gate template {:?}", template.id),
            ));
        }
    }
    let declared_gate_ids = gates
        .iter()
        .map(|gate| gate.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut generated_ids = BTreeSet::new();
    for id in &provenance.generated_gate_ids {
        if !generated_ids.insert(id.as_str()) {
            diagnostics.push(Diagnostic::error(
                Code::DuplicateId,
                path,
                format!("duplicate generated gate id {id:?} in scaffold provenance"),
            ));
        }
        if !declared_gate_ids.contains(id.as_str()) {
            diagnostics.push(Diagnostic::error(
                Code::GateRefUnknown,
                path,
                format!("scaffold provenance references missing generated gate {id:?}"),
            ));
        }
    }
    let has_nonbaseline_template = provenance
        .templates
        .iter()
        .any(|template| template.id != "baseline.git/v1");
    let has_explicit_gate = declared_gate_ids
        .iter()
        .any(|id| !generated_ids.contains(*id));
    if provenance.readiness == gate_scaffold::Readiness::Ready
        && !has_nonbaseline_template
        && !has_explicit_gate
    {
        diagnostics.push(Diagnostic::error(
            Code::GateConfigurationRequired,
            path,
            "generated readiness cannot be \"ready\" with only the baseline diff template",
        ));
    }
    Some(provenance)
}

/// Read a top-level array field; a missing field is an empty list, a
/// non-array is a `field_invalid`.
fn entry_list(
    root: &serde_json::Value,
    field: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<serde_json::Value> {
    match root.get(field) {
        None => Vec::new(),
        Some(serde_json::Value::Array(items)) => items.clone(),
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("\"{field}\" must be an array"),
            ));
            Vec::new()
        }
    }
}

fn parse_preflight(
    entry: &serde_json::Value,
    index: usize,
    path: &str,
    seen_ids: &mut Vec<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Preflight> {
    let at = format!("preflights[{index}]");
    let id = entry_id(entry, &at, path, seen_ids, diagnostics)?;

    let stage_text = required_str(entry, "stage", &at, path, diagnostics)?;
    let stage = PreflightStage::parse(&stage_text);
    if matches!(stage, PreflightStage::Unknown(_)) {
        diagnostics.push(Diagnostic::warning(
            Code::StageUnknown,
            path,
            format!(
                "{at}: unknown preflight stage {stage_text:?}; expected one of {}",
                enum_names(PreflightStage::ALL.iter().map(|s| s.as_str()))
            ),
        ));
    }

    let run = parse_run(entry, &at, path, diagnostics)?;
    let cwd = optional_str(entry, "cwd", &at, path, diagnostics);

    Some(Preflight {
        id,
        stage,
        run,
        cwd,
    })
}

fn parse_gate(
    entry: &serde_json::Value,
    index: usize,
    path: &str,
    seen_ids: &mut Vec<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Gate> {
    let at = format!("gates[{index}]");
    let id = entry_id(entry, &at, path, seen_ids, diagnostics)?;

    let stage_text = required_str(entry, "stage", &at, path, diagnostics)?;
    let stage = GateStage::parse(&stage_text);
    if matches!(stage, GateStage::Unknown(_)) {
        diagnostics.push(Diagnostic::warning(
            Code::StageUnknown,
            path,
            format!(
                "{at}: unknown gate stage {stage_text:?}; expected one of {}",
                enum_names(GateStage::ALL.iter().map(|s| s.as_str()))
            ),
        ));
    }

    let run = parse_run(entry, &at, path, diagnostics)?;
    let cwd = optional_str(entry, "cwd", &at, path, diagnostics);
    let autofix = optional_str(entry, "autofix", &at, path, diagnostics);
    if let Some(autofix) = &autofix {
        check_placeholders(autofix, &format!("{at}.autofix"), path, diagnostics);
    }
    let parallel_safe = optional_bool(entry, "parallel_safe", &at, path, diagnostics);
    let mutates = optional_bool(entry, "mutates", &at, path, diagnostics);

    Some(Gate {
        id,
        stage,
        run,
        cwd,
        autofix,
        parallel_safe,
        mutates,
    })
}

/// Shared id handling: required string, unique across preflights and gates.
fn entry_id(
    entry: &serde_json::Value,
    at: &str,
    path: &str,
    seen_ids: &mut Vec<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let id = required_str(entry, "id", at, path, diagnostics)?;
    if seen_ids.contains(&id) {
        diagnostics.push(Diagnostic::error(
            Code::DuplicateId,
            path,
            format!("{at}: duplicate id {id:?}; ids are unique across preflights and gates"),
        ));
        return None;
    }
    seen_ids.push(id.clone());
    Some(id)
}

/// Exactly one of `command` / `argv`, with placeholder validation.
fn parse_run(
    entry: &serde_json::Value,
    at: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Run> {
    let command = entry.get("command");
    let argv = entry.get("argv");
    match (command, argv) {
        (Some(_), Some(_)) => {
            diagnostics.push(Diagnostic::error(
                Code::CommandConflict,
                path,
                format!("{at}: declares both \"command\" and \"argv\"; exactly one is required"),
            ));
            None
        }
        (None, None) => {
            diagnostics.push(Diagnostic::error(
                Code::CommandMissing,
                path,
                format!("{at}: declares neither \"command\" nor \"argv\"; exactly one is required"),
            ));
            None
        }
        (Some(command), None) => match command.as_str() {
            Some(text) if !text.trim().is_empty() => {
                check_placeholders(text, &format!("{at}.command"), path, diagnostics);
                Some(Run::Command(text.to_owned()))
            }
            _ => {
                diagnostics.push(Diagnostic::error(
                    Code::CommandInvalid,
                    path,
                    format!("{at}: \"command\" must be a non-empty string"),
                ));
                None
            }
        },
        (None, Some(argv)) => {
            let words: Option<Vec<String>> = argv.as_array().and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect()
            });
            match words {
                Some(words) if !words.is_empty() && words.iter().all(|w| !w.is_empty()) => {
                    for (word_index, word) in words.iter().enumerate() {
                        check_placeholders(
                            word,
                            &format!("{at}.argv[{word_index}]"),
                            path,
                            diagnostics,
                        );
                    }
                    Some(Run::Argv(words))
                }
                _ => {
                    diagnostics.push(Diagnostic::error(
                        Code::CommandInvalid,
                        path,
                        format!("{at}: \"argv\" must be a non-empty array of non-empty strings"),
                    ));
                    None
                }
            }
        }
    }
}

/// Flat brace placeholders: `{name}` with a lowercase snake_case name from
/// the known set. No nesting, no empty braces, no unmatched braces.
fn check_placeholders(text: &str, at: &str, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let mut open_start: Option<usize> = None;
    for (idx, ch) in text.char_indices() {
        match ch {
            '{' => {
                if open_start.is_some() {
                    diagnostics.push(Diagnostic::error(
                        Code::PlaceholderInvalid,
                        path,
                        format!("{at}: nested \"{{\" in {text:?}; placeholders are flat"),
                    ));
                    return;
                }
                open_start = Some(idx + 1);
            }
            '}' => match open_start.take() {
                Some(start) => {
                    let name = &text[start..idx];
                    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    {
                        diagnostics.push(Diagnostic::error(
                            Code::PlaceholderInvalid,
                            path,
                            format!("{at}: malformed placeholder {{{name}}} in {text:?}"),
                        ));
                    } else if !KNOWN_PLACEHOLDERS.contains(&name) {
                        diagnostics.push(Diagnostic::warning(
                            Code::PlaceholderUnknown,
                            path,
                            format!(
                                "{at}: unknown placeholder {{{name}}}; expected one of {}",
                                enum_names(KNOWN_PLACEHOLDERS.iter().copied())
                            ),
                        ));
                    }
                }
                None => {
                    diagnostics.push(Diagnostic::error(
                        Code::PlaceholderInvalid,
                        path,
                        format!("{at}: unmatched \"}}\" in {text:?}"),
                    ));
                    return;
                }
            },
            _ => {}
        }
    }
    if open_start.is_some() {
        diagnostics.push(Diagnostic::error(
            Code::PlaceholderInvalid,
            path,
            format!("{at}: unmatched \"{{\" in {text:?}"),
        ));
    }
}

fn parse_gate_sets(
    root: &serde_json::Value,
    gates: &[Gate],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<String, GateSet> {
    let mut sets = BTreeMap::new();
    let entries = match root.get("gate_sets") {
        None => return sets,
        Some(serde_json::Value::Object(entries)) => entries,
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "\"gate_sets\" must be an object of set name to set".to_owned(),
            ));
            return sets;
        }
    };

    for (name, entry) in entries {
        let at = format!("gate_sets.{name}");
        let ids = match entry.get("gates") {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
            _ => {
                diagnostics.push(Diagnostic::error(
                    Code::FieldInvalid,
                    path,
                    format!("{at}: requires a \"gates\" array of gate ids"),
                ));
                continue;
            }
        };
        for id in &ids {
            if !gates.iter().any(|gate| gate.id == *id) {
                diagnostics.push(Diagnostic::error(
                    Code::GateRefUnknown,
                    path,
                    format!("{at}: references unknown gate id {id:?}"),
                ));
            }
        }
        sets.insert(name.clone(), GateSet { gates: ids });
    }
    sets
}

fn parse_selectors(
    root: &serde_json::Value,
    gate_sets: &BTreeMap<String, GateSet>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Selector> {
    entry_list(root, "selectors", path, diagnostics)
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let at = format!("selectors[{index}]");
            let name = required_str(&entry, "name", &at, path, diagnostics)?;
            let paths = required_str_list(&entry, "paths", &at, path, diagnostics)?;
            let exclude = match entry.get("exclude") {
                None => Vec::new(),
                Some(_) => required_str_list(&entry, "exclude", &at, path, diagnostics)?,
            };
            let sets = required_str_list(&entry, "gate_sets", &at, path, diagnostics)?;
            for set in &sets {
                if !gate_sets.contains_key(set) {
                    diagnostics.push(Diagnostic::error(
                        Code::GateSetUnknown,
                        path,
                        format!("{at}: references unknown gate set {set:?}"),
                    ));
                }
            }
            Some(Selector {
                name,
                paths,
                exclude,
                gate_sets: sets,
            })
        })
        .collect()
}

fn required_str(
    entry: &serde_json::Value,
    field: &str,
    at: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    match entry.get(field).and_then(|v| v.as_str()) {
        Some(text) if !text.is_empty() => Some(text.to_owned()),
        _ => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{at}: requires a non-empty string \"{field}\""),
            ));
            None
        }
    }
}

fn optional_str(
    entry: &serde_json::Value,
    field: &str,
    at: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    match entry.get(field) {
        None => None,
        Some(value) => match value.as_str() {
            Some(text) if !text.is_empty() => Some(text.to_owned()),
            _ => {
                diagnostics.push(Diagnostic::error(
                    Code::FieldInvalid,
                    path,
                    format!("{at}: \"{field}\" must be a non-empty string when present"),
                ));
                None
            }
        },
    }
}

fn optional_bool(
    entry: &serde_json::Value,
    field: &str,
    at: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<bool> {
    match entry.get(field) {
        None => None,
        Some(value) => match value.as_bool() {
            Some(b) => Some(b),
            None => {
                diagnostics.push(Diagnostic::error(
                    Code::FieldInvalid,
                    path,
                    format!("{at}: \"{field}\" must be a bool when present"),
                ));
                None
            }
        },
    }
}

fn required_str_list(
    entry: &serde_json::Value,
    field: &str,
    at: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<String>> {
    let items: Option<Vec<String>> =
        entry
            .get(field)
            .and_then(|v| v.as_array())
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect()
            });
    match items {
        Some(items) if items.iter().all(|item| !item.is_empty()) => Some(items),
        _ => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{at}: requires a \"{field}\" array of non-empty strings"),
            ));
            None
        }
    }
}

fn enum_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = ".nopal/gates.jsonc";

    fn parse(text: &str) -> (Option<GatesConfig>, Vec<Diagnostic>) {
        parse_gates(text, PATH)
    }

    fn codes(diagnostics: &[Diagnostic]) -> Vec<Code> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    #[test]
    fn full_valid_config_parses_clean() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "preflights": [
                { "id": "toolchain", "stage": "run_start", "command": "rustup show" }
            ],
            "gates": [
                { "id": "fmt", "stage": "pre_pr", "command": "cargo fmt --all --check",
                  "autofix": "cargo fmt --all" },
                { "id": "clippy", "stage": "pre_pr",
                  "argv": ["cargo", "clippy", "--workspace"], "cwd": "." }
            ],
            "gate_sets": {
                "fast": { "gates": ["fmt", "clippy"] }
            },
            "selectors": [
                { "name": "rust", "paths": ["**/*.rs"], "exclude": ["target/**"],
                  "gate_sets": ["fast"] }
            ]
        }"#;
        let (parsed, diagnostics) = parse(text);
        assert_eq!(diagnostics, vec![]);
        let parsed = parsed.expect("config parses");
        assert_eq!(parsed.preflights.len(), 1);
        assert_eq!(parsed.preflights[0].stage, PreflightStage::RunStart);
        assert_eq!(parsed.gates.len(), 2);
        assert_eq!(
            parsed.gates[1].run,
            Run::Argv(vec!["cargo".into(), "clippy".into(), "--workspace".into()])
        );
        assert_eq!(parsed.gate_sets["fast"].gates, vec!["fmt", "clippy"]);
        assert_eq!(parsed.selectors[0].name, "rust");
    }

    #[test]
    fn missing_and_wrong_version_are_reported() {
        let (_, diagnostics) = parse(r#"{ "gates": [] }"#);
        assert_eq!(codes(&diagnostics), vec![Code::VersionUnsupported]);
        let (_, diagnostics) = parse(r#"{ "version": "nopal.gates/v99" }"#);
        assert_eq!(codes(&diagnostics), vec![Code::VersionUnsupported]);
    }

    #[test]
    fn duplicate_ids_across_preflights_and_gates_are_errors() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "preflights": [
                { "id": "fmt", "stage": "run_start", "command": "x" }
            ],
            "gates": [
                { "id": "fmt", "stage": "pre_pr", "command": "y" },
                { "id": "fmt", "stage": "pre_pr", "command": "z" }
            ]
        }"#;
        let (parsed, diagnostics) = parse(text);
        assert_eq!(
            codes(&diagnostics),
            vec![Code::DuplicateId, Code::DuplicateId]
        );
        // First declaration wins; duplicates are dropped.
        let parsed = parsed.expect("config parses");
        assert_eq!(parsed.preflights.len(), 1);
        assert_eq!(parsed.gates.len(), 0);
    }

    #[test]
    fn unknown_stages_are_warned_and_preserved() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "preflights": [
                { "id": "a", "stage": "someday", "command": "x" }
            ],
            "gates": [
                { "id": "b", "stage": "preflight", "command": "y" }
            ]
        }"#;
        let (parsed, diagnostics) = parse(text);
        assert_eq!(
            codes(&diagnostics),
            vec![Code::StageUnknown, Code::StageUnknown]
        );
        assert!(
            diagnostics
                .iter()
                .all(|d| d.severity == crate::diagnostics::Severity::Warning)
        );
        assert!(
            matches!(parsed, Some(GatesConfig { preflights, gates, .. }) if matches!(preflights[0].stage, PreflightStage::Unknown(_)) && matches!(gates[0].stage, GateStage::Unknown(_)))
        );
        assert!(
            diagnostics[1].message.contains("pre_pr"),
            "{}",
            diagnostics[1].message
        );
    }

    #[test]
    fn command_and_argv_are_exactly_one() {
        let both = r#"{
            "version": "nopal.gates/v1",
            "gates": [
                { "id": "a", "stage": "pre_pr", "command": "x", "argv": ["x"] }
            ]
        }"#;
        let (_, diagnostics) = parse(both);
        assert_eq!(codes(&diagnostics), vec![Code::CommandConflict]);

        let neither = r#"{
            "version": "nopal.gates/v1",
            "gates": [ { "id": "a", "stage": "pre_pr" } ]
        }"#;
        let (_, diagnostics) = parse(neither);
        assert_eq!(codes(&diagnostics), vec![Code::CommandMissing]);
    }

    #[test]
    fn empty_command_and_bad_argv_are_invalid() {
        let empty_command = r#"{
            "version": "nopal.gates/v1",
            "gates": [ { "id": "a", "stage": "pre_pr", "command": "  " } ]
        }"#;
        let (_, diagnostics) = parse(empty_command);
        assert_eq!(codes(&diagnostics), vec![Code::CommandInvalid]);

        for argv in [r#"[]"#, r#"["ok", ""]"#, r#"["ok", 3]"#] {
            let text = format!(
                r#"{{ "version": "nopal.gates/v1",
                     "gates": [ {{ "id": "a", "stage": "pre_pr", "argv": {argv} }} ] }}"#
            );
            let (_, diagnostics) = parse(&text);
            assert_eq!(
                codes(&diagnostics),
                vec![Code::CommandInvalid],
                "argv: {argv}"
            );
        }
    }

    #[test]
    fn placeholders_are_flat_and_known() {
        let cases: &[(&str, Code)] = &[
            ("echo {changed_files} {mystery}", Code::PlaceholderUnknown),
            ("echo {unclosed", Code::PlaceholderInvalid),
            ("echo {a{b}}", Code::PlaceholderInvalid),
            ("echo {}", Code::PlaceholderInvalid),
            ("echo {Bad-Name}", Code::PlaceholderInvalid),
            ("echo close} only", Code::PlaceholderInvalid),
            (
                "cmd {changed_files} bad} {changed_files}",
                Code::PlaceholderInvalid,
            ),
        ];
        for (command, expected) in cases {
            let text = format!(
                r#"{{ "version": "nopal.gates/v1",
                     "gates": [ {{ "id": "a", "stage": "pre_pr", "command": {command:?} }} ] }}"#
            );
            let (_, diagnostics) = parse(&text);
            assert_eq!(codes(&diagnostics), vec![*expected], "command: {command}");
            if *expected == Code::PlaceholderUnknown {
                assert!(diagnostics[0].severity == crate::diagnostics::Severity::Warning);
            }
        }

        let ok = r#"{
            "version": "nopal.gates/v1",
            "gates": [
                { "id": "a", "stage": "pre_pr", "command": "lint {changed_files}" }
            ]
        }"#;
        let (_, diagnostics) = parse(ok);
        assert_eq!(diagnostics, vec![]);
    }

    #[test]
    fn placeholders_in_argv_and_autofix_are_checked() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "gates": [
                { "id": "a", "stage": "pre_pr", "argv": ["lint", "{nope}"],
                  "autofix": "fix {broken" }
            ]
        }"#;
        let (_, diagnostics) = parse(text);
        // argv is validated with the run declaration, autofix after it.
        assert_eq!(
            codes(&diagnostics),
            vec![Code::PlaceholderUnknown, Code::PlaceholderInvalid]
        );
    }

    #[test]
    fn unknown_gate_reference_in_set_is_an_error() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "gates": [ { "id": "fmt", "stage": "pre_pr", "command": "x" } ],
            "gate_sets": { "fast": { "gates": ["fmt", "missing"] } }
        }"#;
        let (_, diagnostics) = parse(text);
        assert_eq!(codes(&diagnostics), vec![Code::GateRefUnknown]);
    }

    #[test]
    fn unknown_gate_set_reference_in_selector_is_an_error() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "gates": [ { "id": "fmt", "stage": "pre_pr", "command": "x" } ],
            "gate_sets": { "fast": { "gates": ["fmt"] } },
            "selectors": [
                { "name": "all", "paths": ["**"], "gate_sets": ["fast", "missing"] }
            ]
        }"#;
        let (_, diagnostics) = parse(text);
        assert_eq!(codes(&diagnostics), vec![Code::GateSetUnknown]);
    }

    #[test]
    fn structural_field_problems_are_field_invalid() {
        let cases = [
            r#"{ "version": "nopal.gates/v1", "gates": {} }"#,
            r#"{ "version": "nopal.gates/v1", "gates": [ { "stage": "pre_pr", "command": "x" } ] }"#,
            r#"{ "version": "nopal.gates/v1", "gate_sets": [] }"#,
            r#"{ "version": "nopal.gates/v1", "gate_sets": { "fast": {} } }"#,
            r#"{ "version": "nopal.gates/v1", "selectors": [ { "name": "a", "gate_sets": [] } ] }"#,
        ];
        for text in cases {
            let (_, diagnostics) = parse(text);
            assert_eq!(
                codes(&diagnostics),
                vec![Code::FieldInvalid],
                "text: {text}"
            );
        }
    }

    #[test]
    fn unparseable_file_returns_no_config() {
        let (parsed, diagnostics) = parse("{ nope }");
        assert!(parsed.is_none());
        assert_eq!(codes(&diagnostics), vec![Code::ModuleParseError]);
    }

    #[test]
    fn parallel_safe_and_mutates_are_optional_bools() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "gates": [
                { "id": "fmt", "stage": "pre_pr", "command": "x",
                  "parallel_safe": true, "mutates": false },
                { "id": "clippy", "stage": "pre_pr", "command": "y" }
            ]
        }"#;
        let (parsed, diagnostics) = parse(text);
        assert_eq!(diagnostics, vec![]);
        let parsed = parsed.expect("config parses");
        assert_eq!(parsed.gates[0].parallel_safe, Some(true));
        assert_eq!(parsed.gates[0].mutates, Some(false));
        assert_eq!(parsed.gates[1].parallel_safe, None);
        assert_eq!(parsed.gates[1].mutates, None);
    }

    #[test]
    fn parallel_safe_and_mutates_wrong_type_is_field_invalid() {
        for field in ["parallel_safe", "mutates"] {
            let text = format!(
                r#"{{ "version": "nopal.gates/v1",
                     "gates": [ {{ "id": "a", "stage": "pre_pr", "command": "x",
                                   "{field}": "yes" }} ] }}"#
            );
            let (_, diagnostics) = parse(&text);
            assert_eq!(
                codes(&diagnostics),
                vec![Code::FieldInvalid],
                "field: {field}"
            );
        }
    }
}
