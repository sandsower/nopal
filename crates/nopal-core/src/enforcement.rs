//! Deterministic enforcement planning for Nopal-launched Pi sessions.
//!
//! Core loads and composes authority sources, selects proof, and explains the
//! result. It deliberately returns gate commands to the host adapter instead
//! of executing them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nopal_ledger_json as ledger_json;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::beislid_import;
use crate::diagnostics::{Code, Diagnostic, Severity};
use crate::gates::{self, GateStage, GatesConfig};
use crate::policy::{self, ActionClass, Decision, EvalRequest, Mode, Placement, PolicyDoc, Source};
use crate::selection::{self, SelectedGate};

pub const ENFORCEMENT_PLAN_KIND: &str = "nopal.enforcement.plan/v2";
pub const ENFORCEMENT_INTENT_KIND: &str = "nopal.enforcement.intent/v1";
const WORKFLOW_PATH: &str = ".beislid/workflow.md";
const REPOSITORY_POLICY_PATH: &str = ".nopal/policy.jsonc";
const REPOSITORY_GATES_PATH: &str = ".nopal/gates.jsonc";
const PROJECT_MANIFEST_PATH: &str = ".nopal/nopal.jsonc";
const BUNDLE_PATH: &str = ".nopal/bundle.jsonc";
const LOCK_PATH: &str = ".nopal/nopal.lock";
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
    /// Ephemeral run capability used to authenticate receipts. The CLI loads
    /// it from protected run state; it is never serialized into evidence,
    /// contracts, subprocess arguments, or the project tree.
    pub receipt_key: Option<&'a [u8]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionSource {
    pub source: String,
    pub decision: Decision,
    pub decision_source: Source,
    pub placement: Placement,
    pub placement_source: Source,
    pub matched_rules: Vec<policy::MatchedRule>,
    pub explanation: Vec<String>,
}

/// Exact Pi call context compiled by the verified adapter.
///
/// Core treats every field as authorization input, not presentation metadata.
/// The adapter must mint a fresh intent per tool call and must never reuse one
/// after Pi changes the call arguments.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct EnforcementIntent {
    pub kind: String,
    pub launch_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input_digest: String,
    pub target_digest: String,
    /// Identity of the run-private gate executable mapping. The CLI gathers
    /// and revalidates it; Core only binds the typed fact.
    pub executor_digest: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub workspace_fingerprint: Option<String>,
    pub mutates: bool,
}

