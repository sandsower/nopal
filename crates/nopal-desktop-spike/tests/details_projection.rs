#![allow(dead_code)]

#[path = "../src/details.rs"]
mod details;
#[path = "../src/model.rs"]
mod model;

use std::collections::{BTreeMap, BTreeSet};

use details::{
    AssuranceFact, AssuranceKey, DetailsProjection, DetailsUiState, ObservedRunIdentity,
    ProjectionError,
};
use model::{
    DesktopApproval, DesktopAssuranceExecution, DesktopAssuranceModel, DesktopAssuranceRepository,
    DesktopAssuranceSession, DesktopDiagnostic, DesktopEvidence, DesktopGateAttempt,
    DesktopObservedRondo, DesktopObservedRun, DesktopPlotAssurance, DesktopProofRequirement,
    DesktopRoot, DesktopRunPlacement, ObservedRondoState, ObservedRunState, ProofPolicy,
    RequirementLevel, SessionState,
};
use nopal_feed_client::field::{
    ApprovalState, DiagnosticSeverity, ExecutionOutcome, ExecutionState, FruitState,
    GateAttemptState, ProgressState,
};

fn approval(id: &str, session_id: &str, state: ApprovalState) -> DesktopApproval {
    DesktopApproval {
        ask_id: id.to_owned(),
        canonical: true,
        action: "git.push".to_owned(),
        reason: "publish".to_owned(),
        session_id: session_id.to_owned(),
        repo: "/repo".to_owned(),
        state,
        created_at: "t0".to_owned(),
        expires_at: "t1".to_owned(),
        run_id: Some("ledger-a".to_owned()),
        source_run_id: Some("ledger-a".to_owned()),
    }
}

fn model() -> DesktopAssuranceModel {
    DesktopAssuranceModel {
        plots: vec![DesktopPlotAssurance {
            plot_id: "plot-a".to_owned(),
            progress: ProgressState::Active,
            conditions: vec!["keep condition".to_owned()],
            fruit: FruitState::Absent,
            sessions: vec![DesktopAssuranceSession {
                session_id: "session-a".to_owned(),
                mode: "interactive".to_owned(),
                host: "pi".to_owned(),
                host_session: "nopal-work".to_owned(),
                host_pane: Some("%7".to_owned()),
                state: SessionState::Active,
                protocol: None,
                workspace: Some("/repo".to_owned()),
                created_at: "t0".to_owned(),
                updated_at: "t1".to_owned(),
                extra: BTreeMap::new(),
            }],
            executions: vec![DesktopAssuranceExecution {
                service_id: "rondo-core".to_owned(),
                repo_id: "repo-a".to_owned(),
                run_id: "execution-a".to_owned(),
                manifest_sha256: "abc".to_owned(),
                status: ExecutionState::Completed,
                outcome: Some(ExecutionOutcome::Completed),
                event_cursor: "rondo.core/v1:3".to_owned(),
                evidence: vec![DesktopEvidence {
                    artifact_kind: "delivery".to_owned(),
                    uri: "rondo://execution-a/delivery".to_owned(),
                }],
                created_at: "t0".to_owned(),
                updated_at: "t1".to_owned(),
            }],
            repositories: vec![DesktopAssuranceRepository {
                repository_id: "repo-a".to_owned(),
                root: "/repo".to_owned(),
                configuration_root: "/repo".to_owned(),
                revision: Some("abc123".to_owned()),
                roots: vec![DesktopRoot {
                    id: "quality".to_owned(),
                    statement: "Quality remains green".to_owned(),
                    proof_requirements: vec![DesktopProofRequirement {
                        id: "pre-pr".to_owned(),
                        stage: "pre_pr".to_owned(),
                        required: RequirementLevel::Required,
                        gates: vec!["test".to_owned()],
                        on_missing: ProofPolicy::Block,
                        on_failure: ProofPolicy::Block,
                    }],
                }],
                gate_ids: vec!["test".to_owned()],
            }],
        }],
        observed_runs: vec![DesktopObservedRun {
            run_id: "ledger-a".to_owned(),
            flow: "implement".to_owned(),
            skill: "implement".to_owned(),
            status: ObservedRunState::Running,
            ticket_id: "TASK-57".to_owned(),
            branch: "codex/task-57".to_owned(),
            started_at: "t0".to_owned(),
            updated_at: "t1".to_owned(),
            placement: DesktopRunPlacement {
                repo: "/repo".to_owned(),
                repo_hash: "repo-hash".to_owned(),
                branch: "codex/task-57".to_owned(),
                run_dir: "/state/ledger-a".to_owned(),
                flow: "implement".to_owned(),
            },
            stale: false,
            gates: vec![DesktopGateAttempt {
                run_id: "ledger-a".to_owned(),
                attempt: Some(1),
                name: "test".to_owned(),
                scope: "repo".to_owned(),
                status: GateAttemptState::Pass,
                classification: "code_pass".to_owned(),
                path: "/state/gates/test.json".to_owned(),
            }],
            rondo: Some(DesktopObservedRondo {
                run_id: "ledger-a".to_owned(),
                status: Some(ObservedRondoState::Completed),
                evidence: vec![DesktopEvidence {
                    artifact_kind: "run-log".to_owned(),
                    uri: "rondo://ledger-a/log".to_owned(),
                }],
            }),
            approvals: vec![approval(
                "ask-attached",
                "session-a",
                ApprovalState::Approved,
            )],
        }],
        unbound_approvals: vec![approval(
            "ask-unbound",
            "missing-session",
            ApprovalState::Pending,
        )],
        diagnostics: vec![DesktopDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: "field_partial_coverage".to_owned(),
            path: "field".to_owned(),
            position: None,
            message: "partial".to_owned(),
        }],
    }
}

