//! Pure reconciliation for unattended executions owned by a durable Plot.

use crate::plot::{ExecutionEvidencePointer, PlotDocument, PlotExecution};
use std::collections::BTreeSet;

const CURSOR_PREFIX: &str = "rondo.core/v1:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceInput {
    pub service_id: String,
    pub repo_id: String,
    pub run_id: String,
    pub manifest_sha256: String,
    pub status: String,
    pub event_cursor: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationInput {
    pub repo_id: String,
    pub run_id: String,
    pub status: String,
    pub event_cursor: String,
    pub evidence: Vec<ExecutionEvidencePointer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Added,
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    PlotNotEstablished,
    ExecutionNotFound,
    IdentityConflict,
    TerminalConflict,
    InvalidInput,
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PlotNotEstablished => formatter.write_str("Plot must be established"),
            Self::ExecutionNotFound => {
                formatter.write_str("execution does not belong to the selected Plot")
            }
            Self::IdentityConflict => {
                formatter.write_str("execution identity conflicts with the durable Plot record")
            }
            Self::TerminalConflict => formatter
                .write_str("execution observation conflicts with its durable terminal outcome"),
            Self::InvalidInput => formatter.write_str("execution facts are malformed"),
        }
    }
}

impl std::error::Error for ExecutionError {}

