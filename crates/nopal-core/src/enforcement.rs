//! Deterministic enforcement planning for Nopal-launched Pi sessions.
//!
//! Core loads and composes authority sources, selects proof, and explains the
//! result. It deliberately returns gate commands to the host adapter instead
//! of executing them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use nopal_ledger_json as ledger_json;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::beislid_import;
use crate::diagnostics::{Code, Diagnostic, Severity};
use crate::gates::{self, GateStage, GatesConfig};
use crate::policy::{self, ActionClass, Decision, EvalRequest, Mode, PolicyDoc};
use crate::selection::{self, SelectedGate};

pub const ENFORCEMENT_PLAN_KIND: &str = "nopal.enforcement.plan/v1";
const WORKFLOW_PATH: &str = ".beislid/workflow.md";
const REPOSITORY_POLICY_PATH: &str = ".nopal/policy.jsonc";
const REPOSITORY_GATES_PATH: &str = ".nopal/gates.jsonc";
const USER_POLICY_FILE: &str = "policy.jsonc";

pub struct EnforcementRequest<'a> {
    pub root: &'a Path,
    pub config_dir: Option<&'a Path>,
    pub mode: Mode,
    pub action: &'a str,
    pub classes: &'a [ActionClass],
    /// Active Workflow Run Ledger directory. Without one every selected gate
    /// is conservatively reported as pending.
    pub run_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionSource {
    pub source: String,
    pub decision: Decision,
    pub matched_rules: Vec<policy::MatchedRule>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReceiptStatus {
    pub gate_id: String,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnforcementPlan {
    pub kind: &'static str,
    pub ok: bool,
    pub root: String,
    pub action: String,
    pub decision: Decision,
    pub decisions: Vec<DecisionSource>,
    /// Gates the adapter must execute before the protected action may run.
    pub required_gates: Vec<SelectedGate>,
    pub receipts: Vec<ReceiptStatus>,
    pub contract_digest: String,
    pub workspace_fingerprint: String,
    pub diagnostics: Vec<Diagnostic>,
}

struct EffectiveContract {
    policies: Vec<(String, PolicyDoc)>,
    gate_sources: Vec<(String, GatesConfig)>,
    diagnostics: Vec<Diagnostic>,
}

pub fn plan(request: EnforcementRequest<'_>) -> io::Result<EnforcementPlan> {
    let contract = load_contract(request.root, request.config_dir)?;
    let ok = contract
        .diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);
    if !ok {
        return Ok(EnforcementPlan {
            kind: ENFORCEMENT_PLAN_KIND,
            ok: false,
            root: request.root.display().to_string(),
            action: request.action.to_owned(),
            decision: Decision::Deny,
            decisions: Vec::new(),
            required_gates: Vec::new(),
            receipts: Vec::new(),
            contract_digest: String::new(),
            workspace_fingerprint: String::new(),
            diagnostics: contract.diagnostics,
        });
    }

    let mut decision = Decision::Allow;
    let mut decisions = Vec::new();
    for (source, document) in &contract.policies {
        let evaluation = policy::evaluate(
            document,
            &EvalRequest {
                mode: request.mode,
                action: request.action,
                classes: request.classes,
                env: &[],
            },
        );
        decision = decision.max(evaluation.decision);
        decisions.push(DecisionSource {
            source: source.clone(),
            decision: evaluation.decision,
            matched_rules: evaluation.matched_rules,
        });
    }

    let mut diagnostics = contract.diagnostics;
    let selected_gates = if decision == Decision::Deny {
        Vec::new()
    } else {
        select_required_gates(request.action, &contract.gate_sources, &mut diagnostics)
    };
    let contract_digest = contract_digest(request.root, request.config_dir)?;
    let workspace_fingerprint = workspace_fingerprint(request.root)?;
    let receipts: Vec<ReceiptStatus> = selected_gates
        .iter()
        .map(|gate| ReceiptStatus {
            gate_id: gate.id.clone(),
            current: request.run_dir.is_some_and(|run_dir| {
                receipt_is_current(run_dir, gate, &contract_digest, &workspace_fingerprint)
            }),
        })
        .collect();
    let required_gates = selected_gates
        .into_iter()
        .zip(&receipts)
        .filter_map(|(gate, receipt)| (!receipt.current).then_some(gate))
        .collect();
    let ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);

    Ok(EnforcementPlan {
        kind: ENFORCEMENT_PLAN_KIND,
        ok,
        root: request.root.display().to_string(),
        action: request.action.to_owned(),
        decision: if ok { decision } else { Decision::Deny },
        decisions,
        required_gates: if ok { required_gates } else { Vec::new() },
        receipts,
        contract_digest,
        workspace_fingerprint,
        diagnostics,
    })
}