impl EnforcementIntent {
    pub fn validate(&self) -> Result<(), String> {
        if self.kind != ENFORCEMENT_INTENT_KIND {
            return Err(format!(
                "unsupported enforcement intent kind {:?}",
                self.kind
            ));
        }
        for (name, value) in [
            ("launch_id", self.launch_id.as_str()),
            ("session_id", self.session_id.as_str()),
            ("tool_call_id", self.tool_call_id.as_str()),
            ("tool_name", self.tool_name.as_str()),
            ("input_digest", self.input_digest.as_str()),
            ("target_digest", self.target_digest.as_str()),
            ("executor_digest", self.executor_digest.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("enforcement intent {name} must not be empty"));
            }
        }
        if self.changed_files.iter().any(|path| {
            path.is_empty()
                || Path::new(path).is_absolute()
                || path.split('/').any(|part| part == "..")
        }) {
            return Err(
                "enforcement intent changed_files must be confined repository-relative paths"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReceiptStatus {
    pub gate_id: String,
    pub current: bool,
    pub gate_definition_digest: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum EvidenceEffect {
    AppendEvent {
        event: String,
        payload: ledger_json::Value,
    },
    WriteJson {
        relative_path: PathBuf,
        payload: ledger_json::Value,
    },
    /// Create immutable evidence exactly once. The CLI adapter must use an
    /// atomic create-new write so duplicate outcomes cannot overwrite history.
    CreateJson {
        relative_path: PathBuf,
        payload: ledger_json::Value,
    },
    RemoveFile {
        relative_path: PathBuf,
        ignore_missing: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceDirective {
    pub effects: Vec<EvidenceEffect>,
}

#[derive(Debug, Clone)]
pub struct GateExecutionContext {
    pub contract_digest: String,
    pub workspace_fingerprint: String,
    pub gate_definition_digest: String,
    pub authorization_binding: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnforcementPlan {
    pub kind: &'static str,
    pub ok: bool,
    pub root: String,
    pub action: String,
    pub decision: Decision,
    pub decision_winners: Vec<String>,
    pub placement: Placement,
    pub placement_winners: Vec<String>,
    pub decisions: Vec<DecisionSource>,
    pub required_stages: Vec<String>,
    /// Gates the adapter must execute before the protected action may run.
    pub required_gates: Vec<SelectedGate>,
    pub receipts: Vec<ReceiptStatus>,
    pub contract_digest: String,
    pub workspace_fingerprint: String,
    pub authorization_binding: String,
    pub approval_current: bool,
    pub authorized: bool,
    pub intent: EnforcementIntent,
    pub diagnostics: Vec<Diagnostic>,
}

struct EffectiveContract {
    policies: Vec<(String, PolicyDoc)>,
    gate_sources: Vec<(String, GatesConfig)>,
    isolation: Option<crate::isolation::AgentIsolation>,
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
    let intent = legacy_intent(&request);
    plan_for_intent(request, intent)
}

/// Plan one exact Pi tool call.
///
/// `EnforcementIntent` is deliberately owned so the resulting plan can carry
/// the precise authorization subject back to the adapter without borrowing
/// caller buffers that may later change.
pub fn plan_for_intent(
    request: EnforcementRequest<'_>,
    intent: EnforcementIntent,
) -> io::Result<EnforcementPlan> {
    let contract = load_contract(request.root, request.config_dir)?;
    let mut diagnostics = contract.diagnostics;
    if let Err(message) = intent.validate() {
        diagnostics.push(Diagnostic::error(
            Code::ModuleParseError,
            "enforcement.intent",
            message,
        ));
    }
    if !action_classes_valid(request.action, request.classes) {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            "enforcement.intent",
            format!(
                "action {:?} does not match the adapter-declared classes [{}]",
                request.action,
                request
                    .classes
                    .iter()
                    .map(ActionClass::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    let contract_ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);

    let mut decision = Decision::Allow;
    let mut placement = Placement::SharedUserRuntime;
    let mut decisions = Vec::new();
    if contract_ok {
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
            placement = placement.max(evaluation.placement);
            decisions.push(DecisionSource {
                source: source.clone(),
                decision: evaluation.decision,
                decision_source: evaluation.decision_source,
                placement: evaluation.placement,
                placement_source: evaluation.placement_source,
                matched_rules: evaluation.matched_rules,
                explanation: evaluation.class_notes,
            });
        }
    } else {
        decision = Decision::Deny;
        placement = Placement::Blocked;
    }
    if contract.isolation.is_some() {
        // A valid `current` projection is already satisfied by the launch
        // worktree. Stronger strategies were diagnosed as unavailable while
        // loading the contract and therefore cannot reach authorization.
        placement = placement.max(Placement::SharedUserRuntime);
    }

    let mut decision_winners = decisions
        .iter()
        .filter(|source| source.decision == decision)
        .map(|source| {
            if source.decision_source == policy::Source::SafetyFloor {
                "built-in safety floor".to_owned()
            } else {
                source.source.clone()
            }
        })
        .collect::<Vec<_>>();
    decision_winners.sort();
    decision_winners.dedup();
    let mut placement_winners = decisions
        .iter()
        .filter(|source| source.placement == placement)
        .map(|source| {
            if source.placement_source == policy::Source::SafetyFloor {
                "built-in safety floor".to_owned()
            } else {
                source.source.clone()
            }
        })
        .collect::<Vec<_>>();
    placement_winners.sort();
    placement_winners.dedup();
    let stages = required_stages(request.action, request.classes);
    let selected_gates = if decision == Decision::Deny || !contract_ok {
        Vec::new()
    } else {
        select_required_gates(
            &stages,
            &intent.changed_files,
            &contract.gate_sources,
            &mut diagnostics,
        )
    };
    let contract_digest = contract.contract_digest;
    let workspace_fingerprint = match &intent.workspace_fingerprint {
        Some(fingerprint) if !fingerprint.is_empty() => fingerprint.clone(),
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                "enforcement.intent",
                "workspace_fingerprint must not be empty when supplied",
            ));
            String::new()
        }
        None => workspace_fingerprint(request.root)?,
    };
    let authorization_binding = authorization_binding(
        &request,
        &intent,
        decision,
        placement,
        &contract_digest,
        &workspace_fingerprint,
        &selected_gates,
    );
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
                        &authorization_binding,
                        receipt_key,
                    )
                })
            }),
            gate_definition_digest: gate_digest(gate),
        })
        .collect();
    let required_gates: Vec<SelectedGate> = selected_gates
        .into_iter()
        .zip(&receipts)
        .filter_map(|(gate, receipt)| (!receipt.current).then_some(gate))
        .collect();
    let approval_current = decision == Decision::Ask
        && request.run_dir.is_some_and(|run_dir| {
            request.receipt_key.is_some_and(|receipt_key| {
                approval_is_current(run_dir, request.action, &authorization_binding, receipt_key)
            })
        });
    let ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);
    let authorized = ok
        && placement != Placement::Blocked
        && required_gates.is_empty()
        && (decision == Decision::Allow || approval_current);

    Ok(EnforcementPlan {
        kind: ENFORCEMENT_PLAN_KIND,
        ok,
        root: request.root.display().to_string(),
        action: request.action.to_owned(),
        decision: if ok { decision } else { Decision::Deny },
        decision_winners,
        placement: if ok { placement } else { Placement::Blocked },
        placement_winners,
        decisions,
        required_stages: stages
            .iter()
            .map(|stage| stage.as_str().to_owned())
            .collect(),
        required_gates: if ok { required_gates } else { Vec::new() },
        receipts,
        contract_digest,
        workspace_fingerprint,
        authorization_binding,
        approval_current,
        authorized,
        intent,
        diagnostics,
    })
}

