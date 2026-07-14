//! Independent live Session, workspace subject, activity, and inspector state.

use std::collections::BTreeMap;

use nopal_feed_client::field::FieldSnapshot;
use nopal_native_lifecycle::reconcile::{
    ExactSessionSelection, RestoreResolution, reconcile_restore,
};

use crate::activity::ActivityKey;
use crate::assurance::AssuranceKey;

/// Exact identity of one Core-owned execution shown in the workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionIdentity {
    plot_id: String,
    service_id: String,
    repo_id: String,
    run_id: String,
}

impl ExecutionIdentity {
    /// Creates a renderer-neutral exact execution identity.
    pub fn new(
        plot_id: impl Into<String>,
        service_id: impl Into<String>,
        repo_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            plot_id: plot_id.into(),
            service_id: service_id.into(),
            repo_id: repo_id.into(),
            run_id: run_id.into(),
        }
    }

    /// Returns the owning Plot identity.
    pub fn plot_id(&self) -> &str {
        &self.plot_id
    }

    /// Returns the execution service identity.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Returns the repository identity carried by Core.
    pub fn repo_id(&self) -> &str {
        &self.repo_id
    }

    /// Returns the execution run identity.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

/// The subject shown on the main workspace stage, independent of live bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceSubject {
    /// No Core-owned Plot or Session can be selected safely.
    Unavailable {
        /// Visible Core-derived reason selection is unavailable.
        reason: String,
    },
    /// Plot overview and assurance.
    Plot {
        /// Exact Core Plot identity.
        plot_id: String,
    },
    /// A Session timeline, which may or may not be the live bound Session.
    Session(ExactSessionSelection),
    /// Structured execution detail.
    Execution(ExecutionIdentity),
}

/// Presence of the unchanged live Session in the latest accepted Core snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveSessionState {
    /// Core still reports the exact bound Session under its Plot.
    Present(ExactSessionSelection),
    /// The binding remains live, but the latest Field facts no longer contain it.
    MissingFromCurrentField(ExactSessionSelection),
}

impl LiveSessionState {
    /// Returns the exact Session identity without asserting current presence.
    pub fn selection(&self) -> &ExactSessionSelection {
        match self {
            Self::Present(selection) | Self::MissingFromCurrentField(selection) => selection,
        }
    }
}

/// Exact secondary detail displayed without replacing the workspace or live Session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectorSelection {
    /// One activity key scoped to its exact Session.
    Activity {
        /// Exact Core Session containing the activity stream.
        session: ExactSessionSelection,
        /// Stable activity or semantic event key.
        key: ActivityKey,
    },
    /// One Plot-bound or unbound Core assurance fact.
    Assurance {
        /// Exact projection key, including unbound fact variants.
        key: AssuranceKey,
    },
}

/// Closed or independently open detail inspector state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectorState {
    /// No secondary detail is open.
    Closed,
    /// One exact detail is open.
    Open(InspectorSelection),
}

/// The only instruction capable of changing structured/Terminal Session bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingDirective {
    /// Preserve all current bindings and Composer state.
    Unchanged,
    /// Atomically replace bindings with this exact Core Session.
    ReplaceWith(ExactSessionSelection),
}

/// Validation failure from an explicit Session activation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionActivationValidationError {
    resolution: RestoreResolution,
}

impl SessionActivationValidationError {
    /// Returns Core-derived evidence explaining why the target was not exact.
    pub fn resolution(&self) -> &RestoreResolution {
        &self.resolution
    }
}

/// Exact activation prepared without mutating the prior live/view state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedSessionActivation {
    target: ExactSessionSelection,
    binding: BindingDirective,
}

impl PreparedSessionActivation {
    /// Returns the exact target validated against current Core facts.
    pub fn target(&self) -> &ExactSessionSelection {
        &self.target
    }

    /// Returns the sole binding instruction produced by view state.
    pub fn binding_directive(&self) -> &BindingDirective {
        &self.binding
    }
}

/// Renderer-neutral interaction state with explicit non-destructive boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldViewState {
    workspace_subject: WorkspaceSubject,
    live_session: Option<LiveSessionState>,
    selected_activity_by_session: BTreeMap<(String, String), ActivityKey>,
    inspector: InspectorState,
}

impl FieldViewState {
    /// Creates state from the restored workspace and optional bound Session.
    pub fn new(
        workspace_subject: WorkspaceSubject,
        live_session: Option<ExactSessionSelection>,
    ) -> Self {
        Self {
            workspace_subject,
            live_session: live_session.map(LiveSessionState::Present),
            selected_activity_by_session: BTreeMap::new(),
            inspector: InspectorState::Closed,
        }
    }