pub fn record_decision(run_dir: &Path, plan: &EnforcementPlan) -> io::Result<()> {
    let payload = ledger_json::from_str(&serde_json::to_string(plan).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    crate::run_ledger_store::append_event(run_dir, "action_decision", &payload, None)
        .map_err(store_error)?;
    Ok(())
}

/// Record one adapter-executed gate attempt in the active Workflow Run Ledger.
/// A passing attempt creates a content-bound receipt; a failure records only
/// the attempt and therefore can never make a later authorization current.
pub fn record_gate(
    request: EnforcementRequest<'_>,
    run_dir: &Path,
    gate_id: &str,
    exit_code: i32,
) -> io::Result<()> {
    let mut validation_request = request;
    validation_request.run_dir = None;
    let plan = plan(validation_request)?;
    if !plan.ok {
        return Err(io::Error::other(
            "cannot record a gate for an invalid enforcement contract",
        ));
    }
    let gate = plan
        .required_gates
        .iter()
        .find(|gate| gate.id == gate_id)
        .ok_or_else(|| {
            io::Error::other(format!("gate {gate_id:?} is not required for this action"))
        })?;
    let definition_digest = gate_digest(gate);
    let payload = ledger_json::json!({
        "action": plan.action,
        "contract_digest": plan.contract_digest,
        "exit_code": exit_code,
        "gate_id": gate_id,
        "gate_definition_digest": definition_digest,
        "workspace_fingerprint": plan.workspace_fingerprint,
    });
    crate::run_ledger_store::append_event(run_dir, "gate_attempt", &payload, None)
        .map_err(store_error)?;
    if exit_code != 0 {
        return Ok(());
    }

    let receipt_path = receipt_path(run_dir, gate_id);
    crate::run_ledger_store::write_json_durable(&receipt_path, &payload)?;
    crate::run_ledger_store::append_event(
        run_dir,
        "gate_receipt",
        &ledger_json::json!({"gate_id": gate_id, "path": receipt_path.display().to_string(), "receipt": payload}),
        None,
    )
    .map_err(store_error)?;
    Ok(())
}

fn store_error(error: crate::run_ledger_store::StoreError) -> io::Error {
    match error {
        crate::run_ledger_store::StoreError::Io(error) => error,
        crate::run_ledger_store::StoreError::Domain(diagnostic) => {
            io::Error::other(diagnostic.message)
        }
    }
}

fn load_contract(root: &Path, config_dir: Option<&Path>) -> io::Result<EffectiveContract> {
    let mut policies = Vec::new();
    let mut gate_sources = Vec::new();
    let mut diagnostics = Vec::new();

    if let Some(config_dir) = config_dir {
        load_policy_path(
            &config_dir.join(USER_POLICY_FILE),
            "user policy",
            false,
            &mut policies,
            &mut diagnostics,
        )?;
    }
    load_policy_path(
        &root.join(REPOSITORY_POLICY_PATH),
        "repository policy",
        true,
        &mut policies,
        &mut diagnostics,
    )?;
    load_gates_path(
        &root.join(REPOSITORY_GATES_PATH),
        "repository gates",
        true,
        &mut gate_sources,
        &mut diagnostics,
    )?;

    let workflow_path = root.join(WORKFLOW_PATH);
    match fs::read_to_string(&workflow_path) {
        Ok(text) => {
            let compiled = beislid_import::compile_text(&text, WORKFLOW_PATH);
            diagnostics.extend(compiled.diagnostics);
            if let Some(value) = compiled.modules.get("policy") {
                let (document, source_diagnostics) =
                    policy::validate_document(value, WORKFLOW_PATH);
                diagnostics.extend(source_diagnostics);
                if let Some(document) = document {
                    policies.push(("workflow policy".to_owned(), document));
                }
            }
            if let Some(value) = compiled.modules.get("gates") {
                let (document, source_diagnostics) = gates::validate_document(value, WORKFLOW_PATH);
                diagnostics.extend(source_diagnostics);
                gate_sources.push(("workflow gates".to_owned(), document));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    crate::diagnostics::sort(&mut diagnostics);
    Ok(EffectiveContract {
        policies,
        gate_sources,
        diagnostics,
    })
}

fn load_policy_path(
    path: &Path,
    source: &str,
    required: bool,
    policies: &mut Vec<(String, PolicyDoc)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            diagnostics.push(Diagnostic::error(
                Code::ModuleMissing,
                path.display().to_string(),
                format!("enforcement requires {source}"),
            ));
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    match crate::config::parse_jsonc(&text, &path.display().to_string(), Code::ModuleParseError) {
        Ok(value) => {
            let (document, source_diagnostics) =
                policy::validate_document(&value, &path.display().to_string());
            diagnostics.extend(source_diagnostics);
            if let Some(document) = document {
                policies.push((source.to_owned(), document));
            }
        }
        Err(diagnostic) => diagnostics.push(diagnostic),
    }
    Ok(())
}

fn load_gates_path(
    path: &Path,
    source: &str,
    required: bool,
    gates_out: &mut Vec<(String, GatesConfig)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            diagnostics.push(Diagnostic::error(
                Code::ModuleMissing,
                path.display().to_string(),
                format!("enforcement requires {source}"),
            ));
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let (document, source_diagnostics) = gates::parse_gates(&text, &path.display().to_string());
    diagnostics.extend(source_diagnostics);
    if let Some(document) = document {
        gates_out.push((source.to_owned(), document));
    }
    Ok(())
}

fn action_stage(action: &str) -> Option<GateStage> {
    match action {
        "git.push" => Some(GateStage::PrePr),
        _ => None,
    }
}

fn select_required_gates(
    action: &str,
    gate_sources: &[(String, GatesConfig)],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<SelectedGate> {
    let Some(stage) = action_stage(action) else {
        return Vec::new();
    };
    let mut selected: Vec<(String, SelectedGate)> = Vec::new();
    for (source, gates) in gate_sources {
        for gate in selection::select(gates, stage.clone(), &[]).selected {
            if let Some((previous_source, previous)) =
                selected.iter().find(|(_, existing)| existing.id == gate.id)
            {
                if gate_semantics(previous) != gate_semantics(&gate) {
                    diagnostics.push(Diagnostic::error(
                        Code::ModuleParseError,
                        REPOSITORY_GATES_PATH,
                        format!(
                            "gate {:?} conflicts between {previous_source} and {source}",
                            gate.id
                        ),
                    ));
                }
                continue;
            }
            selected.push((source.clone(), gate));
        }
    }
    selected.into_iter().map(|(_, gate)| gate).collect()
}

fn gate_semantics(gate: &SelectedGate) -> Value {
    serde_json::json!({
        "id": gate.id,
        "stage": gate.stage,
        "run": gate.run,
        "cwd": gate.cwd,
        "autofix": gate.autofix,
        "parallel_safe": gate.parallel_safe,
        "mutates": gate.mutates,
    })
}

fn gate_digest(gate: &SelectedGate) -> String {
    digest_bytes(&serde_json::to_vec(&gate_semantics(gate)).unwrap_or_default())
}

fn contract_digest(root: &Path, config_dir: Option<&Path>) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let sources: [(PathBuf, &str); 3] = [
        (
            config_dir
                .map(|directory| directory.join(USER_POLICY_FILE))
                .unwrap_or_default(),
            "user-policy",
        ),
        (root.join(REPOSITORY_POLICY_PATH), "repository-policy"),
        (root.join(REPOSITORY_GATES_PATH), "repository-gates"),
    ];
    for (path, label) in sources {
        hasher.update(label.as_bytes());
        if !path.as_os_str().is_empty() {
            match fs::read(path) {
                Ok(bytes) => hasher.update(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    hasher.update(b"<missing>")
                }
                Err(error) => return Err(error),
            }
        }
    }
    hasher.update(WORKFLOW_PATH.as_bytes());
    match fs::read(root.join(WORKFLOW_PATH)) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => hasher.update(b"<missing>"),
        Err(error) => return Err(error),
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Fingerprint the revision and every pending Git-visible content change.
/// Ignored build artifacts stay outside the proof surface, while untracked
/// source files are hashed by path and content instead of status alone.
pub fn workspace_fingerprint(root: &Path) -> io::Result<String> {
    let head = git_output(root, &["rev-parse", "HEAD"]);
    let diff = git_output(root, &["diff", "--binary", "HEAD", "--"]);
    let untracked = git_output_bytes(root, &["ls-files", "--others", "--exclude-standard", "-z"]);
    if let (Some(head), Some(diff), Some(untracked)) = (head, diff, untracked) {
        let mut hasher = Sha256::new();
        hasher.update(head);
        hasher.update(diff);
        for path_bytes in untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            hasher.update(path_bytes);
            let path = String::from_utf8_lossy(path_bytes);
            match fs::read(root.join(path.as_ref())) {
                Ok(bytes) => hasher.update(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    hasher.update(b"<missing>")
                }
                Err(error) => return Err(error),
            }
        }
        return Ok(format!("{:x}", hasher.finalize()));
    }

    // Non-Git fixtures still need deterministic stale-receipt behavior.
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if relative.starts_with(".nopal") || relative.starts_with(".beislid") {
            continue;
        }
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(fs::read(path)?);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn git_output(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    git_output_bytes(root, args)
}

fn git_output_bytes(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if path.is_dir() {
            if matches!(
                relative
                    .components()
                    .next()
                    .and_then(|part| part.as_os_str().to_str()),
                Some(".git" | "target" | "node_modules")
            ) {
                continue;
            }
            collect_files(root, &path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn receipt_path(run_dir: &Path, gate_id: &str) -> PathBuf {
    let safe_id: String = gate_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    run_dir
        .join("artifacts")
        .join("enforcement")
        .join("receipts")
        .join(format!("{safe_id}.json"))
}

fn receipt_is_current(
    run_dir: &Path,
    gate: &SelectedGate,
    contract_digest: &str,
    workspace_fingerprint: &str,
) -> bool {
    let Ok(receipt) = crate::run_ledger_store::read_json(&receipt_path(run_dir, &gate.id)) else {
        return false;
    };
    receipt
        .get("exit_code")
        .and_then(ledger_json::Value::as_i64)
        == Some(0)
        && receipt
            .get("contract_digest")
            .and_then(ledger_json::Value::as_str)
            == Some(contract_digest)
        && receipt
            .get("workspace_fingerprint")
            .and_then(ledger_json::Value::as_str)
            == Some(workspace_fingerprint)
        && receipt
            .get("gate_definition_digest")
            .and_then(ledger_json::Value::as_str)
            == Some(gate_digest(gate).as_str())
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