/// Return every gate definition that an active protected Pi action may select.
/// Launch uses this independent of current policy, selectors, and changed files
/// so missing stage-specific executor capability fails before Pi starts.
pub fn gate_executor_requirements(
    root: &Path,
    config_dir: Option<&Path>,
) -> io::Result<Vec<SelectedGate>> {
    let contract = load_contract(root, config_dir)?;
    if contract
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(io::Error::other(
            "cannot prepare gate executors for an invalid enforcement contract",
        ));
    }
    let mut selected: Vec<(String, SelectedGate)> = Vec::new();
    for (source, config) in contract.gate_sources {
        for gate in config.gates.into_iter().filter(|gate| {
            matches!(
                gate.stage,
                GateStage::Continuous
                    | GateStage::PerEdit
                    | GateStage::PreCommit
                    | GateStage::PrePr
            )
        }) {
            let candidate = SelectedGate {
                id: gate.id,
                stage: gate.stage,
                run: gate.run,
                cwd: gate.cwd,
                autofix: gate.autofix,
                parallel_safe: gate.parallel_safe,
                mutates: gate.mutates,
                via: selection::Via::Default,
            };
            if let Some((previous_source, previous)) = selected
                .iter()
                .find(|(_, previous)| previous.id == candidate.id)
            {
                if gate_semantics(previous) != gate_semantics(&candidate) {
                    return Err(io::Error::other(format!(
                        "gate {:?} conflicts between {previous_source} and {source}",
                        candidate.id
                    )));
                }
                continue;
            }
            selected.push((source.clone(), candidate));
        }
    }
    Ok(selected.into_iter().map(|(_, gate)| gate).collect())
}

