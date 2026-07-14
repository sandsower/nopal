//! Typed, renderer-neutral Core assurance model and exact-key projection.

use std::collections::{BTreeMap, BTreeSet};

use nopal_feed_client::field::{
    ApprovalState, DiagnosticSeverity, ExecutionOutcome, ExecutionState, FieldSnapshot, FruitState,
    GateAttemptState, ProgressState,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSessionProtocol {
    pub kind: String,
    pub transport: String,
    pub address: String,
    pub state: String,
    pub extra: BTreeMap<String, serde_json::Value>,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssuranceKey {
    Progress {
        plot_id: String,
    },
    Condition {
        plot_id: String,
        text: String,
        occurrence: usize,
    },
    Fruit {
        plot_id: String,
    },
    Session {
        plot_id: String,
        session_id: String,
    },
    Repository {
        plot_id: String,
        repository_id: String,
    },
    Root {
        plot_id: String,
        repository_id: String,
        root_id: String,
    },
    ProofRequirement {
        plot_id: String,
        repository_id: String,
        root_id: String,
        proof_id: String,
    },
    DeclaredGate {
        plot_id: String,
        repository_id: String,
        gate_id: String,
    },
    RequiredGateDeclaration {
        plot_id: String,
        repository_id: String,
        root_id: String,
        proof_id: String,
        gate_id: String,
    },
    Execution {
        plot_id: String,
        service_id: String,
        repo_id: String,
        run_id: String,
    },
    Evidence {
        plot_id: String,
        service_id: String,
        repo_id: String,
        run_id: String,
        artifact_kind: String,
        uri: String,
    },
    Approval {
        ask_id: String,
    },
    ObservedRun {
        source: ObservedRunIdentity,
    },
    ObservedRunEvidence {
        source: ObservedRunIdentity,
        rondo_run_id: String,
        artifact_kind: String,
        uri: String,
    },
    ObservedGateAttempt {
        source: ObservedRunIdentity,
        gate_run_id: String,
        name: String,
        scope: String,
        attempt: Option<u64>,
    },
    Diagnostic {
        code: String,
        path: String,
        position: Option<(usize, usize)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObservedRunIdentity {
    pub flow: String,
    pub repo_hash: String,
    pub run_id: String,
}

impl From<&DesktopObservedRun> for ObservedRunIdentity {
    fn from(run: &DesktopObservedRun) -> Self {
        Self {
            flow: run.placement.flow.clone(),
            repo_hash: run.placement.repo_hash.clone(),
            run_id: run.run_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub plot_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssuranceFact {
    Progress {
        key: AssuranceKey,
        state: ProgressState,
    },
    Condition {
        key: AssuranceKey,
        text: String,
    },
    Fruit {
        key: AssuranceKey,
        state: FruitState,
    },
    Session {
        key: AssuranceKey,
        identity: SessionIdentity,
        session: DesktopAssuranceSession,
    },
    Repository {
        key: AssuranceKey,
        repository: DesktopAssuranceRepository,
    },
    Root {
        key: AssuranceKey,
        statement: String,
    },
    ProofRequirement {
        key: AssuranceKey,
        stage: String,
        required: RequirementLevel,
        on_missing: ProofPolicy,
        on_failure: ProofPolicy,
    },
    DeclaredGate {
        key: AssuranceKey,
        gate_id: String,
    },
    RequiredGateDeclaration {
        key: AssuranceKey,
        gate_id: String,
    },
    Execution {
        key: AssuranceKey,
        manifest_sha256: String,
        status: ExecutionState,
        outcome: Option<ExecutionOutcome>,
        event_cursor: String,
        created_at: String,
        updated_at: String,
    },
    Evidence {
        key: AssuranceKey,
        evidence: DesktopEvidence,
    },
    Approval {
        key: AssuranceKey,
        approval: DesktopApproval,
        attached_session: Option<SessionIdentity>,
    },
    ObservedRun {
        key: AssuranceKey,
        run: DesktopObservedRun,
    },
    ObservedRunEvidence {
        key: AssuranceKey,
        evidence: DesktopEvidence,
    },
    ObservedGateAttempt {
        key: AssuranceKey,
        attempt: DesktopGateAttempt,
    },
    Diagnostic {
        key: AssuranceKey,
        diagnostic: DesktopDiagnostic,
    },
}

impl AssuranceFact {
    pub fn key(&self) -> &AssuranceKey {
        match self {
            Self::Progress { key, .. }
            | Self::Condition { key, .. }
            | Self::Fruit { key, .. }
            | Self::Session { key, .. }
            | Self::Repository { key, .. }
            | Self::Root { key, .. }
            | Self::ProofRequirement { key, .. }
            | Self::DeclaredGate { key, .. }
            | Self::RequiredGateDeclaration { key, .. }
            | Self::Execution { key, .. }
            | Self::Evidence { key, .. }
            | Self::Approval { key, .. }
            | Self::ObservedRun { key, .. }
            | Self::ObservedRunEvidence { key, .. }
            | Self::ObservedGateAttempt { key, .. }
            | Self::Diagnostic { key, .. } => key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetailsUiState {
    pub selected_by_plot: BTreeMap<String, AssuranceKey>,
    pub expanded: BTreeSet<AssuranceKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotDetails {
    pub plot_id: String,
    pub facts: Vec<AssuranceFact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailsProjection {
    pub plots: Vec<PlotDetails>,
    pub unbound: Vec<AssuranceFact>,
    pub ui: DetailsUiState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    DuplicateKey(Box<AssuranceKey>),
    UnknownSessionPlot(String),
}

impl DetailsProjection {
    pub fn project(
        model: &DesktopAssuranceModel,
        previous_ui: &DetailsUiState,
    ) -> Result<Self, ProjectionError> {
        let mut projection = Builder::new(model).build()?;
        projection.ui = reconcile_ui(&projection, previous_ui);
        Ok(projection)
    }

    pub fn plot(&self, plot_id: &str) -> Option<&PlotDetails> {
        self.plots.iter().find(|plot| plot.plot_id == plot_id)
    }
}

struct Builder<'a> {
    model: &'a DesktopAssuranceModel,
    plots: Vec<PlotDetails>,
    unbound: Vec<AssuranceFact>,
    keys: BTreeSet<AssuranceKey>,
    sessions: BTreeMap<String, Vec<SessionIdentity>>,
}

impl<'a> Builder<'a> {
    fn new(model: &'a DesktopAssuranceModel) -> Self {
        Self {
            model,
            plots: Vec::new(),
            unbound: Vec::new(),
            keys: BTreeSet::new(),
            sessions: BTreeMap::new(),
        }
    }

    fn build(mut self) -> Result<DetailsProjection, ProjectionError> {
        let plots = self.model.plots.clone();
        for plot in &plots {
            let plot_id = plot.plot_id.clone();
            let mut details = PlotDetails {
                plot_id: plot_id.clone(),
                facts: Vec::new(),
            };
            self.push_plot(
                &mut details,
                AssuranceFact::Progress {
                    key: AssuranceKey::Progress {
                        plot_id: plot_id.clone(),
                    },
                    state: plot.progress.clone(),
                },
            )?;
            let mut condition_occurrences = BTreeMap::<String, usize>::new();
            for condition in &plot.conditions {
                let occurrence = condition_occurrences
                    .entry(condition.clone())
                    .and_modify(|count| *count += 1)
                    .or_insert(0);
                self.push_plot(
                    &mut details,
                    AssuranceFact::Condition {
                        key: AssuranceKey::Condition {
                            plot_id: plot_id.clone(),
                            text: condition.clone(),
                            occurrence: *occurrence,
                        },
                        text: condition.clone(),
                    },
                )?;
            }
            self.push_plot(
                &mut details,
                AssuranceFact::Fruit {
                    key: AssuranceKey::Fruit {
                        plot_id: plot_id.clone(),
                    },
                    state: plot.fruit.clone(),
                },
            )?;
            for session in &plot.sessions {
                let identity = SessionIdentity {
                    plot_id: plot_id.clone(),
                    session_id: session.session_id.clone(),
                };
                if !session.session_id.is_empty() {
                    self.sessions
                        .entry(session.session_id.clone())
                        .or_default()
                        .push(identity.clone());
                }
                self.push_plot(
                    &mut details,
                    AssuranceFact::Session {
                        key: AssuranceKey::Session {
                            plot_id: plot_id.clone(),
                            session_id: session.session_id.clone(),
                        },
                        identity,
                        session: session.clone(),
                    },
                )?;
            }
            for repository in &plot.repositories {
                self.push_plot(
                    &mut details,
                    AssuranceFact::Repository {
                        key: AssuranceKey::Repository {
                            plot_id: plot_id.clone(),
                            repository_id: repository.repository_id.clone(),
                        },
                        repository: repository.clone(),
                    },
                )?;
                for gate_id in &repository.gate_ids {
                    self.push_plot(
                        &mut details,
                        AssuranceFact::DeclaredGate {
                            key: AssuranceKey::DeclaredGate {
                                plot_id: plot_id.clone(),
                                repository_id: repository.repository_id.clone(),
                                gate_id: gate_id.clone(),
                            },
                            gate_id: gate_id.clone(),
                        },
                    )?;
                }
                for root in &repository.roots {
                    self.push_plot(
                        &mut details,
                        AssuranceFact::Root {
                            key: AssuranceKey::Root {
                                plot_id: plot_id.clone(),
                                repository_id: repository.repository_id.clone(),
                                root_id: root.id.clone(),
                            },
                            statement: root.statement.clone(),
                        },
                    )?;
                    for proof in &root.proof_requirements {
                        self.push_plot(
                            &mut details,
                            AssuranceFact::ProofRequirement {
                                key: AssuranceKey::ProofRequirement {
                                    plot_id: plot_id.clone(),
                                    repository_id: repository.repository_id.clone(),
                                    root_id: root.id.clone(),
                                    proof_id: proof.id.clone(),
                                },
                                stage: proof.stage.clone(),
                                required: proof.required.clone(),
                                on_missing: proof.on_missing.clone(),
                                on_failure: proof.on_failure.clone(),
                            },
                        )?;
                        if proof.required == RequirementLevel::Required {
                            for gate_id in &proof.gates {
                                self.push_plot(
                                    &mut details,
                                    AssuranceFact::RequiredGateDeclaration {
                                        key: AssuranceKey::RequiredGateDeclaration {
                                            plot_id: plot_id.clone(),
                                            repository_id: repository.repository_id.clone(),
                                            root_id: root.id.clone(),
                                            proof_id: proof.id.clone(),
                                            gate_id: gate_id.clone(),
                                        },
                                        gate_id: gate_id.clone(),
                                    },
                                )?;
                            }
                        }
                    }
                }
            }
            for execution in &plot.executions {
                let execution_key = AssuranceKey::Execution {
                    plot_id: plot_id.clone(),
                    service_id: execution.service_id.clone(),
                    repo_id: execution.repo_id.clone(),
                    run_id: execution.run_id.clone(),
                };
                self.push_plot(
                    &mut details,
                    AssuranceFact::Execution {
                        key: execution_key,
                        manifest_sha256: execution.manifest_sha256.clone(),
                        status: execution.status.clone(),
                        outcome: execution.outcome.clone(),
                        event_cursor: execution.event_cursor.clone(),
                        created_at: execution.created_at.clone(),
                        updated_at: execution.updated_at.clone(),
                    },
                )?;
                for evidence in &execution.evidence {
                    self.push_plot(
                        &mut details,
                        AssuranceFact::Evidence {
                            key: AssuranceKey::Evidence {
                                plot_id: plot_id.clone(),
                                service_id: execution.service_id.clone(),
                                repo_id: execution.repo_id.clone(),
                                run_id: execution.run_id.clone(),
                                artifact_kind: evidence.artifact_kind.clone(),
                                uri: evidence.uri.clone(),
                            },
                            evidence: evidence.clone(),
                        },
                    )?;
                }
            }
            self.plots.push(details);
        }

        let observed_runs = self.model.observed_runs.clone();
        for run in &observed_runs {
            let source = ObservedRunIdentity::from(run);
            self.push_unbound(AssuranceFact::ObservedRun {
                key: AssuranceKey::ObservedRun {
                    source: source.clone(),
                },
                run: run.clone(),
            })?;
            if let Some(rondo) = &run.rondo {
                for evidence in &rondo.evidence {
                    self.push_unbound(AssuranceFact::ObservedRunEvidence {
                        key: AssuranceKey::ObservedRunEvidence {
                            source: source.clone(),
                            rondo_run_id: rondo.run_id.clone(),
                            artifact_kind: evidence.artifact_kind.clone(),
                            uri: evidence.uri.clone(),
                        },
                        evidence: evidence.clone(),
                    })?;
                }
            }
            for gate in &run.gates {
                self.push_unbound(AssuranceFact::ObservedGateAttempt {
                    key: AssuranceKey::ObservedGateAttempt {
                        source: source.clone(),
                        gate_run_id: gate.run_id.clone(),
                        name: gate.name.clone(),
                        scope: gate.scope.clone(),
                        attempt: gate.attempt,
                    },
                    attempt: gate.clone(),
                })?;
            }
        }

        let approvals = self
            .model
            .observed_runs
            .iter()
            .flat_map(|run| run.approvals.iter())
            .chain(self.model.unbound_approvals.iter())
            .cloned()
            .collect::<Vec<_>>();
        for approval in approvals {
            let run_identity_consistent = match (&approval.source_run_id, &approval.run_id) {
                (Some(source), Some(explicit)) => source == explicit,
                _ => true,
            };
            let attached = (approval.canonical && run_identity_consistent)
                .then(|| self.sessions.get(&approval.session_id))
                .flatten()
                .filter(|matches| matches.len() == 1)
                .and_then(|matches| matches.first())
                .cloned();
            let fact = AssuranceFact::Approval {
                key: AssuranceKey::Approval {
                    ask_id: approval.ask_id.clone(),
                },
                approval,
                attached_session: attached.clone(),
            };
            if let Some(session) = attached {
                let Some(plot) = self
                    .plots
                    .iter_mut()
                    .find(|plot| plot.plot_id == session.plot_id)
                else {
                    return Err(ProjectionError::UnknownSessionPlot(session.plot_id));
                };
                Self::register(&mut self.keys, fact.key())?;
                plot.facts.push(fact);
            } else {
                self.push_unbound(fact)?;
            }
        }

        let diagnostics = self.model.diagnostics.clone();
        for diagnostic in &diagnostics {
            self.push_unbound(AssuranceFact::Diagnostic {
                key: AssuranceKey::Diagnostic {
                    code: diagnostic.code.clone(),
                    path: diagnostic.path.clone(),
                    position: diagnostic.position,
                },
                diagnostic: diagnostic.clone(),
            })?;
        }

        Ok(DetailsProjection {
            plots: self.plots,
            unbound: self.unbound,
            ui: DetailsUiState::default(),
        })
    }

    fn push_plot(
        &mut self,
        plot: &mut PlotDetails,
        fact: AssuranceFact,
    ) -> Result<(), ProjectionError> {
        Self::register(&mut self.keys, fact.key())?;
        plot.facts.push(fact);
        Ok(())
    }

    fn push_unbound(&mut self, fact: AssuranceFact) -> Result<(), ProjectionError> {
        Self::register(&mut self.keys, fact.key())?;
        self.unbound.push(fact);
        Ok(())
    }

    fn register(
        keys: &mut BTreeSet<AssuranceKey>,
        key: &AssuranceKey,
    ) -> Result<(), ProjectionError> {
        if keys.insert(key.clone()) {
            Ok(())
        } else {
            Err(ProjectionError::DuplicateKey(Box::new(key.clone())))
        }
    }
}

fn reconcile_ui(projection: &DetailsProjection, previous: &DetailsUiState) -> DetailsUiState {
    let all_keys = projection
        .plots
        .iter()
        .flat_map(|plot| plot.facts.iter())
        .chain(projection.unbound.iter())
        .map(AssuranceFact::key)
        .cloned()
        .collect::<BTreeSet<_>>();
    let selected_by_plot = projection
        .plots
        .iter()
        .filter_map(|plot| {
            let plot_keys = plot
                .facts
                .iter()
                .map(AssuranceFact::key)
                .collect::<BTreeSet<_>>();
            previous
                .selected_by_plot
                .get(&plot.plot_id)
                .filter(|key| plot_keys.contains(key))
                .cloned()
                .or_else(|| plot.facts.first().map(|fact| fact.key().clone()))
                .map(|selected| (plot.plot_id.clone(), selected))
        })
        .collect();
    let expanded = previous.expanded.intersection(&all_keys).cloned().collect();
    DetailsUiState {
        selected_by_plot,
        expanded,
    }
}
