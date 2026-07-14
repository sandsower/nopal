use std::collections::BTreeMap;

use nopal_feed_client::field::{
    ApprovalState, DiagnosticSeverity, ExecutionOutcome, ExecutionState, FieldSnapshot, FruitState,
    GateAttemptState, ProgressState,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopField {
    pub plots: Vec<DesktopPlot>,
    pub selected_plot_id: Option<String>,
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPlot {
    pub plot_id: String,
    pub title: String,
    pub progress: String,
    pub conditions: Vec<String>,
    pub activities: Vec<DesktopActivity>,
    pub selected_session_id: Option<String>,
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DesktopActivityKey {
    Session(String),
    Execution {
        service_id: String,
        repo_id: String,
        run_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSessionProtocol {
    pub kind: String,
    pub transport: String,
    pub address: String,
    pub state: String,
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSessionContext {
    pub plot_id: String,
    pub session_id: String,
    pub host_pane: Option<String>,
    pub protocol: Option<DesktopSessionProtocol>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAssuranceModel {
    pub plots: Vec<DesktopPlotAssurance>,
    pub observed_runs: Vec<DesktopObservedRun>,
    pub unbound_approvals: Vec<DesktopApproval>,
    pub diagnostics: Vec<DesktopDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPlotAssurance {
    pub plot_id: String,
    pub progress: ProgressState,
    pub conditions: Vec<String>,
    pub fruit: FruitState,
    pub sessions: Vec<DesktopAssuranceSession>,
    pub executions: Vec<DesktopAssuranceExecution>,
    pub repositories: Vec<DesktopAssuranceRepository>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAssuranceSession {
    pub session_id: String,
    pub mode: String,
    pub host: String,
    pub host_session: String,
    pub host_pane: Option<String>,
    pub state: SessionState,
    pub protocol: Option<DesktopSessionProtocol>,
    pub workspace: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Idle,
    Closed,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAssuranceExecution {
    pub service_id: String,
    pub repo_id: String,
    pub run_id: String,
    pub manifest_sha256: String,
    pub status: ExecutionState,
    pub outcome: Option<ExecutionOutcome>,
    pub event_cursor: String,
    pub evidence: Vec<DesktopEvidence>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEvidence {
    pub artifact_kind: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAssuranceRepository {
    pub repository_id: String,
    pub root: String,
    pub configuration_root: String,
    pub revision: Option<String>,
    pub roots: Vec<DesktopRoot>,
    pub gate_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRoot {
    pub id: String,
    pub statement: String,
    pub proof_requirements: Vec<DesktopProofRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProofRequirement {
    pub id: String,
    pub stage: String,
    pub required: RequirementLevel,
    pub gates: Vec<String>,
    pub on_missing: ProofPolicy,
    pub on_failure: ProofPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementLevel {
    Required,
    Optional,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofPolicy {
    Block,
    Warn,
    Ask,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopObservedRun {
    pub run_id: String,
    pub flow: String,
    pub skill: String,
    pub status: ObservedRunState,
    pub ticket_id: String,
    pub branch: String,
    pub started_at: String,
    pub updated_at: String,
    pub placement: DesktopRunPlacement,
    pub stale: bool,
    pub gates: Vec<DesktopGateAttempt>,
    pub rondo: Option<DesktopObservedRondo>,
    pub approvals: Vec<DesktopApproval>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedRunState {
    Running,
    Interrupted,
    Failed,
    Completed,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRunPlacement {
    pub repo: String,
    pub repo_hash: String,
    pub branch: String,
    pub run_dir: String,
    pub flow: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopObservedRondo {
    pub run_id: String,
    pub status: Option<ObservedRondoState>,
    pub evidence: Vec<DesktopEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedRondoState {
    Running,
    Paused,
    Completed,
    Failed,
    Terminated,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopGateAttempt {
    pub run_id: String,
    pub attempt: Option<u64>,
    pub name: String,
    pub scope: String,
    pub status: GateAttemptState,
    pub classification: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApproval {
    pub ask_id: String,
    pub canonical: bool,
    pub action: String,
    pub reason: String,
    pub session_id: String,
    pub repo: String,
    pub state: ApprovalState,
    pub created_at: String,
    pub expires_at: String,
    pub run_id: Option<String>,
    pub source_run_id: Option<String>,
}

impl DesktopApproval {
    pub fn is_approved(&self) -> bool {
        self.canonical && self.state == ApprovalState::Approved
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub path: String,
    pub position: Option<(usize, usize)>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopActivity {
    Session {
        session_id: String,
        host_pane: Option<String>,
        state: String,
        protocol: Option<DesktopSessionProtocol>,
    },
    Execution {
        service_id: String,
        repo_id: String,
        run_id: String,
        status: String,
    },
}

impl DesktopActivity {
    pub fn key(&self) -> DesktopActivityKey {
        match self {
            Self::Session { session_id, .. } => DesktopActivityKey::Session(session_id.clone()),
            Self::Execution {
                service_id,
                repo_id,
                run_id,
                ..
            } => DesktopActivityKey::Execution {
                service_id: service_id.clone(),
                repo_id: repo_id.clone(),
                run_id: run_id.clone(),
            },
        }
    }
}

impl DesktopPlot {
    pub fn activity_keys(&self) -> Vec<DesktopActivityKey> {
        self.activities.iter().map(DesktopActivity::key).collect()
    }
}

impl DesktopAssuranceModel {
    pub fn from_snapshot(snapshot: &FieldSnapshot) -> Self {
        let plots = snapshot
            .plots
            .iter()
            .map(|plot| DesktopPlotAssurance {
                plot_id: plot.plot_id.clone(),
                progress: plot.progress_state(),
                conditions: plot.conditions.clone(),
                fruit: plot.fruit_state(),
                sessions: plot
                    .sessions
                    .iter()
                    .map(|session| DesktopAssuranceSession {
                        session_id: session.session_id.clone(),
                        mode: session.mode.clone(),
                        host: session.host.clone(),
                        host_session: session.host_session.clone(),
                        host_pane: session.host_pane.clone(),
                        state: session_state(&session.state),
                        protocol: session.protocol.as_ref().map(|protocol| {
                            DesktopSessionProtocol {
                                kind: protocol.kind.clone(),
                                transport: protocol.transport.clone(),
                                address: protocol.address.clone(),
                                state: protocol.state.clone(),
                                extra: protocol.extra.clone(),
                            }
                        }),
                        workspace: session.workspace.clone(),
                        created_at: session.created_at.clone(),
                        updated_at: session.updated_at.clone(),
                        extra: session.extra.clone(),
                    })
                    .collect(),
                executions: plot
                    .executions
                    .iter()
                    .map(|execution| DesktopAssuranceExecution {
                        service_id: execution.service_id.clone(),
                        repo_id: execution.repo_id.clone(),
                        run_id: execution.run_id.clone(),
                        manifest_sha256: execution.manifest_sha256.clone(),
                        status: execution.status_state(),
                        outcome: execution.outcome_state(),
                        event_cursor: execution.event_cursor.clone(),
                        evidence: execution
                            .evidence
                            .iter()
                            .map(|pointer| DesktopEvidence {
                                artifact_kind: pointer.artifact_kind.clone(),
                                uri: pointer.uri.clone(),
                            })
                            .collect(),
                        created_at: execution.created_at.clone(),
                        updated_at: execution.updated_at.clone(),
                    })
                    .collect(),
                repositories: plot
                    .repositories
                    .iter()
                    .map(|repository| DesktopAssuranceRepository {
                        repository_id: repository.repository_id.clone(),
                        root: repository.root.clone(),
                        configuration_root: repository.configuration_root.clone(),
                        revision: repository.revision.clone(),
                        roots: repository
                            .roots
                            .iter()
                            .map(|root| DesktopRoot {
                                id: root.id.clone(),
                                statement: root.statement.clone(),
                                proof_requirements: root
                                    .proof_requirements
                                    .iter()
                                    .map(|proof| DesktopProofRequirement {
                                        id: proof.id.clone(),
                                        stage: proof.stage.clone(),
                                        required: if !proof.required_present {
                                            RequirementLevel::Unavailable
                                        } else if proof.required {
                                            RequirementLevel::Required
                                        } else {
                                            RequirementLevel::Optional
                                        },
                                        gates: proof.gates.clone(),
                                        on_missing: proof_policy(&proof.on_missing),
                                        on_failure: proof_policy(&proof.on_failure),
                                    })
                                    .collect(),
                            })
                            .collect(),
                        gate_ids: repository.gate_ids.clone(),
                    })
                    .collect(),
            })
            .collect();
        let observed_runs = snapshot
            .entries
            .iter()
            .map(|entry| DesktopObservedRun {
                run_id: entry.run_id.clone(),
                flow: entry.flow.clone(),
                skill: entry.skill.clone(),
                status: observed_run_state(&entry.status),
                ticket_id: entry.ticket_id.clone(),
                branch: entry.branch.clone(),
                started_at: entry.started_at.clone(),
                updated_at: entry.updated_at.clone(),
                placement: DesktopRunPlacement {
                    repo: entry.placement.repo.clone(),
                    repo_hash: entry.placement.repo_hash.clone(),
                    branch: entry.placement.branch.clone(),
                    run_dir: entry.placement.run_dir.clone(),
                    flow: entry.placement.flow.clone(),
                },
                stale: entry.stale,
                gates: entry
                    .gates
                    .iter()
                    .map(|gate| DesktopGateAttempt {
                        run_id: entry.run_id.clone(),
                        attempt: gate.attempt,
                        name: gate.name.clone(),
                        scope: gate.scope.clone(),
                        status: gate.status_state(),
                        classification: gate.classification.clone(),
                        path: gate.path.clone(),
                    })
                    .collect(),
                rondo: entry.rondo.as_ref().map(|rondo| DesktopObservedRondo {
                    run_id: rondo.run_id.clone(),
                    status: rondo.status.as_deref().map(observed_rondo_state),
                    evidence: rondo
                        .evidence
                        .iter()
                        .map(|pointer| DesktopEvidence {
                            artifact_kind: pointer.artifact_kind.clone(),
                            uri: pointer.uri.clone(),
                        })
                        .collect(),
                }),
                approvals: entry
                    .asks
                    .iter()
                    .map(|ask| approval(ask, Some(entry.run_id.clone())))
                    .collect(),
            })
            .collect();
        let unbound_approvals = snapshot
            .asks_unbound
            .iter()
            .map(|ask| approval(ask, None))
            .collect();
        let diagnostics = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| DesktopDiagnostic {
                severity: diagnostic.severity_state(),
                code: diagnostic.code.clone(),
                path: diagnostic.path.clone(),
                position: diagnostic
                    .position
                    .as_ref()
                    .map(|position| (position.line, position.column)),
                message: diagnostic.message.clone(),
            })
            .collect();
        Self {
            plots,
            observed_runs,
            unbound_approvals,
            diagnostics,
        }
    }
}

fn session_state(value: &str) -> SessionState {
    match value {
        "active" => SessionState::Active,
        "idle" => SessionState::Idle,
        "closed" => SessionState::Closed,
        "" => SessionState::Unavailable,
        value => SessionState::Unknown(value.to_owned()),
    }
}

fn observed_run_state(value: &str) -> ObservedRunState {
    match value {
        "running" => ObservedRunState::Running,
        "interrupted" => ObservedRunState::Interrupted,
        "failed" => ObservedRunState::Failed,
        "completed" => ObservedRunState::Completed,
        "" => ObservedRunState::Unavailable,
        value => ObservedRunState::Unknown(value.to_owned()),
    }
}

fn observed_rondo_state(value: &str) -> ObservedRondoState {
    match value {
        "running" => ObservedRondoState::Running,
        "paused" => ObservedRondoState::Paused,
        "completed" => ObservedRondoState::Completed,
        "failed" => ObservedRondoState::Failed,
        "terminated" => ObservedRondoState::Terminated,
        value => ObservedRondoState::Unknown(value.to_owned()),
    }
}

fn proof_policy(value: &str) -> ProofPolicy {
    match value {
        "block" => ProofPolicy::Block,
        "warn" => ProofPolicy::Warn,
        "ask" => ProofPolicy::Ask,
        "" => ProofPolicy::Unavailable,
        value => ProofPolicy::Unknown(value.to_owned()),
    }
}

fn approval(
    ask: &nopal_feed_client::field::FieldAsk,
    source_run_id: Option<String>,
) -> DesktopApproval {
    DesktopApproval {
        ask_id: ask.ask_id.clone(),
        canonical: ask.is_canonical(),
        action: ask.action.clone(),
        reason: ask.reason.clone(),
        session_id: ask.session_id.clone(),
        repo: ask.repo.clone(),
        state: ask.approval_state(),
        created_at: ask.created_at.clone(),
        expires_at: ask.expires_at.clone(),
        run_id: ask.run_id.clone(),
        source_run_id,
    }
}

impl DesktopField {
    pub fn empty() -> Self {
        Self {
            plots: Vec::new(),
            selected_plot_id: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn demo(pane_id: String) -> Self {
        Self {
            plots: vec![DesktopPlot {
                plot_id: "plot-native-spike".to_owned(),
                title: "Build the native Nopal Field".to_owned(),
                progress: "Active".to_owned(),
                conditions: Vec::new(),
                activities: vec![DesktopActivity::Session {
                    session_id: "session-native-spike".to_owned(),
                    host_pane: Some(pane_id),
                    state: "active".to_owned(),
                    protocol: None,
                }],
                selected_session_id: Some("session-native-spike".to_owned()),
                extra: BTreeMap::new(),
            }],
            selected_plot_id: Some("plot-native-spike".to_owned()),
            extra: BTreeMap::new(),
        }
    }

    pub fn from_snapshot(snapshot: FieldSnapshot, preferred_plot_id: Option<&str>) -> Self {
        let plots = snapshot
            .plots
            .into_iter()
            .map(|plot| DesktopPlot {
                plot_id: plot.plot_id,
                title: plot.title,
                progress: plot.progress,
                conditions: plot.conditions,
                activities: plot
                    .sessions
                    .into_iter()
                    .map(|session| DesktopActivity::Session {
                        session_id: session.session_id,
                        host_pane: session.host_pane,
                        state: session.state,
                        protocol: session.protocol.map(|protocol| DesktopSessionProtocol {
                            kind: protocol.kind,
                            transport: protocol.transport,
                            address: protocol.address,
                            state: protocol.state,
                            extra: protocol.extra,
                        }),
                    })
                    .chain(plot.executions.into_iter().map(|execution| {
                        DesktopActivity::Execution {
                            service_id: execution.service_id,
                            repo_id: execution.repo_id,
                            run_id: execution.run_id,
                            status: execution.status,
                        }
                    }))
                    .collect(),
                selected_session_id: plot.selected_session_id,
                extra: plot.extra,
            })
            .collect::<Vec<_>>();
        let selected_plot_id = preferred_plot_id
            .filter(|id| plots.iter().any(|plot| plot.plot_id == **id))
            .map(str::to_owned)
            .or_else(|| plots.first().map(|plot| plot.plot_id.clone()));

        Self {
            plots,
            selected_plot_id,
            extra: snapshot.extra,
        }
    }

    pub fn selected_plot(&self) -> Option<&DesktopPlot> {
        let selected_plot_id = self.selected_plot_id.as_deref()?;
        self.plots
            .iter()
            .find(|plot| plot.plot_id == selected_plot_id)
    }

    pub fn selected_session(&self) -> Option<&DesktopActivity> {
        let plot = self.selected_plot()?;
        let selected_session_id = plot.selected_session_id.as_deref()?;
        plot.activities.iter().find(|activity| {
            matches!(
                activity,
                DesktopActivity::Session { session_id, .. } if session_id == selected_session_id
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use nopal_feed_client::field::parse_field;

    use nopal_feed_client::field::{
        ApprovalState, DiagnosticSeverity, ExecutionOutcome, ExecutionState, FruitState,
        GateAttemptState, ProgressState,
    };

    use super::{
        DesktopActivity, DesktopAssuranceModel, DesktopField, ObservedRondoState, ObservedRunState,
        ProofPolicy, RequirementLevel, SessionState,
    };

    fn snapshot() -> nopal_feed_client::field::FieldSnapshot {
        let value = serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": [
                {
                    "kind": "nopal.plot/v1",
                    "plot_id": "plot-a",
                    "title": "First Plot",
                    "progress": "active",
                    "conditions": ["waiting"],
                    "selected_session_id": "session-a",
                    "sessions": [{
                        "session_id": "session-a",
                        "host_pane": "%7",
                        "state": "active",
                        "protocol": {
                            "kind": "nopal.session/v1",
                            "transport": "unix",
                            "address": "/tmp/session-a.sock",
                            "state": "ready",
                            "future_protocol_fact": "preserved"
                        }
                    }],
                    "executions": [{
                        "service_id": "rondo",
                        "repo_id": "repo-a",
                        "run_id": "run-a",
                        "status": "running"
                    }],
                    "future_plot_fact": "preserved"
                },
                {
                    "kind": "nopal.plot/v1",
                    "plot_id": "plot-b",
                    "title": "Second Plot",
                    "progress": "review",
                    "sessions": [{
                        "session_id": "session-b",
                        "host_pane": "%8",
                        "state": "idle"
                    }],
                    "executions": []
                }
            ],
            "entries": [],
            "future_field_fact": {"version": 2}
        });
        parse_field(&value).expect("fixture must satisfy the host-neutral contract")
    }

    fn assurance_snapshot() -> nopal_feed_client::field::FieldSnapshot {
        parse_field(&serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": [{
                "kind": "nopal.plot/v1",
                "plot_id": "plot-a",
                "progress": "active",
                "conditions": ["keep quality"],
                "fruit": {"state": "absent"},
                "sessions": [{
                    "session_id": "session-a",
                    "mode": "interactive",
                    "host": "pi",
                    "host_session": "nopal-work",
                    "host_pane": "%7",
                    "state": "active",
                    "protocol": {
                        "kind": "nopal.session/v3",
                        "transport": "unix",
                        "address": "/tmp/session-a.sock",
                        "state": "ready"
                    },
                    "workspace": "/repo",
                    "created_at": "t0",
                    "updated_at": "t1",
                    "future_session_fact": true
                }],
                "executions": [{
                    "service_id": "rondo-core",
                    "repo_id": "repo-a",
                    "run_id": "execution-a",
                    "manifest_sha256": "abc",
                    "status": "completed",
                    "outcome": "completed",
                    "event_cursor": "rondo.core/v1:3",
                    "evidence": [{"artifact_kind": "log", "uri": "rondo://execution-a/log"}],
                    "created_at": "t0",
                    "updated_at": "t1"
                }],
                "repositories": [{
                    "repository_id": "repo-a",
                    "root": "/repo",
                    "configuration_root": "/repo",
                    "revision": "abc123",
                    "roots": [{
                        "id": "quality",
                        "statement": "Quality remains green",
                        "proof_requirements": [{
                            "id": "pre-pr",
                            "stage": "pre_pr",
                            "required": true,
                            "gates": ["test"],
                            "on_missing": "block",
                            "on_failure": "ask"
                        }]
                    }],
                    "gate_ids": ["test"]
                }]
            }],
            "entries": [{
                "run_id": "ledger-a",
                "flow": "implement",
                "skill": "implement",
                "status": "running",
                "ticket_id": "TASK-57",
                "branch": "codex/task-57",
                "started_at": "t0",
                "updated_at": "t1",
                "stale": false,
                "placement": {
                    "repo": "/repo",
                    "repo_hash": "repo-hash",
                    "branch": "codex/task-57",
                    "run_dir": "/state/ledger-a",
                    "flow": "implement"
                },
                "gates": [{"attempt": 2, "name": "test", "scope": "repo", "status": "pass"}],
                "rondo": {
                    "run_id": "ledger-a",
                    "status": "completed",
                    "evidence": [{"artifact_kind": "run-log", "uri": "rondo://ledger-a/log"}]
                },
                "asks": [{
                    "kind": "nopal.ask/v1",
                    "ask_id": "ask-a",
                    "action": "git.push",
                    "reason": "publish",
                    "session_id": "session-a",
                    "state": "approved",
                    "run_id": "ledger-a"
                }]
            }],
            "asks_unbound": [{
                "kind": "nopal.ask/v1",
                "ask_id": "ask-b",
                "session_id": "missing-session",
                "state": "pending"
            }],
            "diagnostics": [{
                "severity": "info",
                "code": "field_partial_coverage",
                "path": "field",
                "message": "partial"
            }]
        }))
        .unwrap()
    }

    #[test]
    fn preserves_contract_order_and_durable_plot_selection() {
        let field = DesktopField::from_snapshot(snapshot(), Some("plot-b"));

        assert_eq!(
            field
                .plots
                .iter()
                .map(|plot| plot.plot_id.as_str())
                .collect::<Vec<_>>(),
            vec!["plot-a", "plot-b"]
        );
        assert_eq!(field.selected_plot_id.as_deref(), Some("plot-b"));
        assert_eq!(
            field.selected_plot().map(|plot| plot.title.as_str()),
            Some("Second Plot")
        );
    }

    #[test]
    fn falls_back_to_the_first_plot_when_saved_selection_disappears() {
        let field = DesktopField::from_snapshot(snapshot(), Some("vanished"));

        assert_eq!(field.selected_plot_id.as_deref(), Some("plot-a"));
    }

    #[test]
    fn sessions_precede_executions_and_selected_session_resolves_by_id() {
        let field = DesktopField::from_snapshot(snapshot(), Some("plot-a"));
        let plot = field.selected_plot().expect("selected Plot");

        assert!(matches!(
            plot.activities.as_slice(),
            [
                DesktopActivity::Session { session_id, .. },
                DesktopActivity::Execution { run_id, .. }
            ] if session_id == "session-a" && run_id == "run-a"
        ));
        assert!(matches!(
            field.selected_session(),
            Some(DesktopActivity::Session { session_id, host_pane, .. })
                if session_id == "session-a" && host_pane.as_deref() == Some("%7")
        ));
    }

    #[test]
    fn preserves_additive_contract_fields_at_the_renderer_boundary() {
        let field = DesktopField::from_snapshot(snapshot(), Some("plot-a"));

        assert_eq!(field.extra["future_field_fact"]["version"], 2);
        assert_eq!(
            field.selected_plot().expect("selected Plot").extra["future_plot_fact"],
            "preserved"
        );
        let Some(DesktopActivity::Session {
            protocol: Some(protocol),
            ..
        }) = field.selected_session()
        else {
            panic!("selected Session protocol");
        };
        assert_eq!(protocol.kind, "nopal.session/v1");
        assert_eq!(protocol.transport, "unix");
        assert_eq!(protocol.address, "/tmp/session-a.sock");
        assert_eq!(protocol.state, "ready");
        assert_eq!(protocol.extra["future_protocol_fact"], "preserved");
    }

    #[test]
    fn preserves_core_assurance_lanes_as_typed_desktop_facts() {
        let model = DesktopAssuranceModel::from_snapshot(&assurance_snapshot());
        let plot = &model.plots[0];

        assert_eq!(plot.progress, ProgressState::Active);
        assert_eq!(plot.conditions, ["keep quality"]);
        assert_eq!(plot.fruit, FruitState::Absent);
        assert_eq!(plot.sessions[0].state, SessionState::Active);
        assert_eq!(plot.sessions[0].host, "pi");
        assert_eq!(plot.sessions[0].host_pane.as_deref(), Some("%7"));
        assert_eq!(
            plot.sessions[0].protocol.as_ref().unwrap().kind,
            "nopal.session/v3"
        );
        assert_eq!(plot.sessions[0].extra["future_session_fact"], true);
        assert_eq!(plot.executions[0].status, ExecutionState::Completed);
        assert_eq!(
            plot.executions[0].outcome,
            Some(ExecutionOutcome::Completed)
        );
        assert_eq!(
            plot.executions[0].evidence[0].uri,
            "rondo://execution-a/log"
        );
        let proof = &plot.repositories[0].roots[0].proof_requirements[0];
        assert_eq!(plot.repositories[0].root, "/repo");
        assert_eq!(plot.repositories[0].revision.as_deref(), Some("abc123"));
        assert_eq!(proof.required, RequirementLevel::Required);
        assert_eq!(proof.on_missing, ProofPolicy::Block);
        assert_eq!(proof.on_failure, ProofPolicy::Ask);
        assert_eq!(
            model.observed_runs[0].gates[0].status,
            GateAttemptState::Pass
        );
        assert_eq!(model.observed_runs[0].gates[0].attempt, Some(2));
        assert_eq!(model.observed_runs[0].status, ObservedRunState::Running);
        assert_eq!(model.observed_runs[0].placement.repo_hash, "repo-hash");
        assert_eq!(
            model.observed_runs[0].rondo.as_ref().unwrap().evidence[0].uri,
            "rondo://ledger-a/log"
        );
        assert_eq!(
            model.observed_runs[0].rondo.as_ref().unwrap().status,
            Some(ObservedRondoState::Completed)
        );
        assert_eq!(
            model.observed_runs[0].approvals[0].state,
            ApprovalState::Approved
        );
        assert!(model.observed_runs[0].approvals[0].is_approved());
        assert_eq!(model.unbound_approvals[0].state, ApprovalState::Pending);
        assert_eq!(model.diagnostics[0].severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn malformed_desktop_assurance_facts_remain_non_positive() {
        let mut snapshot = assurance_snapshot();
        snapshot.plots[0].progress.clear();
        snapshot.plots[0].fruit.state = "accepted-ish".to_owned();
        snapshot.plots[0].executions[0].status = "done-ish".to_owned();
        snapshot.plots[0].executions[0].outcome = None;
        snapshot.plots[0].repositories[0].roots[0].proof_requirements[0].required_present = false;
        snapshot.entries[0].gates[0].status = "passing".to_owned();
        snapshot.entries[0].asks[0].kind = "nopal.ask/v2".to_owned();

        let model = DesktopAssuranceModel::from_snapshot(&snapshot);
        let plot = &model.plots[0];

        assert_eq!(plot.progress, ProgressState::Unavailable);
        assert_eq!(plot.fruit, FruitState::Unknown("accepted-ish".to_owned()));
        assert_eq!(
            plot.executions[0].status,
            ExecutionState::Unknown("done-ish".to_owned())
        );
        assert_eq!(plot.executions[0].outcome, None);
        assert_eq!(
            plot.repositories[0].roots[0].proof_requirements[0].required,
            RequirementLevel::Unavailable
        );
        assert_eq!(
            model.observed_runs[0].gates[0].status,
            GateAttemptState::Unknown("passing".to_owned())
        );
        assert!(!model.observed_runs[0].approvals[0].is_approved());
    }
}