#[test]
fn keeps_assurance_lanes_distinct_and_correlates_only_exact_session_identity() {
    let projection = DetailsProjection::project(&model(), &DetailsUiState::default()).unwrap();
    let plot = projection.plot("plot-a").unwrap();

    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Progress {
            state: ProgressState::Active,
            ..
        }
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Fruit {
            state: FruitState::Absent,
            ..
        }
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::RequiredGateDeclaration { gate_id, .. } if gate_id == "test"
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::DeclaredGate { gate_id, .. } if gate_id == "test"
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Session { session, .. }
            if session.host == "pi" && session.host_pane.as_deref() == Some("%7")
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Repository { repository, .. }
            if repository.root == "/repo" && repository.revision.as_deref() == Some("abc123")
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Root { statement, .. } if statement == "Quality remains green"
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::ProofRequirement {
            required: RequirementLevel::Required,
            on_missing: ProofPolicy::Block,
            ..
        }
    )));
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Execution {
            manifest_sha256,
            status: ExecutionState::Completed,
            ..
        } if manifest_sha256 == "abc"
    )));
    assert!(
        plot.facts
            .iter()
            .any(|fact| matches!(fact, AssuranceFact::Evidence { .. }))
    );
    assert!(plot.facts.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Approval { approval, attached_session: Some(session), .. }
            if approval.ask_id == "ask-attached" && session.session_id == "session-a"
    )));
    assert!(
        !plot
            .facts
            .iter()
            .any(|fact| matches!(fact, AssuranceFact::ObservedGateAttempt { .. }))
    );
    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Approval { approval, attached_session: None, .. }
            if approval.ask_id == "ask-unbound"
    )));
    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::ObservedRun { run, .. }
            if run.run_id == "ledger-a"
                && run.placement.repo_hash == "repo-hash"
                && run.status == ObservedRunState::Running
    )));
    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::ObservedRunEvidence { evidence, .. }
            if evidence.uri == "rondo://ledger-a/log"
    )));
    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::ObservedGateAttempt { attempt, .. }
            if attempt.run_id == "ledger-a"
    )));
    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Diagnostic { diagnostic, .. }
            if diagnostic.code == "field_partial_coverage"
    )));
}

