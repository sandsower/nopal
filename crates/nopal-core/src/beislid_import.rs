//! Import Beislið `.beislid/workflow.md` fenced blocks into draft Nopal modules.
//!
//! The importer is deliberately conservative: every unsupported block or field
//! becomes a diagnostic instead of being silently dropped. The parser accepts the
//! small YAML-like subset used by Beislið workflow blocks without adding a YAML
//! runtime dependency to Nopal's cold core.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Position, Severity};
use crate::gates;
use crate::guidance;
use crate::integrations;
use crate::policy;
use crate::review_policy;
use crate::toon::{self, Value as ToonValue};
use crate::workflow;

pub const BEISLID_IMPORT_KIND: &str = "nopal.beislid_import/v1";

const DEFAULT_SOURCE: &str = ".beislid/workflow.md";
const MANAGED_OUTPUTS: [(&str, &str); 6] = [
    ("integrations", "integrations.jsonc"),
    ("gates", "gates.jsonc"),
    ("policy", "policy.jsonc"),
    ("workflow", "workflow.jsonc"),
    ("guidance", "guidance.jsonc"),
    ("review_policy", "review_policy.jsonc"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    Preview,
    Write,
    Check,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub kind: &'static str,
    pub ok: bool,
    pub source: String,
    pub mode: ImportMode,
    pub outputs: Vec<DraftOutput>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DraftOutput {
    pub module: String,
    pub path: String,
    pub action: OutputAction,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputAction {
    Preview,
    Written,
    BlockedExists,
    Checked,
    Missing,
    Invalid,
    Drift,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub source: PathBuf,
    pub output_dir: PathBuf,
    pub write: bool,
    pub overwrite: bool,
    pub check: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            source: PathBuf::from(DEFAULT_SOURCE),
            output_dir: PathBuf::from(crate::discover::NOPAL_DIR),
            write: false,
            overwrite: false,
            check: false,
        }
    }
}

#[derive(Debug, Clone)]
struct Block {
    key: String,
    body: String,
    line: usize,
}

#[derive(Debug, Clone)]
struct DraftModule {
    name: &'static str,
    filename: &'static str,
    value: Value,
}

/// Typed, in-memory result shared by launch-time enforcement and the explicit
/// import command. Markdown prose never enters this representation.
#[derive(Debug, Clone)]
pub struct CompiledWorkflow {
    pub modules: BTreeMap<String, Value>,
    pub diagnostics: Vec<Diagnostic>,
}

impl CompiledWorkflow {
    pub fn ok(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error)
    }
}

fn compile_drafts(
    text: &str,
    source: &str,
    output_dir: &Path,
) -> (Vec<DraftModule>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let blocks = extract_blocks(text, source, &mut diagnostics);
    let modules = build_modules(&blocks, source, &mut diagnostics);
    for module in &modules {
        validate_draft_module(module, output_dir, &mut diagnostics);
    }
    diagnostics::sort(&mut diagnostics);
    (modules, diagnostics)
}

/// Compile recognized `beislid:*` fences without writing generated modules.
/// Unknown Beislið-owned fences remain diagnostics, while invalid recognized
/// fences make [`CompiledWorkflow::ok`] false so launch can fail closed.
pub fn compile_text(text: &str, source: &str) -> CompiledWorkflow {
    let (modules, diagnostics) =
        compile_drafts(text, source, Path::new(crate::discover::NOPAL_DIR));
    CompiledWorkflow {
        modules: modules
            .into_iter()
            .map(|module| (module.name.to_owned(), module.value))
            .collect(),
        diagnostics,
    }
}