    /// Returns the main workspace subject.
    pub fn workspace_subject(&self) -> &WorkspaceSubject {
        &self.workspace_subject
    }

    /// Returns the unchanged live binding identity and current Core presence.
    pub fn live_session(&self) -> Option<&LiveSessionState> {
        self.live_session.as_ref()
    }

    /// Returns the secondary inspector state.
    pub fn inspector(&self) -> &InspectorState {
        &self.inspector
    }

    /// Returns the selected activity retained for an exact Session.
    pub fn selected_activity(&self, session: &ExactSessionSelection) -> Option<&ActivityKey> {
        self.selected_activity_by_session.get(&session_key(session))
    }

    /// Changes only the main workspace subject.
    pub fn show_subject(&mut self, subject: WorkspaceSubject) -> BindingDirective {
        self.workspace_subject = subject;
        BindingDirective::Unchanged
    }

    /// Changes only per-Session activity selection.
    pub fn select_activity(
        &mut self,
        session: &ExactSessionSelection,
        key: ActivityKey,
    ) -> BindingDirective {
        self.selected_activity_by_session
            .insert(session_key(session), key);
        BindingDirective::Unchanged
    }

    /// Opens exact secondary detail without changing its workspace subject.
    pub fn inspect(&mut self, selection: InspectorSelection) -> BindingDirective {
        self.inspector = InspectorState::Open(selection);
        BindingDirective::Unchanged
    }

    /// Closes only secondary detail.
    pub fn close_inspector(&mut self) -> BindingDirective {
        self.inspector = InspectorState::Closed;
        BindingDirective::Unchanged
    }

    /// Reconciles Core presence while retaining the actual live Session identity.
    pub fn reconcile(&mut self, snapshot: &FieldSnapshot) -> BindingDirective {
        if let Some(live) = self.live_session.take() {
            let selection = live.selection().clone();
            self.live_session = Some(if session_exists(snapshot, &selection) {
                LiveSessionState::Present(selection)
            } else {
                LiveSessionState::MissingFromCurrentField(selection)
            });
        }
        BindingDirective::Unchanged
    }

    /// Closes a stale inspector key while preserving workspace and bindings.
    pub fn reconcile_inspector(
        &mut self,
        is_current: impl FnOnce(&InspectorSelection) -> bool,
    ) -> BindingDirective {
        if let InspectorState::Open(selection) = &self.inspector
            && !is_current(selection)
        {
            self.inspector = InspectorState::Closed;
        }
        BindingDirective::Unchanged
    }

    /// Validates one explicit activation without mutating prior state.
    pub fn prepare_activate_session(
        &self,
        snapshot: &FieldSnapshot,
        target: ExactSessionSelection,
    ) -> Result<PreparedSessionActivation, SessionActivationValidationError> {
        let resolution = reconcile_restore(snapshot, Some(&target));
        if !matches!(resolution, RestoreResolution::Exact(_)) {
            return Err(SessionActivationValidationError { resolution });
        }
        let unchanged = self
            .live_session
            .as_ref()
            .is_some_and(|live| live.selection() == &target);
        Ok(PreparedSessionActivation {
            target: target.clone(),
            binding: if unchanged {
                BindingDirective::Unchanged
            } else {
                BindingDirective::ReplaceWith(target)
            },
        })
    }

    /// Commits view/live state only after the binding owner reports success.
    pub fn commit_session_activation(
        &mut self,
        prepared: PreparedSessionActivation,
    ) -> BindingDirective {
        self.live_session = Some(LiveSessionState::Present(prepared.target.clone()));
        self.workspace_subject = WorkspaceSubject::Session(prepared.target);
        BindingDirective::Unchanged
    }
}

fn session_key(selection: &ExactSessionSelection) -> (String, String) {
    (
        selection.plot_id().to_owned(),
        selection.session_id().to_owned(),
    )
}

fn session_exists(snapshot: &FieldSnapshot, selection: &ExactSessionSelection) -> bool {
    snapshot.plots.iter().any(|plot| {
        plot.plot_id == selection.plot_id()
            && plot
                .sessions
                .iter()
                .any(|session| session.session_id == selection.session_id())
    })
}

#[cfg(test)]
mod tests {
    use nopal_feed_client::field::FieldSnapshot;
    use nopal_native_lifecycle::reconcile::ExactSessionSelection;

