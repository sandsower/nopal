//! One local verification transaction shared by Pi authorization and headless proof.
//!
//! Callers provide an exact intent and invocation purpose.
//! This module owns observation, planning, gate execution, durable evidence,
//! re-observation, and bounded convergence, while Core remains values-in and
//! values-out and the caller retains approval and protected-action execution.

use std::io;
use std::path::Path;

use nopal_core::enforcement::{
    self, EnforcementIntent, EnforcementPlan, EnforcementRequest, GateExecutionContext,
};
use nopal_core::policy::{ActionClass, Decision, Mode, Placement};
use serde::Serialize;

use crate::enforcement_adapter;
use crate::gate_executor::{self, GateRuntime};

const MAX_CONVERGENCE_PASSES: usize = 3;
const MAX_SELECTED_GATES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationPurpose {
    AuthorizeProtectedAction,
    EvidenceOnly,
}

pub struct VerificationRequest<'a> {
    pub root: &'a Path,
    pub config_dir: Option<&'a Path>,
    pub run_dir: &'a Path,
    pub mode: Mode,
    pub action: &'a str,
    pub classes: &'a [ActionClass],
    pub receipt_key: &'a [u8],
    pub runtime: &'a GateRuntime,
    pub intent: EnforcementIntent,
    pub purpose: VerificationPurpose,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VerificationOutcome {
    Blocked {
        reason: String,
        plan: EnforcementPlan,
    },
    ApprovalRequired {
        plan: EnforcementPlan,
    },
    Verified {
        plan: EnforcementPlan,
    },
    Released {
        release_id: String,
        plan: EnforcementPlan,
    },
}

pub fn advance(mut request: VerificationRequest<'_>) -> io::Result<VerificationOutcome> {
    if request.mode != Mode::SupervisedAuto {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "verification requires the pinned supervised_auto policy mode",
        ));
    }
    gate_executor::validate(
        request.run_dir,
        &request.runtime.digest,
        &request.runtime.runtime_digest,
    )?;

    for _pass in 0..MAX_CONVERGENCE_PASSES {
        refresh_intent(request.root, &mut request.intent)?;
        let mut plan = plan_request(&request)?;
        enforcement_adapter::apply_evidence(
            request.run_dir,
            enforcement::decision_evidence(&plan)?,
        )?;

        if !plan.ok || plan.decision == Decision::Deny || plan.placement == Placement::Blocked {
            return Ok(VerificationOutcome::Blocked {
                reason: blocked_reason(&plan),
                plan,
            });
        }
        if plan.required_gates.len() > MAX_SELECTED_GATES {
            return Err(io::Error::other(format!(
                "verification selected more than {MAX_SELECTED_GATES} gates"
            )));
        }
        if !plan.required_gates.is_empty() {
            let gates = std::mem::take(&mut plan.required_gates);
            for gate in gates {
                let execution =
                    gate_executor::execute(request.root, request.run_dir, request.runtime, &gate)?;

                // A gate may intentionally mutate through an autofix contract.
                // Bind its result only after re-observing the state it actually
                // left behind, and reject any contract or gate-definition drift.
                refresh_intent(request.root, &mut request.intent)?;
                let post_gate = plan_request(&request)?;
                let receipt = post_gate
                    .receipts
                    .iter()
                    .find(|receipt| receipt.gate_id == gate.id)
                    .ok_or_else(|| {
                        io::Error::other(format!(
                            "gate {:?} was no longer selected after execution",
                            gate.id
                        ))
                    })?;
                if post_gate.contract_digest != plan.contract_digest {
                    return Err(io::Error::other(
                        "enforcement contract changed while a gate was executing",
                    ));
                }
                let evidence = enforcement::gate_evidence_for_intent(
                    core_request(&request),
                    request.intent.clone(),
                    &gate.id,
                    execution.exit_code,
                    &GateExecutionContext {
                        contract_digest: post_gate.contract_digest.clone(),
                        workspace_fingerprint: post_gate.workspace_fingerprint.clone(),
                        gate_definition_digest: receipt.gate_definition_digest.clone(),
                        authorization_binding: post_gate.authorization_binding.clone(),
                    },
                )?;
                enforcement_adapter::apply_evidence(request.run_dir, evidence)?;
                if execution.exit_code != 0 {
                    return Ok(VerificationOutcome::Blocked {
                        reason: format!(
                            "required gate {:?} failed with exit code {}: {}",
                            gate.id,
                            execution.exit_code,
                            nopal_core::run_ledger::redact_text(
                                &execution.stderr,
                                nopal_core::run_ledger::TEXT_LIMIT,
                            )
                        ),
                        plan: post_gate,
                    });
                }
            }
            continue;
        }

        if plan.decision == Decision::Ask && !plan.approval_current {
            return Ok(VerificationOutcome::ApprovalRequired { plan });
        }
        if !plan.authorized {
            return Ok(VerificationOutcome::Blocked {
                reason: "the exact verification subject is not authorized".to_owned(),
                plan,
            });
        }
        if request.purpose == VerificationPurpose::EvidenceOnly {
            return Ok(VerificationOutcome::Verified { plan });
        }

        let release_id = enforcement::authorization_release_id(&plan, request.receipt_key)?;
        enforcement_adapter::apply_evidence(
            request.run_dir,
            enforcement::authorization_release_evidence(&plan, request.receipt_key)?,
        )?;
        return Ok(VerificationOutcome::Released { release_id, plan });
    }

    Err(io::Error::other(format!(
        "verification did not converge after {MAX_CONVERGENCE_PASSES} passes"
    )))
}

fn refresh_intent(root: &Path, intent: &mut EnforcementIntent) -> io::Result<()> {
    let workspace = enforcement_adapter::observe(root)?;
    intent.changed_files = workspace.changed_files;
    intent.workspace_fingerprint = Some(workspace.fingerprint);
    Ok(())
}

fn plan_request(request: &VerificationRequest<'_>) -> io::Result<EnforcementPlan> {
    enforcement::plan_for_intent(core_request(request), request.intent.clone())
}

fn core_request<'a>(request: &'a VerificationRequest<'_>) -> EnforcementRequest<'a> {
    EnforcementRequest {
        root: request.root,
        config_dir: request.config_dir,
        mode: request.mode,
        action: request.action,
        classes: request.classes,
        run_dir: Some(request.run_dir),
        receipt_key: Some(request.receipt_key),
    }
}

fn blocked_reason(plan: &EnforcementPlan) -> String {
    plan.diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == nopal_core::diagnostics::Severity::Error)
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| format!("policy decision is {}", plan.decision.as_str()))
}
