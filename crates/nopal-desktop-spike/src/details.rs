use std::collections::{BTreeMap, BTreeSet};

use nopal_feed_client::field::{ExecutionOutcome, ExecutionState, FruitState, ProgressState};

use crate::model::{
    DesktopApproval, DesktopAssuranceModel, DesktopAssuranceRepository, DesktopAssuranceSession,
    DesktopDiagnostic, DesktopEvidence, DesktopGateAttempt, DesktopObservedRun, ProofPolicy,
    RequirementLevel,
};

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