pub fn apply_acceptance(
    plot: &mut PlotDocument,
    input: AcceptanceInput,
    now: &str,
) -> Result<ApplyOutcome, ExecutionError> {
    if plot.provisional || plot.establishment.is_none() {
        return Err(ExecutionError::PlotNotEstablished);
    }
    validate_acceptance(&input)?;

    if let Some(index) = plot.executions.iter().position(|execution| {
        execution.repo_id == input.repo_id && execution.run_id == input.run_id
    }) {
        let existing = &plot.executions[index];
        if existing.service_id != input.service_id
            || existing.manifest_sha256 != input.manifest_sha256
        {
            return Err(ExecutionError::IdentityConflict);
        }
        let existing_offset =
            cursor_position(&existing.event_cursor).ok_or(ExecutionError::InvalidInput)?;
        let input_offset =
            cursor_position(&input.event_cursor).ok_or(ExecutionError::InvalidInput)?;
        let stale_nonterminal_replay = terminal_status(&existing.status)
            && !terminal_status(&input.status)
            && existing.status != input.status;
        if stale_nonterminal_replay && input_offset > existing_offset {
            return Err(ExecutionError::TerminalConflict);
        }
        if !stale_nonterminal_replay {
            validate_status_transition(&existing.status, &input.status)?;
        }
        let status_changed = !stale_nonterminal_replay && existing.status != input.status;
        let cursor_advanced = input_offset > existing_offset;
        if !status_changed && !cursor_advanced {
            return Ok(ApplyOutcome::Unchanged);
        }
        let existing = &mut plot.executions[index];
        if status_changed {
            existing.status = input.status;
            existing.outcome = terminal_outcome(&existing.status);
        }
        if cursor_advanced {
            existing.event_cursor = input.event_cursor;
        }
        existing.updated_at = now.to_owned();
        plot.updated_at = now.to_owned();
        return Ok(ApplyOutcome::Updated);
    }

    let outcome = terminal_outcome(&input.status);
    plot.executions.push(PlotExecution {
        service_id: input.service_id,
        repo_id: input.repo_id,
        run_id: input.run_id,
        manifest_sha256: input.manifest_sha256,
        status: input.status,
        outcome,
        event_cursor: input.event_cursor,
        evidence: Vec::new(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    });
    plot.executions.sort_by(|left, right| {
        left.repo_id
            .cmp(&right.repo_id)
            .then(left.run_id.cmp(&right.run_id))
    });
    plot.updated_at = now.to_owned();
    Ok(ApplyOutcome::Added)
}

pub fn apply_observation(
    plot: &mut PlotDocument,
    input: ObservationInput,
    now: &str,
) -> Result<ApplyOutcome, ExecutionError> {
    if plot.provisional || plot.establishment.is_none() {
        return Err(ExecutionError::PlotNotEstablished);
    }
    validate_observation(&input)?;
    let index = plot
        .executions
        .iter()
        .position(|execution| {
            execution.repo_id == input.repo_id && execution.run_id == input.run_id
        })
        .ok_or(ExecutionError::ExecutionNotFound)?;
    let existing = &plot.executions[index];
    validate_status_transition(&existing.status, &input.status)?;
    let existing_offset =
        cursor_position(&existing.event_cursor).ok_or(ExecutionError::InvalidInput)?;
    let input_offset = cursor_position(&input.event_cursor).ok_or(ExecutionError::InvalidInput)?;
    let status_changed = existing.status != input.status;
    let cursor_advanced = input_offset > existing_offset;
    let new_evidence: Vec<_> = input
        .evidence
        .into_iter()
        .filter(|pointer| !existing.evidence.contains(pointer))
        .collect();
    if !status_changed && !cursor_advanced && new_evidence.is_empty() {
        return Ok(ApplyOutcome::Unchanged);
    }

    let existing = &mut plot.executions[index];
    if status_changed {
        existing.status = input.status;
        existing.outcome = terminal_outcome(&existing.status);
    }
    if cursor_advanced {
        existing.event_cursor = input.event_cursor;
    }
    for pointer in new_evidence {
        if !existing.evidence.contains(&pointer) {
            existing.evidence.push(pointer);
        }
    }
    existing.updated_at = now.to_owned();
    plot.updated_at = now.to_owned();
    Ok(ApplyOutcome::Updated)
}

/// Return the append-only position encoded in a Rondo Core cursor.
///
/// This is intentionally the only interpretation Nopal assigns to the opaque
/// token. Callers can compare replay positions without accepting another
/// surface, an oversized integer, or a malformed value.
pub fn cursor_position(value: &str) -> Option<u128> {
    let digits = value.strip_prefix(CURSOR_PREFIX)?;
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Validate execution facts reconstructed from a durable Plot snapshot.
///
/// Store reads use this before exposing a Plot so malformed, duplicated, or
/// internally contradictory execution facts degrade as an unreadable snapshot
/// instead of becoming trusted correlation state.
pub fn validate_plot_snapshot(plot: &PlotDocument) -> Result<(), ExecutionError> {
    if plot.fruit.state != "absent" {
        return Err(ExecutionError::InvalidInput);
    }
    if !plot.executions.is_empty() && (plot.provisional || plot.establishment.is_none()) {
        return Err(ExecutionError::PlotNotEstablished);
    }

    let mut identities = BTreeSet::new();
    for execution in &plot.executions {
        validate_acceptance(&AcceptanceInput {
            service_id: execution.service_id.clone(),
            repo_id: execution.repo_id.clone(),
            run_id: execution.run_id.clone(),
            manifest_sha256: execution.manifest_sha256.clone(),
            status: execution.status.clone(),
            event_cursor: execution.event_cursor.clone(),
        })?;
        if execution.outcome != terminal_outcome(&execution.status)
            || !valid_bounded_text(&execution.created_at, 128)
            || !valid_bounded_text(&execution.updated_at, 128)
            || execution
                .evidence
                .iter()
                .any(|pointer| !valid_evidence_pointer(pointer))
        {
            return Err(ExecutionError::InvalidInput);
        }
        if !identities.insert((&execution.repo_id, &execution.run_id)) {
            return Err(ExecutionError::IdentityConflict);
        }
        let mut evidence = BTreeSet::new();
        if execution
            .evidence
            .iter()
            .any(|pointer| !evidence.insert((&pointer.artifact_kind, &pointer.uri)))
        {
            return Err(ExecutionError::IdentityConflict);
        }
    }
    Ok(())
}

fn validate_acceptance(input: &AcceptanceInput) -> Result<(), ExecutionError> {
    if !valid_opaque_identifier(&input.service_id)
        || !valid_opaque_identifier(&input.repo_id)
        || !valid_opaque_identifier(&input.run_id)
        || !valid_status(&input.status)
        || !valid_digest(&input.manifest_sha256)
        || cursor_position(&input.event_cursor).is_none()
    {
        return Err(ExecutionError::InvalidInput);
    }
    Ok(())
}

fn validate_observation(input: &ObservationInput) -> Result<(), ExecutionError> {
    if !valid_opaque_identifier(&input.repo_id)
        || !valid_opaque_identifier(&input.run_id)
        || !valid_status(&input.status)
        || cursor_position(&input.event_cursor).is_none()
        || input
            .evidence
            .iter()
            .any(|pointer| !valid_evidence_pointer(pointer))
    {
        return Err(ExecutionError::InvalidInput);
    }
    Ok(())
}

fn valid_opaque_identifier(value: &str) -> bool {
    valid_bounded_text(value, 512)
}

fn valid_status(value: &str) -> bool {
    matches!(
        value,
        "running" | "paused" | "completed" | "failed" | "terminated"
    )
}

fn valid_evidence_pointer(pointer: &ExecutionEvidencePointer) -> bool {
    valid_bounded_text(&pointer.artifact_kind, 1_024)
        && valid_bounded_text(&pointer.uri, 2_048)
        && pointer.uri.starts_with("rondo-run://")
}

fn valid_bounded_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_status_transition(current: &str, observed: &str) -> Result<(), ExecutionError> {
    if terminal_status(current) && current != observed {
        return Err(ExecutionError::TerminalConflict);
    }
    Ok(())
}

fn terminal_outcome(status: &str) -> Option<String> {
    matches!(status, "completed" | "failed" | "terminated").then(|| status.to_owned())
}

fn terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "terminated" | "paused")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::{ExecutionEvidencePointer, PlotDocument};

    fn plot() -> PlotDocument {
        serde_json::from_value(serde_json::json!({
            "kind": "nopal.plot/v1",
            "plot_id": "plot-1",
            "title": "Execution Plot",
            "provisional": false,
            "progress": "planned",
            "conditions": ["keep-condition"],
            "seed": {"source": "test", "text": "seed"},
            "intent": "Exercise unattended work",
            "sessions": [],
            "selected_session_id": null,
            "establishment": {
                "event": "kickoff_context_ready",
                "primary_repository_id": "repo-1",
                "effective_workflow": {
                    "source_repository_id": "repo-1",
                    "source_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "value": {}
                },
                "applied_requests": [],
                "established_at": "t0"
            },
            "repositories": [{
                "repository_id": "repo-1",
                "root": "/repo",
                "configuration_root": "/repo",
                "revision": "abc",
                "process_artifact_hash": "process-1",
                "roots": [{
                    "id": "quality",
                    "statement": "Quality remains green",
                    "proof_requirements": [{
                        "id": "pre-pr",
                        "stage": "pre_pr",
                        "required": true,
                        "gates": ["test"],
                        "on_missing": "block",
                        "on_failure": "block"
                    }]
                }],
                "gate_ids": ["test"],
                "policy_hash": "policy-1"
            }],
            "created_at": "t0",
            "updated_at": "t0"
        }))
        .unwrap()
    }

    fn acceptance(run_id: &str) -> AcceptanceInput {
        AcceptanceInput {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-1".to_owned(),
            run_id: run_id.to_owned(),
            manifest_sha256: "a".repeat(64),
            status: "running".to_owned(),
            event_cursor: "rondo.core/v1:0".to_owned(),
        }
    }

    fn observation(status: &str, cursor: &str) -> ObservationInput {
        ObservationInput {
            repo_id: "repo-1".to_owned(),
            run_id: "run-1".to_owned(),
            status: status.to_owned(),
            event_cursor: cursor.to_owned(),
            evidence: vec![ExecutionEvidencePointer {
                artifact_kind: "delivery_artifact".to_owned(),
                uri: "rondo-run://run-1/artifacts/delivery.json".to_owned(),
            }],
        }
    }

    #[test]
    fn old_plot_documents_default_to_no_executions() {
        let plot = plot();

        assert!(plot.executions.is_empty());
        assert_eq!(plot.fruit.state, "absent");
    }

    #[test]
    fn plot_snapshots_reject_unauthorized_fruit_states() {
        let mut plot = plot();
        plot.fruit.state = "accepted".to_owned();

        assert_eq!(
            validate_plot_snapshot(&plot),
            Err(ExecutionError::InvalidInput)
        );
    }

    #[test]
    fn acceptance_is_idempotent_and_does_not_mutate_assurance_facts() {
        let mut plot = plot();
        let before_progress = plot.progress.clone();
        let before_conditions = plot.conditions.clone();
        let before_fruit = plot.fruit.clone();
        let before_repositories = plot.repositories.clone();

        assert_eq!(
            apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap(),
            ApplyOutcome::Added
        );
        let once = plot.clone();
        assert_eq!(
            apply_acceptance(&mut plot, acceptance("run-1"), "t2").unwrap(),
            ApplyOutcome::Unchanged
        );

        assert_eq!(plot, once);
        assert_eq!(plot.progress, before_progress);
        assert_eq!(plot.conditions, before_conditions);
        assert_eq!(plot.fruit, before_fruit);
        assert_eq!(plot.repositories, before_repositories);
        assert_eq!(plot.executions.len(), 1);
        assert_eq!(plot.executions[0].outcome, None);
    }

    #[test]
    fn stale_exact_acceptance_replay_reuses_a_terminal_execution() {
        let mut plot = plot();
        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        apply_observation(&mut plot, observation("completed", "rondo.core/v1:3"), "t2").unwrap();
        let terminal = plot.clone();

        assert_eq!(
            apply_acceptance(&mut plot, acceptance("run-1"), "t3").unwrap(),
            ApplyOutcome::Unchanged
        );
        assert_eq!(plot, terminal);

        let mut conflicting_terminal = acceptance("run-1");
        conflicting_terminal.status = "failed".to_owned();
        assert_eq!(
            apply_acceptance(&mut plot, conflicting_terminal, "t3"),
            Err(ExecutionError::TerminalConflict)
        );
        assert_eq!(plot, terminal);
    }

    #[test]
    fn acceptance_rejects_an_immutable_identity_conflict_atomically() {
        let mut plot = plot();
        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        let before = plot.clone();
        let mut conflict = acceptance("run-1");
        conflict.manifest_sha256 = "b".repeat(64);

        assert_eq!(
            apply_acceptance(&mut plot, conflict, "t2"),
            Err(ExecutionError::IdentityConflict)
        );
        assert_eq!(plot, before);
    }

    #[test]
    fn observation_advances_status_cursor_and_deduplicates_evidence() {
        let mut plot = plot();
        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        let before_progress = plot.progress.clone();
        let before_conditions = plot.conditions.clone();
        let before_fruit = plot.fruit.clone();
        let before_repositories = plot.repositories.clone();

        assert_eq!(
            apply_observation(&mut plot, observation("completed", "rondo.core/v1:3"), "t2")
                .unwrap(),
            ApplyOutcome::Updated
        );
        assert_eq!(
            apply_observation(&mut plot, observation("completed", "rondo.core/v1:2"), "t3")
                .unwrap(),
            ApplyOutcome::Unchanged
        );

        let execution = &plot.executions[0];
        assert_eq!(execution.status, "completed");
        assert_eq!(execution.outcome.as_deref(), Some("completed"));
        assert_eq!(execution.event_cursor, "rondo.core/v1:3");
        assert_eq!(execution.evidence.len(), 1);
        assert_eq!(execution.updated_at, "t2");
        assert_eq!(plot.progress, before_progress);
        assert_eq!(plot.conditions, before_conditions);
        assert_eq!(plot.fruit, before_fruit);
        assert_eq!(plot.repositories, before_repositories);
    }

    #[test]
    fn terminal_observation_cannot_regress_or_change_outcome() {
        let mut plot = plot();
        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        apply_observation(&mut plot, observation("completed", "rondo.core/v1:3"), "t2").unwrap();
        let before = plot.clone();

        assert_eq!(
            apply_observation(&mut plot, observation("running", "rondo.core/v1:4"), "t3"),
            Err(ExecutionError::TerminalConflict)
        );
        assert_eq!(plot, before);

        assert_eq!(
            apply_observation(&mut plot, observation("failed", "rondo.core/v1:4"), "t3"),
            Err(ExecutionError::TerminalConflict)
        );
        assert_eq!(plot, before);
    }

    #[test]
    fn unknown_and_foreign_observations_are_rejected_atomically() {
        let mut plot = plot();
        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        let before = plot.clone();

        let mut unknown = observation("running", "rondo.core/v1:0");
        unknown.run_id = "run-unknown".to_owned();
        assert_eq!(
            apply_observation(&mut plot, unknown, "t2"),
            Err(ExecutionError::ExecutionNotFound)
        );
        assert_eq!(plot, before);

        let mut foreign = observation("running", "rondo.core/v1:0");
        foreign.repo_id = "repo-foreign".to_owned();
        assert_eq!(
            apply_observation(&mut plot, foreign, "t2"),
            Err(ExecutionError::ExecutionNotFound)
        );
        assert_eq!(plot, before);
    }

    #[test]
    fn malformed_acceptance_and_observation_are_rejected_before_mutation() {
        let mut plot = plot();
        let before = plot.clone();
        let mut malformed = acceptance("run-1");
        malformed.event_cursor = "foreign:0".to_owned();

        assert_eq!(
            apply_acceptance(&mut plot, malformed, "t1"),
            Err(ExecutionError::InvalidInput)
        );
        assert_eq!(plot, before);

        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        let before = plot.clone();
        let mut malformed = observation("running", "rondo.core/v1:1");
        malformed.evidence[0].uri.clear();
        assert_eq!(
            apply_observation(&mut plot, malformed, "t2"),
            Err(ExecutionError::InvalidInput)
        );
        assert_eq!(plot, before);
    }

    #[test]
    fn cursor_position_accepts_only_the_bounded_rondo_core_token() {
        assert_eq!(cursor_position("rondo.core/v1:0"), Some(0));
        assert_eq!(
            cursor_position("rondo.core/v1:18446744073709551615"),
            Some(18_446_744_073_709_551_615)
        );
        assert_eq!(cursor_position("rondo.core/v1:0004"), Some(4));
        assert_eq!(cursor_position("rondo.core/v1:"), None);
        assert_eq!(cursor_position("foreign:4"), None);
        assert_eq!(cursor_position("rondo.core/v1:184467440737095516150"), None);
        assert_eq!(cursor_position("rondo.core/v1:-1"), None);
    }

    #[test]
    fn paused_is_a_closed_execution_state_but_not_an_outcome() {
        let mut plot = plot();
        let mut paused = acceptance("run-1");
        paused.status = "paused".to_owned();

        apply_acceptance(&mut plot, paused, "t1").unwrap();
        assert_eq!(plot.executions[0].status, "paused");
        assert_eq!(plot.executions[0].outcome, None);
        let before = plot.clone();
        assert_eq!(
            apply_observation(&mut plot, observation("running", "rondo.core/v1:1"), "t2"),
            Err(ExecutionError::TerminalConflict)
        );
        assert_eq!(plot, before);
    }

    #[test]
    fn unknown_status_and_non_rondo_evidence_are_rejected_atomically() {
        let mut plot = plot();
        let before = plot.clone();
        let mut unknown = acceptance("run-1");
        unknown.status = "successful".to_owned();
        assert_eq!(
            apply_acceptance(&mut plot, unknown, "t1"),
            Err(ExecutionError::InvalidInput)
        );
        assert_eq!(plot, before);

        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        let before = plot.clone();
        let mut foreign = observation("running", "rondo.core/v1:1");
        foreign.evidence[0].uri = "file:///private/ledger.json".to_owned();
        assert_eq!(
            apply_observation(&mut plot, foreign, "t2"),
            Err(ExecutionError::InvalidInput)
        );
        assert_eq!(plot, before);
    }

    #[test]
    fn snapshot_validation_rejects_duplicate_and_contradictory_facts() {
        let mut plot = plot();
        apply_acceptance(&mut plot, acceptance("run-1"), "t1").unwrap();
        assert_eq!(validate_plot_snapshot(&plot), Ok(()));

        let mut duplicate = plot.clone();
        duplicate.executions.push(duplicate.executions[0].clone());
        assert_eq!(
            validate_plot_snapshot(&duplicate),
            Err(ExecutionError::IdentityConflict)
        );

        let mut contradictory = plot.clone();
        contradictory.executions[0].outcome = Some("completed".to_owned());
        assert_eq!(
            validate_plot_snapshot(&contradictory),
            Err(ExecutionError::InvalidInput)
        );
    }
}