fn legacy_intent(request: &EnforcementRequest<'_>) -> EnforcementIntent {
    let classes = request
        .classes
        .iter()
        .map(ActionClass::as_str)
        .collect::<Vec<_>>()
        .join(",");
    EnforcementIntent {
        kind: ENFORCEMENT_INTENT_KIND.to_owned(),
        launch_id: "legacy".to_owned(),
        session_id: "legacy".to_owned(),
        tool_call_id: format!("legacy:{}", request.action),
        tool_name: "legacy".to_owned(),
        input_digest: digest_bytes(format!("{}:{classes}", request.action).as_bytes()),
        target_digest: digest_bytes(request.root.as_os_str().as_encoded_bytes()),
        executor_digest: "legacy-executor".to_owned(),
        changed_files: Vec::new(),
        workspace_fingerprint: None,
        mutates: request
            .classes
            .iter()
            .any(|class| class.as_str() != "read" && class.as_str() != "network_read"),
    }
}

fn authorization_binding(
    request: &EnforcementRequest<'_>,
    intent: &EnforcementIntent,
    decision: Decision,
    placement: Placement,
    contract_digest: &str,
    workspace_fingerprint: &str,
    gates: &[SelectedGate],
) -> String {
    let value = serde_json::json!({
        "schema": ENFORCEMENT_PLAN_KIND,
        "action": request.action,
        "classes": request.classes,
        "mode": request.mode,
        "intent": intent,
        "decision": decision,
        "placement": placement,
        "contract_digest": contract_digest,
        "workspace_fingerprint": workspace_fingerprint,
        "gates": gates.iter().map(gate_digest).collect::<Vec<_>>(),
    });
    digest_bytes(&serde_json::to_vec(&value).unwrap_or_default())
}