#[test]
fn ambiguous_session_identity_and_noncanonical_asks_stay_unbound() {
    let mut input = model();
    input.plots.push(DesktopPlotAssurance {
        plot_id: "plot-b".to_owned(),
        progress: ProgressState::Planned,
        conditions: vec![],
        fruit: FruitState::Absent,
        sessions: vec![DesktopAssuranceSession {
            session_id: "session-a".to_owned(),
            mode: "interactive".to_owned(),
            host: "pi".to_owned(),
            host_session: "nopal-work".to_owned(),
            host_pane: Some("%8".to_owned()),
            state: SessionState::Idle,
            protocol: None,
            workspace: None,
            created_at: "t0".to_owned(),
            updated_at: "t1".to_owned(),
            extra: BTreeMap::new(),
        }],
        executions: vec![],
        repositories: vec![],
    });
    input.unbound_approvals[0].canonical = false;
    input.unbound_approvals[0].session_id = "session-a".to_owned();

    let projection = DetailsProjection::project(&input, &DetailsUiState::default()).unwrap();

    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Approval { approval, attached_session: None, .. }
            if approval.ask_id == "ask-attached"
    )));
    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Approval { approval, attached_session: None, .. }
            if approval.ask_id == "ask-unbound" && !approval.is_approved()
    )));
}

#[test]
fn conflicting_approval_run_identity_stays_unbound() {
    let mut input = model();
    input.observed_runs[0].approvals[0].run_id = Some("different-run".to_owned());

    let projection = DetailsProjection::project(&input, &DetailsUiState::default()).unwrap();

    assert!(projection.unbound.iter().any(|fact| matches!(
        fact,
        AssuranceFact::Approval { approval, attached_session: None, .. }
            if approval.ask_id == "ask-attached"
                && approval.source_run_id.as_deref() == Some("ledger-a")
                && approval.run_id.as_deref() == Some("different-run")
    )));
}

#[test]
fn duplicate_exact_source_keys_fail_instead_of_overwriting_facts() {
    let mut input = model();
    input
        .unbound_approvals
        .push(approval("ask-attached", "session-a", ApprovalState::Denied));

    assert!(matches!(
        DetailsProjection::project(&input, &DetailsUiState::default()),
        Err(ProjectionError::DuplicateKey(key))
            if matches!(key.as_ref(), AssuranceKey::Approval { ask_id }
                if ask_id == "ask-attached")
    ));
}

