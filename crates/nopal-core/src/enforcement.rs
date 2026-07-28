//! Deterministic enforcement planning for Nopal-launched Pi sessions.
//!
//! Core loads and composes authority sources, selects proof, and explains the
//! result. It deliberately returns gate commands to the host adapter instead
//! of executing them.

use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
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
const RECEIPT_CAPABILITY_PATH: &str = "artifacts/enforcement/receipt-capability";

pub struct EnforcementRequest<'a> {
    pub root: &'a Path,
    pub config_dir: Option<&'a Path>,
    pub mode: Mode,
    pub action: &'a str,
    pub classes: &'a [ActionClass],
    /// Active Workflow Run Ledger directory. Without one every selected gate
    /// is conservatively reported as pending.
    pub run_dir: Option<&'a Path>,
    /// Ephemeral run capability used to authenticate receipts. The CLI loads
    /// it from protected run state; it is never serialized into evidence,
    /// contracts, subprocess arguments, or the project tree.
    pub receipt_key: Option<&'a [u8]>,
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
    pub gate_definition_digest: String,
}

#[derive(Debug, Clone)]
pub struct GateExecutionContext {
    pub contract_digest: String,
    pub workspace_fingerprint: String,
    pub gate_definition_digest: String,
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
    /// Digest of the exact confined bytes parsed into this contract.
    contract_digest: String,
}

struct ContractDigest(Sha256);

impl ContractDigest {
    fn new() -> Self {
        Self(Sha256::new())
    }

    /// Length-prefix every field so distinct source boundaries cannot hash to
    /// the same concatenated byte stream. Missing authority is part of the
    /// contract and is distinct from an empty file.
    fn record(&mut self, label: &str, bytes: Option<&[u8]>) {
        self.0.update((label.len() as u64).to_be_bytes());
        self.0.update(label.as_bytes());
        match bytes {
            Some(bytes) => {
                self.0.update([1]);
                self.0.update((bytes.len() as u64).to_be_bytes());
                self.0.update(bytes);
            }
            None => self.0.update([0]),
        }
    }

    fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
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
    let contract_digest = contract.contract_digest;
    let workspace_fingerprint = workspace_fingerprint(request.root)?;
    let receipts: Vec<ReceiptStatus> = selected_gates
        .iter()
        .map(|gate| ReceiptStatus {
            gate_id: gate.id.clone(),
            current: request.run_dir.is_some_and(|run_dir| {
                request.receipt_key.is_some_and(|receipt_key| {
                    receipt_is_current(
                        run_dir,
                        request.action,
                        gate,
                        &contract_digest,
                        &workspace_fingerprint,
                        receipt_key,
                    )
                })
            }),
            gate_definition_digest: gate_digest(gate),
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
    expected: &GateExecutionContext,
) -> io::Result<()> {
    let receipt_key = request.receipt_key.ok_or_else(|| {
        io::Error::other("recording a gate requires the active adapter receipt capability")
    })?;
    if receipt_key.len() < 32 {
        return Err(io::Error::other(
            "the adapter receipt capability must contain at least 256 bits",
        ));
    }

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
    if plan.contract_digest != expected.contract_digest
        || plan.workspace_fingerprint != expected.workspace_fingerprint
        || definition_digest != expected.gate_definition_digest
    {
        return Err(io::Error::other(
            "the enforcement contract, workspace, or gate definition changed during execution",
        ));
    }

    let signature = receipt_signature(
        receipt_key,
        &plan.action,
        &plan.contract_digest,
        &plan.workspace_fingerprint,
        gate_id,
        &definition_digest,
        exit_code,
    );
    let payload = ledger_json::json!({
        "action": plan.action,
        "contract_digest": plan.contract_digest,
        "exit_code": exit_code,
        "gate_id": gate_id,
        "gate_definition_digest": definition_digest,
        "workspace_fingerprint": plan.workspace_fingerprint,
        "signature": signature,
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
    let mut digest = ContractDigest::new();

    if let Some(config_dir) = config_dir {
        load_policy_path(
            config_dir,
            Path::new(USER_POLICY_FILE),
            "user policy",
            false,
            &mut policies,
            &mut diagnostics,
            &mut digest,
        )?;
    } else {
        digest.record("user policy", None);
    }
    load_policy_path(
        root,
        Path::new(REPOSITORY_POLICY_PATH),
        "repository policy",
        true,
        &mut policies,
        &mut diagnostics,
        &mut digest,
    )?;
    load_gates_path(
        root,
        Path::new(REPOSITORY_GATES_PATH),
        "repository gates",
        true,
        &mut gate_sources,
        &mut diagnostics,
        &mut digest,
    )?;

    match read_contract_text(root, Path::new(WORKFLOW_PATH), "workflow", &mut digest) {
        Ok(Some(text)) => {
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
        Ok(None) => {}
        Err(error) => diagnostics.push(Diagnostic::error(
            Code::ModuleParseError,
            WORKFLOW_PATH,
            format!("could not read confined workflow authority: {error}"),
        )),
    }

    crate::diagnostics::sort(&mut diagnostics);
    Ok(EffectiveContract {
        policies,
        gate_sources,
        diagnostics,
        contract_digest: digest.finish(),
    })
}

fn read_contract_text(
    directory: &Path,
    relative: &Path,
    label: &str,
    digest: &mut ContractDigest,
) -> io::Result<Option<String>> {
    let bytes = crate::confined_read::read_bytes(directory, relative, 1024 * 1024)?;
    digest.record(label, bytes.as_deref());
    bytes
        .map(|bytes| {
            String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
        .transpose()
}

fn load_policy_path(
    directory: &Path,
    relative: &Path,
    source: &str,
    required: bool,
    policies: &mut Vec<(String, PolicyDoc)>,
    diagnostics: &mut Vec<Diagnostic>,
    digest: &mut ContractDigest,
) -> io::Result<()> {
    let path = directory.join(relative);
    let text = match read_contract_text(directory, relative, source, digest) {
        Ok(Some(text)) => text,
        Ok(None) if !required => return Ok(()),
        Ok(None) => {
            diagnostics.push(Diagnostic::error(
                Code::ModuleMissing,
                path.display().to_string(),
                format!("enforcement requires {source}"),
            ));
            return Ok(());
        }
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                Code::ModuleParseError,
                path.display().to_string(),
                format!("could not read confined {source}: {error}"),
            ));
            return Ok(());
        }
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
    directory: &Path,
    relative: &Path,
    source: &str,
    required: bool,
    gates_out: &mut Vec<(String, GatesConfig)>,
    diagnostics: &mut Vec<Diagnostic>,
    digest: &mut ContractDigest,
) -> io::Result<()> {
    let path = directory.join(relative);
    let text = match read_contract_text(directory, relative, source, digest) {
        Ok(Some(text)) => text,
        Ok(None) if !required => return Ok(()),
        Ok(None) => {
            diagnostics.push(Diagnostic::error(
                Code::ModuleMissing,
                path.display().to_string(),
                format!("enforcement requires {source}"),
            ));
            return Ok(());
        }
        Err(error) => {
            diagnostics.push(Diagnostic::error(
                Code::ModuleParseError,
                path.display().to_string(),
                format!("could not read confined {source}: {error}"),
            ));
            return Ok(());
        }
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
    let selections = gate_sources
        .iter()
        .map(|(source, gates)| (source, gates, selection::select(gates, stage.clone(), &[])))
        .collect::<Vec<_>>();
    // Authority suppresses generated defaults only when same-stage explicit
    // proof was actually selected. Selector-scoped declarations that match no
    // changed file cannot turn a protected action into a zero-gate action.
    let selected_explicit_authority = selections.iter().any(|(_, gates, selection)| {
        let generated = gates.generated_gate_ids();
        selection
            .selected
            .iter()
            .any(|gate| !generated.contains(gate.id.as_str()))
    });
    let mut selected: Vec<(String, SelectedGate)> = Vec::new();
    for (source, gates, selection) in selections {
        let generated = gates.generated_gate_ids();
        for gate in selection
            .selected
            .into_iter()
            .filter(|gate| !(selected_explicit_authority && generated.contains(gate.id.as_str())))
        {
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

/// Fingerprint the revision and every pending Git-visible content change.
/// Ignored build artifacts stay outside the proof surface, while untracked
/// source files are hashed by path and content instead of status alone.
/// Git-visible symlinks may point within the repository, where their targets
/// are already covered, but external targets fail closed because Git cannot
/// provide stable evidence for their changing content.
pub fn workspace_fingerprint(root: &Path) -> io::Result<String> {
    let head = git_output(root, &["rev-parse", "HEAD"]);
    let diff = git_output(root, &["diff", "--binary", "HEAD", "--"]);
    let untracked = git_output_bytes(root, &["ls-files", "--others", "--exclude-standard", "-z"]);
    let visible = git_output_bytes(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    );
    if let (Some(head), Some(diff), Some(untracked), Some(visible)) =
        (head, diff, untracked, visible)
    {
        reject_external_symlinks(root, &visible)?;
        let mut hasher = Sha256::new();
        hasher.update(head);
        hasher.update(diff);
        for path_bytes in untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            hasher.update(path_bytes);
            let path = String::from_utf8_lossy(path_bytes);
            let full_path = root.join(path.as_ref());
            match fs::symlink_metadata(&full_path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    hasher.update(b"<symlink>");
                    hasher.update(fs::read_link(full_path)?.to_string_lossy().as_bytes());
                }
                Ok(_) => hasher.update(fs::read(full_path)?),
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
    let visible = files
        .iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .flat_map(|path| {
            let mut bytes = path.to_string_lossy().as_bytes().to_vec();
            bytes.push(0);
            bytes
        })
        .collect::<Vec<_>>();
    reject_external_symlinks(root, &visible)?;
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if relative.starts_with(".nopal") || relative.starts_with(".beislid") {
            continue;
        }
        hasher.update(relative.to_string_lossy().as_bytes());
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            hasher.update(b"<symlink>");
            hasher.update(fs::read_link(path)?.to_string_lossy().as_bytes());
        } else {
            hasher.update(fs::read(path)?);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn reject_external_symlinks(root: &Path, nul_paths: &[u8]) -> io::Result<()> {
    let canonical_root = fs::canonicalize(root)?;
    for path_bytes in nul_paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = String::from_utf8_lossy(path_bytes);
        let path = root.join(relative.as_ref());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let target = fs::canonicalize(&path).map_err(|error| {
            io::Error::other(format!(
                "workspace symlink {} cannot provide stable gate evidence: {error}",
                path.display()
            ))
        })?;
        if target != canonical_root && !target.starts_with(&canonical_root) {
            return Err(io::Error::other(format!(
                "workspace symlink {} escapes the repository proof surface",
                path.display()
            )));
        }
    }
    Ok(())
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
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            files.push(path);
            continue;
        }
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
    action: &str,
    gate: &SelectedGate,
    contract_digest: &str,
    workspace_fingerprint: &str,
    receipt_key: &[u8],
) -> bool {
    let Ok(receipt) = crate::run_ledger_store::read_json(&receipt_path(run_dir, &gate.id)) else {
        return false;
    };
    let definition_digest = gate_digest(gate);
    let expected_signature = receipt_signature(
        receipt_key,
        action,
        contract_digest,
        workspace_fingerprint,
        &gate.id,
        &definition_digest,
        0,
    );
    receipt.get("action").and_then(ledger_json::Value::as_str) == Some(action)
        && receipt
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
            == Some(definition_digest.as_str())
        && receipt
            .get("signature")
            .and_then(ledger_json::Value::as_str)
            .is_some_and(|actual| {
                constant_time_eq(actual.as_bytes(), expected_signature.as_bytes())
            })
}

fn receipt_signature(
    key: &[u8],
    action: &str,
    contract_digest: &str,
    workspace_fingerprint: &str,
    gate_id: &str,
    gate_definition_digest: &str,
    exit_code: i32,
) -> String {
    let mut normalized_key = [0_u8; 64];
    if key.len() > normalized_key.len() {
        normalized_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for index in 0..normalized_key.len() {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for component in [
        action,
        contract_digest,
        workspace_fingerprint,
        gate_id,
        gate_definition_digest,
    ] {
        inner.update((component.len() as u64).to_be_bytes());
        inner.update(component.as_bytes());
    }
    inner.update(exit_code.to_be_bytes());
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    format!("{:x}", outer.finalize())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub fn initialize_receipt_capability(run_dir: &Path) -> io::Result<()> {
    let path = run_dir.join(RECEIPT_CAPABILITY_PATH);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("receipt capability path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(generate_receipt_key()?.as_bytes())?;
    file.sync_all()?;
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub fn load_receipt_capability(run_dir: &Path) -> io::Result<String> {
    let capability = fs::read_to_string(run_dir.join(RECEIPT_CAPABILITY_PATH))?;
    if capability.len() != 64 || !capability.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other(
            "enforcement receipt capability is malformed",
        ));
    }
    Ok(capability)
}

pub fn generate_receipt_key() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        io::Error::other(format!(
            "could not initialize enforcement receipt capability: {error}"
        ))
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[test]
    fn loaded_contract_digest_is_bound_to_the_same_bytes_as_parsed_authority() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join(REPOSITORY_POLICY_PATH),
            r#"{"version":"nopal.policy/v1","modes":{}}"#,
        );
        let gates = temp.path().join(REPOSITORY_GATES_PATH);
        write(
            &gates,
            r#"{"version":"nopal.gates/v1","gates":[{"id":"first","stage":"pre_pr","argv":["true"]}]}"#,
        );

        let first = load_contract(temp.path(), None).unwrap();
        write(
            &gates,
            r#"{"version":"nopal.gates/v1","gates":[{"id":"second","stage":"pre_pr","argv":["false"]}]}"#,
        );
        let second = load_contract(temp.path(), None).unwrap();

        assert_eq!(first.gate_sources[0].1.gates[0].id, "first");
        assert_eq!(second.gate_sources[0].1.gates[0].id, "second");
        assert_ne!(first.contract_digest, second.contract_digest);
    }
}