    use super::{
        BindingDirective, ExecutionIdentity, FieldViewState, InspectorSelection, InspectorState,
        LiveSessionState, WorkspaceSubject,
    };
    use crate::activity::ActivityKey;
    use crate::assurance::AssuranceKey;

    #[test]
    fn subject_activity_and_inspector_interactions_never_retarget_the_live_session() {
        let live = session("plot-a", "session-a");
        let mut state =
            FieldViewState::new(WorkspaceSubject::Session(live.clone()), Some(live.clone()));
        let execution = ExecutionIdentity::new("plot-b", "rondo", "repo-b", "run-b");

        assert_eq!(
            state.show_subject(WorkspaceSubject::Execution(execution.clone())),
            BindingDirective::Unchanged
        );
        let activity = ActivityKey::Activity("tool-1".to_owned());
        assert_eq!(
            state.select_activity(&live, activity.clone()),
            BindingDirective::Unchanged
        );
        assert_eq!(
            state.inspect(InspectorSelection::Activity {
                session: live.clone(),
                key: activity.clone(),
            }),
            BindingDirective::Unchanged
        );
        assert_eq!(
            state.inspect(InspectorSelection::Assurance {
                key: AssuranceKey::Approval {
                    ask_id: "unbound-ask".to_owned(),
                },
            }),
            BindingDirective::Unchanged
        );
        assert_eq!(state.close_inspector(), BindingDirective::Unchanged);

        assert_eq!(
            state.workspace_subject(),
            &WorkspaceSubject::Execution(execution)
        );
        assert_eq!(
            state.live_session(),
            Some(&LiveSessionState::Present(live.clone()))
        );
        assert_eq!(state.selected_activity(&live), Some(&activity));
        assert_eq!(state.inspector(), &InspectorState::Closed);
    }

    #[test]
    fn refresh_marks_a_removed_live_session_missing_without_retargeting() {
        let live = session("plot-a", "session-a");
        let subject = WorkspaceSubject::Plot {
            plot_id: "plot-b".to_owned(),
        };
        let mut state = FieldViewState::new(subject.clone(), Some(live.clone()));

        assert_eq!(
            state.reconcile(&field(&[("plot-b", &["session-b"])])),
            BindingDirective::Unchanged
        );

        assert_eq!(state.workspace_subject(), &subject);
        assert_eq!(
            state.live_session(),
            Some(&LiveSessionState::MissingFromCurrentField(live))
        );
    }

    #[test]
    fn only_explicit_exact_activation_prepares_a_replacement() {
        let old = session("plot-a", "session-a");
        let next = session("plot-b", "session-b");
        let mut state =
            FieldViewState::new(WorkspaceSubject::Session(old.clone()), Some(old.clone()));
        let snapshot = field(&[("plot-a", &["session-a"]), ("plot-b", &["session-b"])]);

        let prepared = state
            .prepare_activate_session(&snapshot, next.clone())
            .expect("exact Core Session should prepare");
        assert_eq!(
            prepared.binding_directive(),
            &BindingDirective::ReplaceWith(next.clone())
        );
        assert_eq!(state.live_session(), Some(&LiveSessionState::Present(old)));

        assert_eq!(
            state.commit_session_activation(prepared),
            BindingDirective::Unchanged
        );
        assert_eq!(
            state.live_session(),
            Some(&LiveSessionState::Present(next.clone()))
        );
        assert_eq!(state.workspace_subject(), &WorkspaceSubject::Session(next));
    }

    #[test]
    fn invalid_activation_does_not_change_any_view_or_live_state() {
        let old = session("plot-a", "session-a");
        let state = FieldViewState::new(WorkspaceSubject::Session(old.clone()), Some(old.clone()));
        let before = state.clone();

        assert!(
            state
                .prepare_activate_session(
                    &field(&[("plot-a", &["session-a"])]),
                    session("plot-a", "invented"),
                )
                .is_err()
        );
        assert_eq!(state, before);
    }

    fn session(plot_id: &str, session_id: &str) -> ExactSessionSelection {
        ExactSessionSelection::new(plot_id, session_id)
    }

    fn field(plots: &[(&str, &[&str])]) -> FieldSnapshot {
        serde_json::from_value(serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": plots
                .iter()
                .map(|(plot_id, sessions)| serde_json::json!({
                    "kind": "nopal.plot/v1",
                    "plot_id": plot_id,
                    "sessions": sessions
                        .iter()
                        .map(|session_id| serde_json::json!({ "session_id": session_id }))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "entries": [],
        }))
        .expect("fixture should satisfy the Field contract")
    }
}