#[test]
fn repeated_run_ids_in_distinct_canonical_placements_remain_distinct() {
    let mut input = model();
    let mut second = input.observed_runs[0].clone();
    second.flow = "review".to_owned();
    second.placement.flow = "review".to_owned();
    second.placement.repo = "/other-repo".to_owned();
    second.placement.repo_hash = "other-repo-hash".to_owned();
    second.placement.run_dir = "/state/review/other-repo-hash/ledger-a".to_owned();
    second.approvals.clear();
    input.observed_runs.push(second);

    let projection = DetailsProjection::project(&input, &DetailsUiState::default())
        .expect("flow/repo placement is part of a run's canonical identity");
    let expected_sources = BTreeSet::from([
        ObservedRunIdentity {
            flow: "implement".to_owned(),
            repo_hash: "repo-hash".to_owned(),
            run_id: "ledger-a".to_owned(),
        },
        ObservedRunIdentity {
            flow: "review".to_owned(),
            repo_hash: "other-repo-hash".to_owned(),
            run_id: "ledger-a".to_owned(),
        },
    ]);

    for sources in [
        projection
            .unbound
            .iter()
            .filter_map(|fact| match fact.key() {
                AssuranceKey::ObservedRun { source } => Some(source.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        projection
            .unbound
            .iter()
            .filter_map(|fact| match fact.key() {
                AssuranceKey::ObservedRunEvidence { source, .. } => Some(source.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        projection
            .unbound
            .iter()
            .filter_map(|fact| match fact.key() {
                AssuranceKey::ObservedGateAttempt {
                    source,
                    gate_run_id,
                    ..
                } if gate_run_id == "ledger-a" => Some(source.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
    ] {
        assert_eq!(sources, expected_sources);
    }
}

fn assert_condition_selection_survives_refresh(refreshed_conditions: Vec<String>) {
    let mut input = model();
    input.plots[0].conditions = vec!["keep quality".to_owned(), "ship safely".to_owned()];
    let initial = match DetailsProjection::project(&input, &DetailsUiState::default()) {
        Ok(initial) => initial,
        Err(error) => panic!("initial Condition projection failed: {error:?}"),
    };
    let selected = initial.plots[0].facts.iter().find_map(|fact| match fact {
        AssuranceFact::Condition { key, text } if text == "ship safely" => Some(key.clone()),
        _ => None,
    });
    let Some(selected) = selected else {
        panic!("initial Condition projection omitted the selected fact")
    };
    let previous = DetailsUiState {
        selected_by_plot: BTreeMap::from([("plot-a".to_owned(), selected.clone())]),
        expanded: BTreeSet::from([selected.clone()]),
    };
    input.plots[0].conditions = refreshed_conditions;

    let refreshed = match DetailsProjection::project(&input, &previous) {
        Ok(refreshed) => refreshed,
        Err(error) => panic!("refreshed Condition projection failed: {error:?}"),
    };
    let reconciled = &refreshed.ui.selected_by_plot["plot-a"];
    let selected_text = refreshed.plots[0].facts.iter().find_map(|fact| match fact {
        AssuranceFact::Condition { key, text } if key == reconciled => Some(text.as_str()),
        _ => None,
    });

    assert_eq!(selected_text, Some("ship safely"));
    assert!(matches!(
        reconciled,
        AssuranceKey::Condition {
            text,
            occurrence: 0,
            ..
        } if text == "ship safely"
    ));
    assert!(refreshed.ui.expanded.contains(reconciled));
}

#[test]
fn condition_selection_does_not_migrate_after_an_earlier_insertion() {
    assert_condition_selection_survives_refresh(vec![
        "new condition".to_owned(),
        "keep quality".to_owned(),
        "ship safely".to_owned(),
    ]);
}

#[test]
fn condition_selection_does_not_migrate_after_reordering() {
    assert_condition_selection_survives_refresh(vec![
        "ship safely".to_owned(),
        "keep quality".to_owned(),
    ]);
}

#[test]
fn selection_and_expansion_reconcile_by_exact_key_with_deterministic_fallback() {
    let evidence = AssuranceKey::Evidence {
        plot_id: "plot-a".to_owned(),
        service_id: "rondo-core".to_owned(),
        repo_id: "repo-a".to_owned(),
        run_id: "execution-a".to_owned(),
        artifact_kind: "delivery".to_owned(),
        uri: "rondo://execution-a/delivery".to_owned(),
    };
    let vanished = AssuranceKey::Condition {
        plot_id: "plot-a".to_owned(),
        text: "vanished".to_owned(),
        occurrence: 0,
    };
    let previous = DetailsUiState {
        selected_by_plot: BTreeMap::from([("plot-a".to_owned(), evidence.clone())]),
        expanded: BTreeSet::from([evidence.clone(), vanished]),
    };

    let projection = DetailsProjection::project(&model(), &previous).unwrap();
    assert_eq!(projection.ui.selected_by_plot["plot-a"], evidence);
    assert_eq!(projection.ui.expanded, BTreeSet::from([evidence.clone()]));

    let mut changed = model();
    changed.plots[0].executions[0].evidence.clear();
    let projection = DetailsProjection::project(&changed, &projection.ui).unwrap();
    assert_eq!(
        projection.ui.selected_by_plot["plot-a"],
        AssuranceKey::Progress {
            plot_id: "plot-a".to_owned()
        }
    );
    assert!(projection.ui.expanded.is_empty());
}

#[test]
fn diagnostic_keys_survive_refresh_reordering_and_exact_duplicates_fail_closed() {
    let mut input = model();
    input.diagnostics.push(DesktopDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "field_rondo_feed_absent".to_owned(),
        path: "rondo".to_owned(),
        position: Some((4, 2)),
        message: "feed absent".to_owned(),
    });
    let diagnostic_key = AssuranceKey::Diagnostic {
        code: "field_rondo_feed_absent".to_owned(),
        path: "rondo".to_owned(),
        position: Some((4, 2)),
    };
    let previous = DetailsUiState {
        selected_by_plot: BTreeMap::new(),
        expanded: BTreeSet::from([diagnostic_key.clone()]),
    };
    input.diagnostics.reverse();

    let projection = DetailsProjection::project(&input, &previous).unwrap();

    assert!(projection.ui.expanded.contains(&diagnostic_key));

    input.diagnostics.push(input.diagnostics[0].clone());
    assert!(matches!(
        DetailsProjection::project(&input, &DetailsUiState::default()),
        Err(ProjectionError::DuplicateKey(key))
            if matches!(key.as_ref(), AssuranceKey::Diagnostic {
                code,
                path,
                position: Some((4, 2)),
            } if code == "field_rondo_feed_absent" && path == "rondo")
    ));
}