pub fn import(root: &Path, options: &ImportOptions) -> io::Result<ImportReport> {
    let source_path = root.join(&options.source);
    let source_rel = rel_string(&options.source);
    let text = fs::read_to_string(&source_path)?;
    let (modules, mut diagnostics) = compile_drafts(&text, &source_rel, &options.output_dir);
    let generated_filenames: BTreeSet<&'static str> =
        modules.iter().map(|module| module.filename).collect();

    let mut outputs = Vec::new();
    for module in modules {
        let content = module_json(&module.value).map_err(io::Error::other)?;
        let path = options.output_dir.join(module.filename);
        let display_path = rel_string(&path);
        if options.check {
            let abs_path = root.join(&path);
            let action = if !abs_path.exists() {
                diagnostics.push(Diagnostic::error(
                    Code::BeislidImportMissing,
                    display_path.clone(),
                    "generated Beislið module is missing from the checked-in Nopal config",
                ));
                OutputAction::Missing
            } else {
                let checked_text = fs::read_to_string(&abs_path)?;
                match config::parse_jsonc(
                    &checked_text,
                    &display_path,
                    Code::BeislidImportCheckParseError,
                ) {
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        OutputAction::Invalid
                    }
                    Ok(checked_value) if checked_value == module.value => OutputAction::Checked,
                    Ok(_) => {
                        diagnostics.push(Diagnostic::error(
                            Code::BeislidImportDrift,
                            display_path.clone(),
                            "checked-in Nopal module semantics differ from the Beislið-generated module",
                        ));
                        OutputAction::Drift
                    }
                }
            };
            outputs.push(DraftOutput {
                module: module.name.to_owned(),
                path: display_path,
                action,
                bytes: content.len(),
                content: None,
            });
        } else if options.write {
            let abs_path = root.join(&path);
            if abs_path.exists() && !options.overwrite {
                diagnostics.push(Diagnostic::error(
                    Code::BeislidImportOverwriteBlocked,
                    display_path.clone(),
                    "refusing to overwrite existing file; rerun with --overwrite to replace it explicitly",
                ));
                outputs.push(DraftOutput {
                    module: module.name.to_owned(),
                    path: display_path,
                    action: OutputAction::BlockedExists,
                    bytes: content.len(),
                    content: None,
                });
                continue;
            }
            if let Some(parent) = abs_path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(&abs_path, content.as_bytes())?;
            outputs.push(DraftOutput {
                module: module.name.to_owned(),
                path: display_path,
                action: OutputAction::Written,
                bytes: content.len(),
                content: None,
            });
        } else {
            outputs.push(DraftOutput {
                module: module.name.to_owned(),
                path: display_path,
                action: OutputAction::Preview,
                bytes: content.len(),
                content: Some(content),
            });
        }
    }

    if options.check {
        for (module, filename) in MANAGED_OUTPUTS {
            if generated_filenames.contains(filename) {
                continue;
            }
            let path = options.output_dir.join(filename);
            let abs_path = root.join(&path);
            if !abs_path.exists() {
                continue;
            }
            let checked_text = fs::read_to_string(&abs_path)?;
            let display_path = rel_string(&path);
            diagnostics.push(Diagnostic::error(
                Code::BeislidImportDrift,
                display_path.clone(),
                "checked-in importer-owned module remains but the Beislið source no longer generates it",
            ));
            outputs.push(DraftOutput {
                module: module.to_owned(),
                path: display_path,
                action: OutputAction::Drift,
                bytes: checked_text.len(),
                content: None,
            });
        }
    }

    diagnostics::sort(&mut diagnostics);
    let ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);
    Ok(ImportReport {
        kind: BEISLID_IMPORT_KIND,
        ok,
        source: source_rel,
        mode: if options.check {
            ImportMode::Check
        } else if options.write {
            ImportMode::Write
        } else {
            ImportMode::Preview
        },
        outputs,
        diagnostics,
    })
}

pub fn report_toon(report: &ImportReport) -> String {
    let doc = vec![
        ("kind".into(), ToonValue::str(report.kind)),
        ("ok".into(), ToonValue::Bool(report.ok)),
        ("source".into(), ToonValue::str(report.source.clone())),
        (
            "mode".into(),
            ToonValue::str(match report.mode {
                ImportMode::Preview => "preview",
                ImportMode::Write => "write",
                ImportMode::Check => "check",
            }),
        ),
        ("outputs".into(), outputs_table(&report.outputs)),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

fn outputs_table(outputs: &[DraftOutput]) -> ToonValue {
    ToonValue::Table {
        fields: vec![
            "module".into(),
            "path".into(),
            "action".into(),
            "bytes".into(),
        ],
        rows: outputs
            .iter()
            .map(|output| {
                vec![
                    ToonValue::str(output.module.clone()),
                    ToonValue::str(output.path.clone()),
                    ToonValue::str(match output.action {
                        OutputAction::Preview => "preview",
                        OutputAction::Written => "written",
                        OutputAction::BlockedExists => "blocked_exists",
                        OutputAction::Checked => "checked",
                        OutputAction::Missing => "missing",
                        OutputAction::Invalid => "invalid",
                        OutputAction::Drift => "drift",
                    }),
                    ToonValue::Int(output.bytes as i64),
                ]
            })
            .collect(),
    }
}

fn recognized_block_key(key: &str) -> bool {
    matches!(
        key,
        "ticket_source"
            | "ticket_update"
            | "pr_review_source"
            | "pr_review_update"
            | "probe_cache"
            | "model_routing"
            | "gates"
            | "gate_sets"
            | "action_policy"
            | "agent_isolation"
            | "lifecycle_actions"
            | "plot_establishment"
            | "visual_surfaces"
            | "workflow_signals"
            | "guidance"
            | "hints"
            | "review_policy"
            | "split_policy"
    )
}

fn unclosed_block_diagnostic(path: &str, block: &Block, message: String) -> Diagnostic {
    if recognized_block_key(&block.key) {
        Diagnostic::error(Code::BeislidImportParseError, path, message).with_position(block.line, 1)
    } else {
        Diagnostic::warning(Code::BeislidImportUnsupported, path, message)
            .with_position(block.line, 1)
    }
}

fn extract_blocks(text: &str, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Option<Block> = None;

    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```beislid:") {
            if let Some(open) = current.take() {
                diagnostics.push(unclosed_block_diagnostic(
                    path,
                    &open,
                    format!(
                        "beislid block {:?} opened before previous block was closed",
                        open.key
                    ),
                ));
            }
            let key = rest.trim().to_owned();
            if key.is_empty() {
                diagnostics.push(
                    Diagnostic::warning(
                        Code::BeislidImportUnsupported,
                        path,
                        "beislid block is missing a recognized key after beislid:",
                    )
                    .with_position(line_no, 1),
                );
                continue;
            }
            current = Some(Block {
                key,
                body: String::new(),
                line: line_no,
            });
            continue;
        }

        if trimmed == "```" {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            continue;
        }

        if let Some(block) = current.as_mut() {
            block.body.push_str(line);
            block.body.push('\n');
        }
    }

    if let Some(open) = current.take() {
        diagnostics.push(unclosed_block_diagnostic(
            path,
            &open,
            format!("beislid block {:?} is missing closing fence", open.key),
        ));
    }

    blocks
}