pub fn decision_evidence(plan: &EnforcementPlan) -> io::Result<EvidenceDirective> {
    let payload = ledger_json::from_str(&serde_json::to_string(plan).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    Ok(EvidenceDirective {
        effects: vec![EvidenceEffect::AppendEvent {
            event: "action_decision".to_owned(),
            payload,
        }],
    })
}

/// Validate one adapter-executed gate attempt and return exact durable effects.
/// Core does not apply those effects; the CLI adapter owns publication.
pub fn gate_evidence(
    request: EnforcementRequest<'_>,
    gate_id: &str,
    exit_code: i32,
    expected: &GateExecutionContext,
) -> io::Result<EvidenceDirective> {
    let intent = legacy_intent(&request);
    gate_evidence_for_intent(request, intent, gate_id, exit_code, expected)
}

pub fn gate_evidence_for_intent(
    request: EnforcementRequest<'_>,
    intent: EnforcementIntent,
    gate_id: &str,
    exit_code: i32,
    expected: &GateExecutionContext,
) -> io::Result<EvidenceDirective> {
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
    let plan = plan_for_intent(validation_request, intent)?;
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
        || plan.authorization_binding != expected.authorization_binding
        || definition_digest != expected.gate_definition_digest
    {
        return Err(io::Error::other(
            "the enforcement contract, workspace, or gate definition changed during execution",
        ));
    }

    let signature = receipt_signature(
        receipt_key,
        ReceiptSignatureSubject {
            action: &plan.action,
            contract_digest: &plan.contract_digest,
            workspace_fingerprint: &plan.workspace_fingerprint,
            authorization_binding: &plan.authorization_binding,
            gate_id,
            gate_definition_digest: &definition_digest,
            exit_code,
        },
    );
    let payload = ledger_json::json!({
        "action": plan.action,
        "contract_digest": plan.contract_digest,
        "exit_code": exit_code,
        "gate_id": gate_id,
        "gate_definition_digest": definition_digest,
        "workspace_fingerprint": plan.workspace_fingerprint,
        "authorization_binding": plan.authorization_binding,
        "signature": signature,
    });
    let mut effects = vec![EvidenceEffect::AppendEvent {
        event: "gate_attempt".to_owned(),
        payload: payload.clone(),
    }];
    if exit_code == 0 {
        let relative_path = receipt_relative_path(gate_id, &plan.authorization_binding);
        effects.push(EvidenceEffect::CreateJson {
            relative_path: relative_path.clone(),
            payload: payload.clone(),
        });
        effects.push(EvidenceEffect::AppendEvent {
            event: "gate_receipt".to_owned(),
            payload: ledger_json::json!({
                "gate_id": gate_id,
                "path": relative_path.display().to_string(),
                "receipt": payload,
            }),
        });
    }
    Ok(EvidenceDirective { effects })
}

fn load_contract(root: &Path, config_dir: Option<&Path>) -> io::Result<EffectiveContract> {
    let mut policies = Vec::new();
    let mut gate_sources = Vec::new();
    let mut isolation = None;
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
    for (label, relative) in [
        ("project manifest", PROJECT_MANIFEST_PATH),
        ("distribution bundle", BUNDLE_PATH),
        ("distribution lock", LOCK_PATH),
    ] {
        if let Err(error) = read_contract_text(root, Path::new(relative), label, &mut digest) {
            diagnostics.push(Diagnostic::error(
                Code::ModuleParseError,
                relative,
                format!("could not read confined {label}: {error}"),
            ));
        }
    }

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
            if let Some(value) = compiled
                .modules
                .get("workflow")
                .and_then(|workflow| workflow.get("agent_isolation"))
            {
                let (validated_isolation, isolation_diagnostics) =
                    crate::isolation::validate(value, WORKFLOW_PATH);
                diagnostics.extend(isolation_diagnostics);
                isolation = validated_isolation;
                if let Some(contract) = &isolation {
                    if contract.orchestrator != crate::isolation::OrchestratorPlacement::Current {
                        diagnostics.push(Diagnostic::error(
                            Code::FieldInvalid,
                            WORKFLOW_PATH,
                            "agent_isolation requests orchestrator placement that this direct Pi launcher cannot prove; use current or launch from a trusted placement adapter",
                        ));
                    }
                    if contract.delegate != crate::isolation::DelegatePlacement::Sequential {
                        diagnostics.push(Diagnostic::error(
                            Code::FieldInvalid,
                            WORKFLOW_PATH,
                            "agent_isolation requests delegated mutation placement that this direct Pi launcher cannot prove; use sequential or launch from a trusted placement adapter",
                        ));
                    }
                    if !contract.runtime_profiles.is_empty() {
                        diagnostics.push(Diagnostic::error(
                            Code::FieldInvalid,
                            WORKFLOW_PATH,
                            "agent_isolation requests runtime profile capability that this direct Pi launcher cannot prove; launch from a trusted runtime-profile adapter",
                        ));
                    }
                }
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
        isolation,
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

fn action_classes_valid(action: &str, classes: &[ActionClass]) -> bool {
    let expected = match action {
        "fs.read" => Some("read"),
        "fs.write" => Some("workspace_write"),
        "file.read_credential"
        | "file.search_credential"
        | "env.dump"
        | "export.bare"
        | "echo.secret_var"
        | "network.exfil_credential"
        | "network.exfil_env" => Some("secret_bearing"),
        "git.read" | "git.add" | "git.commit" => Some("git_local"),
        "git.push" | "git.push_force" | "gh.pr_mutate" | "gh.issue_mutate" => Some("git_remote"),
        "gh.read" => Some("network_read"),
        "dependency.install" => Some("dependency_install"),
        "network.transfer" | "wrangler.mutate" | "terraform.mutate" | "kubectl.mutate"
        | "deploy.mutate" => Some("network_write"),
        "git.reset_hard"
        | "git.clean_force"
        | "rm.recursive"
        | "sudo.exec"
        | "chmod.perm_777"
        | "chown.exec"
        | "disk.raw_write"
        | "system.shutdown"
        | "find.delete"
        | "find.exec"
        | "nopal.enforcement_internal" => Some("destructive"),
        _ => None,
    };
    expected.is_some_and(|expected| classes.len() == 1 && classes[0].as_str() == expected)
}

fn required_stages(action: &str, classes: &[ActionClass]) -> Vec<GateStage> {
    let mut stages = vec![GateStage::Continuous];
    let has_class = |name: &str| classes.iter().any(|class| class.as_str() == name);
    if action == "fs.write" || has_class("workspace_write") {
        stages.push(GateStage::PerEdit);
    }
    if action == "git.commit" {
        stages.push(GateStage::PreCommit);
    }
    if matches!(action, "git.push" | "gh.pr_mutate") {
        stages.push(GateStage::PrePr);
    }
    stages
}

fn select_required_gates(
    stages: &[GateStage],
    changed_files: &[String],
    gate_sources: &[(String, GatesConfig)],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<SelectedGate> {
    let mut selected: Vec<(String, SelectedGate)> = Vec::new();
    for stage in stages {
        let selections = gate_sources
            .iter()
            .map(|(source, gates)| {
                (
                    source,
                    gates,
                    selection::select(gates, stage.clone(), changed_files),
                )
            })
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
        for (source, gates, selection) in selections {
            let generated = gates.generated_gate_ids();
            for gate in selection.selected.into_iter().filter(|gate| {
                !(selected_explicit_authority && generated.contains(gate.id.as_str()))
            }) {
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

/// Pure fallback fingerprint for callers that do not supply a CLI-captured
/// Git observation.
///
/// Production Nopal launches always provide `EnforcementIntent.workspace_fingerprint`.
/// This fallback walks project content without executing Git or any
/// other command, preserving Core's no-execution contract for fixtures and
/// embedders.
pub fn workspace_fingerprint(root: &Path) -> io::Result<String> {
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

fn approval_relative_path(authorization_binding: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("enforcement")
        .join("approvals")
        .join(format!("{authorization_binding}.json"))
}

fn approval_path(run_dir: &Path, authorization_binding: &str) -> PathBuf {
    run_dir.join(approval_relative_path(authorization_binding))
}

fn approval_signature(
    key: &[u8],
    action: &str,
    authorization_binding: &str,
    approved: bool,
) -> String {
    receipt_signature(
        key,
        ReceiptSignatureSubject {
            action,
            contract_digest: "approval",
            workspace_fingerprint: if approved { "approved" } else { "denied" },
            authorization_binding,
            gate_id: "human-approval",
            gate_definition_digest: authorization_binding,
            exit_code: i32::from(approved),
        },
    )
}

pub fn approval_evidence(
    plan: &EnforcementPlan,
    approved: bool,
    by: &str,
    receipt_key: &[u8],
) -> io::Result<EvidenceDirective> {
    if !plan.ok || plan.decision != Decision::Ask {
        return Err(io::Error::other(
            "human approval can resolve only a current policy ask",
        ));
    }
    if receipt_key.len() < 32 {
        return Err(io::Error::other(
            "recording approval requires the active adapter receipt capability",
        ));
    }
    let signature = approval_signature(
        receipt_key,
        &plan.action,
        &plan.authorization_binding,
        approved,
    );
    let payload = ledger_json::json!({
        "action": plan.action,
        "approved": approved,
        "authorization_binding": plan.authorization_binding,
        "by": crate::run_ledger::redact_text(by, crate::run_ledger::HINT_LIMIT),
        "signature": signature,
    });
    let relative_path = approval_relative_path(&plan.authorization_binding);
    let mut effects = if approved {
        vec![EvidenceEffect::WriteJson {
            relative_path,
            payload: payload.clone(),
        }]
    } else {
        vec![EvidenceEffect::RemoveFile {
            relative_path,
            ignore_missing: true,
        }]
    };
    effects.push(EvidenceEffect::AppendEvent {
        event: "action_approval".to_owned(),
        payload,
    });
    Ok(EvidenceDirective { effects })
}

fn approval_is_current(
    run_dir: &Path,
    action: &str,
    authorization_binding: &str,
    receipt_key: &[u8],
) -> bool {
    let Ok(receipt) =
        crate::run_ledger_store::read_json(&approval_path(run_dir, authorization_binding))
    else {
        return false;
    };
    let expected = approval_signature(receipt_key, action, authorization_binding, true);
    receipt.get("action").and_then(ledger_json::Value::as_str) == Some(action)
        && receipt.get("approved") == Some(&ledger_json::Value::Bool(true))
        && receipt
            .get("authorization_binding")
            .and_then(ledger_json::Value::as_str)
            == Some(authorization_binding)
        && receipt
            .get("signature")
            .and_then(ledger_json::Value::as_str)
            .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()))
}

fn release_signature(
    receipt_key: &[u8],
    action: &str,
    authorization_binding: &str,
    tool_call_id: &str,
) -> String {
    receipt_signature(
        receipt_key,
        ReceiptSignatureSubject {
            action,
            contract_digest: "authorization-release",
            workspace_fingerprint: tool_call_id,
            authorization_binding,
            gate_id: "tool-release",
            gate_definition_digest: authorization_binding,
            exit_code: 0,
        },
    )
}

/// Return the authenticated identity of the one-shot release. The adapter must
/// carry this identity until the matching tool result or shutdown interruption.
pub fn authorization_release_id(plan: &EnforcementPlan, receipt_key: &[u8]) -> io::Result<String> {
    if !plan.authorized {
        return Err(io::Error::other(
            "authorization is not current: policy, approval, gates, or placement still blocks",
        ));
    }
    if receipt_key.len() < 32 {
        return Err(io::Error::other(
            "authorization release requires the active adapter receipt capability",
        ));
    }
    Ok(release_signature(
        receipt_key,
        &plan.action,
        &plan.authorization_binding,
        &plan.intent.tool_call_id,
    ))
}

/// Consume the final exact-call authorization before Pi releases the tool.
/// An approved ask is removed first, making retries require another explicit
/// human decision even when a process crashes before the original effect.
pub fn authorization_release_evidence(
    plan: &EnforcementPlan,
    receipt_key: &[u8],
) -> io::Result<EvidenceDirective> {
    let release_id = authorization_release_id(plan, receipt_key)?;
    let payload = ledger_json::json!({
        "action": plan.action,
        "authorization_binding": plan.authorization_binding,
        "tool_call_id": plan.intent.tool_call_id,
        "release_id": release_id,
    });
    let mut effects = vec![EvidenceEffect::CreateJson {
        relative_path: PathBuf::from("artifacts")
            .join("enforcement")
            .join("releases")
            .join(format!("{release_id}.json")),
        payload: payload.clone(),
    }];
    if plan.decision == Decision::Ask {
        effects.push(EvidenceEffect::RemoveFile {
            relative_path: approval_relative_path(&plan.authorization_binding),
            ignore_missing: false,
        });
    }
    effects.push(EvidenceEffect::AppendEvent {
        event: "authorization_release".to_owned(),
        payload,
    });
    Ok(EvidenceDirective { effects })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Success,
    Error,
    Cancelled,
    Interrupted,
}

impl ToolOutcome {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "error" => Some(Self::Error),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Authenticate and publish the terminal state of one released tool call.
/// Outcome recording intentionally does not re-plan against the post-effect
/// workspace because a successful mutator is expected to change that evidence.
pub fn tool_outcome_evidence(
    action: &str,
    authorization_binding: &str,
    tool_call_id: &str,
    release_id: &str,
    outcome: ToolOutcome,
    receipt_key: &[u8],
) -> io::Result<EvidenceDirective> {
    if receipt_key.len() < 32 {
        return Err(io::Error::other(
            "tool outcome requires the active adapter receipt capability",
        ));
    }
    let expected = release_signature(receipt_key, action, authorization_binding, tool_call_id);
    if !constant_time_eq(expected.as_bytes(), release_id.as_bytes()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "tool outcome does not match the exact authorization release",
        ));
    }
    let payload = ledger_json::json!({
        "action": action,
        "authorization_binding": authorization_binding,
        "tool_call_id": tool_call_id,
        "release_id": release_id,
        "outcome": outcome.as_str(),
    });
    let relative_path = PathBuf::from("artifacts")
        .join("enforcement")
        .join("outcomes")
        .join(format!("{release_id}.json"));
    Ok(EvidenceDirective {
        effects: vec![
            EvidenceEffect::CreateJson {
                relative_path,
                payload: payload.clone(),
            },
            EvidenceEffect::AppendEvent {
                event: "tool_outcome".to_owned(),
                payload,
            },
        ],
    })
}

fn receipt_relative_path(gate_id: &str, authorization_binding: &str) -> PathBuf {
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
    let identity = digest_bytes(format!("{gate_id}\0{authorization_binding}").as_bytes());
    let identity = identity.strip_prefix("sha256:").unwrap_or(&identity);
    PathBuf::from("artifacts")
        .join("enforcement")
        .join("receipts")
        .join(safe_id)
        .join(format!("{identity}.json"))
}

fn receipt_path(run_dir: &Path, gate_id: &str, authorization_binding: &str) -> PathBuf {
    run_dir.join(receipt_relative_path(gate_id, authorization_binding))
}

fn receipt_is_current(
    run_dir: &Path,
    action: &str,
    gate: &SelectedGate,
    contract_digest: &str,
    workspace_fingerprint: &str,
    authorization_binding: &str,
    receipt_key: &[u8],
) -> bool {
    let Ok(receipt) =
        crate::run_ledger_store::read_json(&receipt_path(run_dir, &gate.id, authorization_binding))
    else {
        return false;
    };
    let definition_digest = gate_digest(gate);
    let expected_signature = receipt_signature(
        receipt_key,
        ReceiptSignatureSubject {
            action,
            contract_digest,
            workspace_fingerprint,
            authorization_binding,
            gate_id: &gate.id,
            gate_definition_digest: &definition_digest,
            exit_code: 0,
        },
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
            .get("authorization_binding")
            .and_then(ledger_json::Value::as_str)
            == Some(authorization_binding)
        && receipt
            .get("signature")
            .and_then(ledger_json::Value::as_str)
            .is_some_and(|actual| {
                constant_time_eq(actual.as_bytes(), expected_signature.as_bytes())
            })
}

struct ReceiptSignatureSubject<'a> {
    action: &'a str,
    contract_digest: &'a str,
    workspace_fingerprint: &'a str,
    authorization_binding: &'a str,
    gate_id: &'a str,
    gate_definition_digest: &'a str,
    exit_code: i32,
}

fn receipt_signature(key: &[u8], subject: ReceiptSignatureSubject<'_>) -> String {
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
        subject.action,
        subject.contract_digest,
        subject.workspace_fingerprint,
        subject.authorization_binding,
        subject.gate_id,
        subject.gate_definition_digest,
    ] {
        inner.update((component.len() as u64).to_be_bytes());
        inner.update(component.as_bytes());
    }
    inner.update(subject.exit_code.to_be_bytes());
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    format!("{:x}", outer.finalize())
}

pub fn capability_matches(expected: &[u8], provided: &[u8]) -> bool {
    constant_time_eq(expected, provided)
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