fn build_modules(
    blocks: &[Block],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<DraftModule> {
    let mut integrations_doc = object_with_version(integrations::INTEGRATIONS_KIND);
    let mut gates_doc = object_with_version(gates::GATES_KIND);
    let mut policy_doc = object_with_version(policy::POLICY_KIND);
    let mut workflow_doc = object_with_version(workflow::WORKFLOW_KIND);
    let mut guidance_doc = object_with_version(guidance::GUIDANCE_KIND);
    let mut review_policy_doc = object_with_version(review_policy::REVIEW_POLICY_KIND);
    let mut has_integrations = false;
    let mut has_gates = false;
    let mut has_policy = false;
    let mut has_workflow = false;
    let mut has_guidance = false;
    let mut has_review_policy = false;
    let mut seen_block_keys = BTreeSet::new();

    for block in blocks {
        if !seen_block_keys.insert(block.key.clone()) {
            diagnostics.push(
                Diagnostic::warning(
                    Code::BeislidImportUnsupported,
                    path,
                    format!(
                        "duplicate beislid block {:?} ignored; first occurrence wins",
                        block.key
                    ),
                )
                .with_position(block.line, 1),
            );
            continue;
        }
        match block.key.as_str() {
            "ticket_source" => {
                has_integrations = true;
                let value = flat_map_block(block, path, diagnostics);
                put_nested(&mut integrations_doc, &["tracker", "ticket_source"], value);
            }
            "ticket_update" => {
                has_integrations = true;
                let value = flat_map_block(block, path, diagnostics);
                put_nested(&mut integrations_doc, &["tracker", "ticket_update"], value);
            }
            "pr_review_source" => {
                has_integrations = true;
                let value = flat_map_block(block, path, diagnostics);
                put_nested(&mut integrations_doc, &["pr_reviews", "source"], value);
            }
            "pr_review_update" => {
                has_integrations = true;
                let value = flat_map_block(block, path, diagnostics);
                put_nested(&mut integrations_doc, &["pr_reviews", "update"], value);
            }
            "probe_cache" => {
                has_integrations = true;
                let value = flat_map_block(block, path, diagnostics);
                put_nested(&mut integrations_doc, &["probe_cache"], value);
            }
            "model_routing" => {
                has_integrations = true;
                let value = yaml_like_block(block, path, diagnostics);
                put_nested(&mut integrations_doc, &["model_routing"], value);
            }
            "gates" => {
                has_gates = true;
                let gates = gates_block(block, path, diagnostics);
                gates_doc.insert("gates".to_owned(), Value::Array(gates));
            }
            "gate_sets" => {
                has_gates = true;
                let gate_sets = gate_sets_block(block, path, diagnostics);
                gates_doc.insert("gate_sets".to_owned(), gate_sets);
            }
            "action_policy" => {
                has_policy = true;
                let modes = policy_block(block, path, diagnostics);
                policy_doc.insert("modes".to_owned(), Value::Object(modes));
            }
            "agent_isolation" => {
                has_workflow = true;
                workflow_doc.insert(
                    "agent_isolation".to_owned(),
                    nested_yaml_block(block, path, diagnostics),
                );
            }
            "lifecycle_actions" => {
                has_workflow = true;
                let events = lifecycle_actions_block(block, path, diagnostics);
                put_nested(
                    &mut workflow_doc,
                    &["lifecycle", "events"],
                    Value::Object(events),
                );
            }
            "plot_establishment" => {
                has_workflow = true;
                let value = yaml_like_block(block, path, diagnostics);
                put_nested(&mut workflow_doc, &["establishment"], value);
            }
            "visual_surfaces" => {
                has_integrations = true;
                integrations_doc.insert(
                    "visual_surfaces".to_owned(),
                    yaml_like_block(block, path, diagnostics),
                );
            }
            "workflow_signals" => {
                has_integrations = true;
                integrations_doc.insert(
                    "workflow_signals".to_owned(),
                    yaml_like_block(block, path, diagnostics),
                );
            }
            "guidance" | "hints" => {
                has_guidance = true;
                guidance_doc.insert(
                    "hints".to_owned(),
                    yaml_like_block(block, path, diagnostics),
                );
            }
            "review_policy" => {
                has_review_policy = true;
                let (risk, skipped_agentic_subkeys) = review_policy_block(block, path, diagnostics);
                review_policy_doc.insert("risk".to_owned(), risk);
                if !skipped_agentic_subkeys.is_empty() {
                    let subkeys = skipped_agentic_subkeys
                        .iter()
                        .map(|key| format!("agentic_reviewer.{key}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    diagnostics.push(Diagnostic {
                        severity: Severity::Info,
                        code: Code::BeislidImportUnsupported,
                        path: path.to_owned(),
                        position: Some(Position {
                            line: block.line,
                            column: 1,
                        }),
                        message: format!(
                            "block {:?}: {subkeys} stay beislid-side (host integration, not a review-risk decision input)",
                            block.key
                        ),
                    });
                }
            }
            "split_policy" => {
                has_review_policy = true;
                match block.body.lines().find_map(meaningful_line) {
                    Some(token) => {
                        review_policy_doc
                            .insert("split_policy".to_owned(), Value::String(token.to_owned()));
                    }
                    None => diagnostics.push(
                        Diagnostic::error(
                            Code::BeislidImportParseError,
                            path,
                            format!(
                                "block {:?}: expected a single split-policy token",
                                block.key
                            ),
                        )
                        .with_position(block.line, 1),
                    ),
                }
            }
            "branch_pattern" => unsupported_block(
                block,
                path,
                diagnostics,
                "branch patterns have no stable Nopal v1 module field",
            ),
            other => unsupported_block(
                block,
                path,
                diagnostics,
                format!("unsupported beislid block {other:?}"),
            ),
        }
    }

    let mut modules = Vec::new();
    if has_integrations {
        modules.push(DraftModule {
            name: "integrations",
            filename: "integrations.jsonc",
            value: Value::Object(integrations_doc),
        });
    }
    if has_gates {
        modules.push(DraftModule {
            name: "gates",
            filename: "gates.jsonc",
            value: Value::Object(gates_doc),
        });
    }
    if has_policy {
        modules.push(DraftModule {
            name: "policy",
            filename: "policy.jsonc",
            value: Value::Object(policy_doc),
        });
    }
    if has_workflow {
        modules.push(DraftModule {
            name: "workflow",
            filename: "workflow.jsonc",
            value: Value::Object(workflow_doc),
        });
    }
    if has_guidance {
        modules.push(DraftModule {
            name: "guidance",
            filename: "guidance.jsonc",
            value: Value::Object(guidance_doc),
        });
    }
    if has_review_policy {
        modules.push(DraftModule {
            name: "review_policy",
            filename: "review_policy.jsonc",
            value: Value::Object(review_policy_doc),
        });
    }
    modules
}

fn object_with_version(version: &str) -> Map<String, Value> {
    let mut obj = Map::new();
    obj.insert("version".to_owned(), Value::String(version.to_owned()));
    obj
}

fn put_nested(root: &mut Map<String, Value>, path: &[&str], value: Value) {
    if let Some((last, parents)) = path.split_last() {
        let mut current = root;
        for parent in parents {
            let entry = current
                .entry((*parent).to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            current = entry.as_object_mut().unwrap_or_else(|| unreachable!());
        }
        current.insert((*last).to_owned(), value);
    }
}

fn flat_map_block(block: &Block, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Value {
    let mut obj = Map::new();
    for (offset, raw) in block.body.lines().enumerate() {
        let Some(line) = meaningful_line(raw) else {
            continue;
        };
        if let Some((key, value)) = split_key_value(line) {
            obj.insert(key.to_owned(), scalar(value));
        } else {
            diagnostics.push(
                Diagnostic::error(
                    Code::BeislidImportParseError,
                    path,
                    format!("block {:?}: expected key: value line", block.key),
                )
                .with_position(block.line + offset + 1, 1),
            );
        }
    }
    Value::Object(obj)
}

fn nested_yaml_block(block: &Block, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Value {
    match serde_yaml_ng::from_str::<Value>(&block.body) {
        Ok(Value::Object(value)) => Value::Object(value),
        Ok(_) => {
            diagnostics.push(
                Diagnostic::error(
                    Code::BeislidImportParseError,
                    path,
                    format!("block {:?}: expected a mapping", block.key),
                )
                .with_position(block.line + 1, 1),
            );
            Value::Object(Map::new())
        }
        Err(error) => {
            let location = error.location();
            diagnostics.push(
                Diagnostic::error(
                    Code::BeislidImportParseError,
                    path,
                    format!("block {:?}: invalid nested YAML: {error}", block.key),
                )
                .with_position(
                    block.line + location.as_ref().map_or(1, |location| location.line()),
                    location.as_ref().map_or(1, |location| location.column()),
                ),
            );
            Value::Object(Map::new())
        }
    }
}

fn yaml_like_block(block: &Block, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Value {
    let mut root = Map::new();
    let mut current_key: Option<String> = None;

    for (offset, raw) in block.body.lines().enumerate() {
        let line_no = block.line + offset + 1;
        let Some(line) = meaningful_line(raw) else {
            continue;
        };
        let indent = raw.chars().take_while(|c| *c == ' ').count();
        if indent == 0 {
            let Some((key, value)) = split_key_value(line) else {
                diagnostics.push(
                    Diagnostic::error(
                        Code::BeislidImportParseError,
                        path,
                        format!("block {:?}: expected key: value line", block.key),
                    )
                    .with_position(line_no, 1),
                );
                current_key = None;
                continue;
            };
            current_key = Some(key.to_owned());
            if value.is_empty() {
                root.insert(key.to_owned(), Value::Object(Map::new()));
            } else {
                root.insert(key.to_owned(), scalar(value));
            }
            continue;
        }

        let Some(parent_key) = current_key.as_ref() else {
            diagnostics.push(
                Diagnostic::error(
                    Code::BeislidImportParseError,
                    path,
                    format!(
                        "block {:?}: nested field appeared before a parent key",
                        block.key
                    ),
                )
                .with_position(line_no, 1),
            );
            continue;
        };

        if let Some(rest) = line.strip_prefix("- ") {
            let item = if let Some((key, value)) = split_key_value(rest.trim()) {
                let mut obj = Map::new();
                obj.insert(key.to_owned(), scalar(value));
                Value::Object(obj)
            } else {
                scalar(rest.trim())
            };
            let entry = root
                .entry(parent_key.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            if !entry.is_array() {
                *entry = Value::Array(Vec::new());
            }
            if let Some(items) = entry.as_array_mut() {
                items.push(item);
            }
            continue;
        }

        let Some((key, value)) = split_key_value(line) else {
            diagnostics.push(
                Diagnostic::error(
                    Code::BeislidImportParseError,
                    path,
                    format!("block {:?}: expected nested key: value line", block.key),
                )
                .with_position(line_no, 1),
            );
            continue;
        };
        let entry = root
            .entry(parent_key.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(items) = entry.as_array_mut()
            && let Some(Value::Object(last)) = items.last_mut()
        {
            last.insert(key.to_owned(), scalar(value));
            continue;
        }
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(key.to_owned(), scalar(value));
        }
    }

    Value::Object(root)
}

fn gates_block(block: &Block, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Vec<Value> {
    let mut entries: Vec<Map<String, Value>> = Vec::new();
    for (offset, raw) in block.body.lines().enumerate() {
        let Some(line) = meaningful_line(raw) else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("- ") {
            entries.push(Map::new());
            if !rest.trim().is_empty() {
                parse_gate_field(
                    rest,
                    entries.last_mut(),
                    block,
                    path,
                    block.line + offset + 1,
                    diagnostics,
                );
            }
        } else {
            parse_gate_field(
                line,
                entries.last_mut(),
                block,
                path,
                block.line + offset + 1,
                diagnostics,
            );
        }
    }

    entries
        .into_iter()
        .enumerate()
        .map(|(index, mut entry)| {
            let mut gate = Map::new();
            gate.insert(
                "id".to_owned(),
                entry
                    .remove("id")
                    .or_else(|| entry.remove("name"))
                    .unwrap_or_else(|| Value::String(format!("gate-{}", index + 1))),
            );
            gate.insert(
                "stage".to_owned(),
                entry
                    .remove("stage")
                    .and_then(|v| v.as_str().map(normalize_token).map(Value::String))
                    .unwrap_or_else(|| Value::String("pre_pr".to_owned())),
            );
            for key in [
                "command",
                "argv",
                "cwd",
                "autofix",
                "parallel_safe",
                "mutates",
            ] {
                if let Some(value) = entry.remove(key) {
                    gate.insert(key.to_owned(), value);
                }
            }
            for key in entry.keys() {
                diagnostics.push(
                    Diagnostic::warning(
                        Code::BeislidImportUnsupported,
                        path,
                        format!("block {:?}: unsupported gates field {key:?}", block.key),
                    )
                    .with_position(block.line, 1),
                );
            }
            Value::Object(gate)
        })
        .collect()
}

fn parse_gate_field(
    line: &str,
    current: Option<&mut Map<String, Value>>,
    block: &Block,
    path: &str,
    line_no: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(current) = current else {
        diagnostics.push(
            Diagnostic::error(
                Code::BeislidImportParseError,
                path,
                format!(
                    "block {:?}: gate field appeared before first list item",
                    block.key
                ),
            )
            .with_position(line_no, 1),
        );
        return;
    };
    if let Some((key, value)) = split_key_value(line) {
        current.insert(key.to_owned(), scalar(value));
    } else {
        diagnostics.push(
            Diagnostic::error(
                Code::BeislidImportParseError,
                path,
                format!("block {:?}: expected gate key: value", block.key),
            )
            .with_position(line_no, 1),
        );
    }
}

fn gate_sets_block(block: &Block, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Value {
    let Value::Object(raw_sets) = yaml_like_block(block, path, diagnostics) else {
        return Value::Object(Map::new());
    };
    let mut sets = Map::new();
    for (name, value) in raw_sets {
        match value {
            Value::Array(gates) => {
                let mut set = Map::new();
                set.insert("gates".to_owned(), Value::Array(gates));
                sets.insert(name, Value::Object(set));
            }
            Value::Object(mut obj) => {
                if let Some(gates) = obj.remove("gates") {
                    obj.insert("gates".to_owned(), gates);
                }
                sets.insert(name, Value::Object(obj));
            }
            other => {
                diagnostics.push(
                    Diagnostic::warning(
                        Code::BeislidImportUnsupported,
                        path,
                        format!(
                            "block {:?}: gate set {name:?} should be a list or object; preserving value for validation",
                            block.key
                        ),
                    )
                    .with_position(block.line, 1),
                );
                sets.insert(name, other);
            }
        }
    }
    Value::Object(sets)
}

/// Parse the `beislid:review_policy` fence. Two top-level keys carry real
/// structure - `risk` (flat scalar fields plus two glob lists) and
/// `agentic_reviewer` (host-integration fields that stay beislid-side, per
/// decision 2: one nopal module reads all three review-risk decision
/// inputs, but the reviewer-provider wiring is not one of them). Returns the
/// `risk` object plus the names of any skipped `agentic_reviewer` subkeys.
fn review_policy_block(
    block: &Block,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Value, Vec<String>) {
    let mut risk = Map::new();
    let mut current_top: Option<&'static str> = None;
    let mut current_list_key: Option<String> = None;
    let mut skipped_agentic_subkeys = Vec::new();

    for (offset, raw) in block.body.lines().enumerate() {
        let line_no = block.line + offset + 1;
        let Some(line) = meaningful_line(raw) else {
            continue;
        };
        let indent = raw.chars().take_while(|c| *c == ' ').count();

        if indent == 0 {
            let Some((key, _value)) = split_key_value(line) else {
                diagnostics.push(
                    Diagnostic::error(
                        Code::BeislidImportParseError,
                        path,
                        format!("block {:?}: expected key: value line", block.key),
                    )
                    .with_position(line_no, 1),
                );
                current_top = None;
                continue;
            };
            current_top = match key {
                "risk" => Some("risk"),
                "agentic_reviewer" => Some("agentic_reviewer"),
                other => {
                    diagnostics.push(
                        Diagnostic::warning(
                            Code::BeislidImportUnsupported,
                            path,
                            format!(
                                "block {:?}: unsupported review_policy field {other:?}",
                                block.key
                            ),
                        )
                        .with_position(line_no, 1),
                    );
                    None
                }
            };
            current_list_key = None;
            continue;
        }

        match current_top {
            Some("agentic_reviewer") => {
                if indent == 2
                    && let Some((key, _)) = split_key_value(line)
                {
                    skipped_agentic_subkeys.push(key.to_owned());
                }
            }
            Some("risk") => {
                if indent == 2 {
                    current_list_key = None;
                    let Some((key, value)) = split_key_value(line) else {
                        diagnostics.push(
                            Diagnostic::error(
                                Code::BeislidImportParseError,
                                path,
                                format!("block {:?}: expected risk key: value", block.key),
                            )
                            .with_position(line_no, 1),
                        );
                        continue;
                    };
                    if value.is_empty() {
                        current_list_key = Some(key.to_owned());
                        risk.insert(key.to_owned(), Value::Array(Vec::new()));
                    } else {
                        risk.insert(key.to_owned(), scalar(value));
                    }
                } else if let Some(rest) = line.strip_prefix("- ")
                    && let Some(list_key) = current_list_key.as_ref()
                    && let Some(Value::Array(items)) = risk.get_mut(list_key)
                {
                    items.push(scalar(rest.trim()));
                }
            }
            _ => {}
        }
    }

    (Value::Object(risk), skipped_agentic_subkeys)
}

fn policy_block(
    block: &Block,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Map<String, Value> {
    let mut modes: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut current_mode: Option<String> = None;
    let mut section: Option<&str> = None;
    let mut seen_rule_ids = BTreeSet::new();

    for (offset, raw) in block.body.lines().enumerate() {
        let line_no = block.line + offset + 1;
        let Some(trimmed) = meaningful_line(raw) else {
            continue;
        };
        let indent = raw.chars().take_while(|c| *c == ' ').count();
        if indent == 0 && trimmed == "modes:" {
            continue;
        }
        if indent == 2 && trimmed.ends_with(':') {
            let mode = normalize_token(trimmed.trim_end_matches(':'));
            modes.entry(mode.clone()).or_default();
            current_mode = Some(mode);
            section = None;
            continue;
        }
        if indent == 4 && trimmed.ends_with(':') {
            match trimmed.trim_end_matches(':') {
                "rules" => section = Some("rules"),
                "actions" => section = Some("actions"),
                other => {
                    diagnostics.push(
                        Diagnostic::warning(
                            Code::BeislidImportUnsupported,
                            path,
                            format!(
                                "block {:?}: unsupported action_policy field {other:?}",
                                block.key
                            ),
                        )
                        .with_position(line_no, 1),
                    );
                    section = None;
                }
            }
            continue;
        }
        if indent >= 6 {
            let (Some(mode), Some(section)) = (current_mode.as_ref(), section) else {
                continue;
            };
            if let Some((key, decision)) = split_key_value(trimmed) {
                let mut rule = Map::new();
                let normalized_key = normalize_token(key);
                let id = unique_rule_id(section, &normalized_key, &mut seen_rule_ids);
                rule.insert("id".to_owned(), Value::String(id));
                match section {
                    "rules" => {
                        rule.insert(
                            "classes".to_owned(),
                            Value::Array(vec![Value::String(normalized_key)]),
                        );
                    }
                    "actions" => {
                        if key.contains('*') {
                            diagnostics.push(
                                Diagnostic::warning(
                                    Code::BeislidImportUnsupported,
                                    path,
                                    format!(
                                        "block {:?}: wildcard action {key:?} has no Nopal v1 wildcard semantics and is preserved literally",
                                        block.key
                                    ),
                                )
                                .with_position(line_no, 1),
                            );
                        }
                        rule.insert(
                            "actions".to_owned(),
                            Value::Array(vec![Value::String(key.to_owned())]),
                        );
                    }
                    _ => {}
                }
                rule.insert(
                    "decision".to_owned(),
                    Value::String(normalize_token(decision)),
                );
                modes
                    .entry(mode.clone())
                    .or_default()
                    .push(Value::Object(rule));
            } else {
                diagnostics.push(
                    Diagnostic::error(
                        Code::BeislidImportParseError,
                        path,
                        format!(
                            "block {:?}: expected action policy key: decision",
                            block.key
                        ),
                    )
                    .with_position(line_no, 1),
                );
            }
        }
    }

    modes
        .into_iter()
        .map(|(mode, rules)| {
            let mut mode_obj = Map::new();
            mode_obj.insert("rules".to_owned(), Value::Array(rules));
            (mode, Value::Object(mode_obj))
        })
        .collect()
}

fn unique_rule_id(section: &str, key: &str, seen: &mut BTreeSet<String>) -> String {
    let prefix = if section == "actions" {
        "action"
    } else {
        "class"
    };
    let base = format!(
        "{prefix}-{}",
        key.replace(['.', '*'], "wildcard").replace('_', "-")
    );
    if seen.insert(base.clone()) {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if seen.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

fn lifecycle_actions_block(
    block: &Block,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Map<String, Value> {
    let mut events: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut current_event: Option<String> = None;
    let mut current_action: Option<Map<String, Value>> = None;

    for (offset, raw) in block.body.lines().enumerate() {
        let line_no = block.line + offset + 1;
        let Some(trimmed) = meaningful_line(raw) else {
            continue;
        };
        let indent = raw.chars().take_while(|c| *c == ' ').count();
        if (indent == 0 && trimmed == "events:") || trimmed == "actions:" {
            continue;
        }
        if ((indent == 0) || (indent == 2)) && trimmed.ends_with(':') {
            if let (Some(event), Some(action)) = (current_event.as_ref(), current_action.take()) {
                events
                    .entry(event.clone())
                    .or_default()
                    .push(Value::Object(action));
            }
            current_event = Some(normalize_token(trimmed.trim_end_matches(':')));
            events
                .entry(current_event.clone().unwrap_or_default())
                .or_default();
            continue;
        }
        if trimmed.starts_with("- ") {
            if let (Some(event), Some(action)) = (current_event.as_ref(), current_action.take()) {
                events
                    .entry(event.clone())
                    .or_default()
                    .push(Value::Object(action));
            }
            current_action = Some(Map::new());
            let rest = trimmed.trim_start_matches("- ").trim();
            if !rest.is_empty() {
                parse_action_field(
                    rest,
                    current_action.as_mut(),
                    block,
                    path,
                    line_no,
                    diagnostics,
                );
            }
            continue;
        }
        if indent >= 4 {
            parse_action_field(
                trimmed,
                current_action.as_mut(),
                block,
                path,
                line_no,
                diagnostics,
            );
        }
    }

    if let (Some(event), Some(action)) = (current_event.as_ref(), current_action.take()) {
        events
            .entry(event.clone())
            .or_default()
            .push(Value::Object(action));
    }

    events
        .into_iter()
        .map(|(event, actions)| {
            let mut body = Map::new();
            body.insert("actions".to_owned(), Value::Array(actions));
            (event, Value::Object(body))
        })
        .collect()
}

fn parse_action_field(
    line: &str,
    current: Option<&mut Map<String, Value>>,
    block: &Block,
    path: &str,
    line_no: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(action) = current else {
        diagnostics.push(
            Diagnostic::error(
                Code::BeislidImportParseError,
                path,
                format!(
                    "block {:?}: action field appeared before first action item",
                    block.key
                ),
            )
            .with_position(line_no, 1),
        );
        return;
    };
    if let Some((key, value)) = split_key_value(line) {
        let key = if key == "name" { "id" } else { key };
        action.insert(key.to_owned(), scalar(value));
    } else {
        diagnostics.push(
            Diagnostic::error(
                Code::BeislidImportParseError,
                path,
                format!("block {:?}: expected action key: value", block.key),
            )
            .with_position(line_no, 1),
        );
    }
}

fn unsupported_block(
    block: &Block,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    message: impl Into<String>,
) {
    diagnostics.push(
        Diagnostic::warning(Code::BeislidImportUnsupported, path, message)
            .with_position(block.line, 1),
    );
}

fn meaningful_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        None
    } else {
        Some(trimmed)
    }
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value.trim()))
}

fn scalar(text: &str) -> Value {
    let trimmed = text.trim();
    if let Some(array) = scalar_array(trimmed) {
        return array;
    }
    let unquoted = strip_quotes(trimmed);
    if unquoted == "true" {
        return Value::Bool(true);
    }
    if unquoted == "false" {
        return Value::Bool(false);
    }
    if let Ok(number) = unquoted.parse::<i64>() {
        return Value::Number(number.into());
    }
    Value::String(unquoted.to_owned())
}

fn scalar_array(text: &str) -> Option<Value> {
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    let items = inner
        .split(',')
        .map(|item| strip_quotes(item.trim()))
        .filter(|item| !item.is_empty())
        .map(|item| Value::String(item.to_owned()))
        .collect();
    Some(Value::Array(items))
}

fn strip_quotes(text: &str) -> &str {
    if text.len() >= 2 {
        let bytes = text.as_bytes();
        if (bytes[0] == b'\'' && bytes[text.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[text.len() - 1] == b'"')
        {
            return &text[1..text.len() - 1];
        }
    }
    text
}

fn normalize_token(text: &str) -> String {
    text.trim().replace('-', "_")
}

fn module_json(value: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value).map(|mut text| {
        text.push('\n');
        text
    })
}

fn validate_draft_module(
    module: &DraftModule,
    output_dir: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let path = rel_string(&output_dir.join(module.filename));
    let module_diagnostics = match module.name {
        "integrations" => integrations::validate_document(&module.value, &path),
        "gates" => gates::validate_document(&module.value, &path).1,
        "policy" => policy::validate_document(&module.value, &path).1,
        "workflow" => workflow::validate_document(&module.value, &path),
        "guidance" => guidance::validate_document(&module.value, &path),
        "review_policy" => review_policy::validate_document(&module.value, &path).1,
        _ => Vec::new(),
    };
    diagnostics.extend(module_diagnostics);
}

fn rel_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
