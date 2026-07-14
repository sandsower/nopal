//! Atomic coordination of current Core facts, projection, view state, and activation.

use std::fmt;
use std::io;

use nopal_feed_client::field::FieldSnapshot;
use nopal_native_lifecycle::current_field::{
    AcceptFieldError, CurrentCoreFieldAuthority, CurrentSelectionNotWrittenReason,
    CurrentSelectionWriteOutcome, FieldGeneration, FieldRefreshTicket,
};
use nopal_native_lifecycle::reconcile::ExactSessionSelection;

use crate::assurance::{
    AssuranceKey, DesktopAssuranceModel, DetailsProjection, DetailsUiState, ProjectionError,
};
use crate::field_refresh::{FieldLoadError, FieldRefreshUpdate};
use crate::view_state::{
    BindingDirective, FieldViewState, InspectorSelection, InspectorState,
    SessionActivationValidationError,
};

/// Complete snapshot-derived presentation prepared before current facts are accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldProjection {
    assurance: DetailsProjection,
}

impl FieldProjection {
    fn prepare(
        snapshot: &FieldSnapshot,
        previous_ui: &DetailsUiState,
    ) -> Result<Self, ProjectionError> {
        let model = DesktopAssuranceModel::from_snapshot(snapshot);
        DetailsProjection::project(&model, previous_ui).map(|assurance| Self { assurance })
    }

    /// Returns typed Core assurance facts and exact-key UI state.
    pub fn assurance(&self) -> &DetailsProjection {
        &self.assurance
    }

    fn contains_assurance(&self, key: &AssuranceKey) -> bool {
        self.assurance
            .plots
            .iter()
            .flat_map(|plot| &plot.facts)
            .chain(&self.assurance.unbound)
            .any(|fact| fact.key() == key)
    }
}

/// Honest freshness state while the last accepted snapshot remains visible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldFreshness {
    /// Presentation and accepted Core facts share this generation.
    Current { accepted: FieldGeneration },
    /// The latest request failed before returning a candidate snapshot.
    LoadFailed {
        accepted: FieldGeneration,
        requested: FieldGeneration,
        error: FieldLoadError,
    },
    /// Core returned a candidate that could not produce a complete projection.
    ProjectionRejected {
        accepted: FieldGeneration,
        requested: FieldGeneration,
        error: ProjectionError,
    },
}

/// Result of applying one bounded refresh update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyFieldRefreshOutcome {
    /// The candidate and complete projection became current together.
    Applied {
        generation: FieldGeneration,
        binding: BindingDirective,
    },
    /// A superseded result was ignored without preparing or mutating anything.
    IgnoredStale {
        generation: FieldGeneration,
        latest_requested: FieldGeneration,
    },
    /// The latest load failed and the last good state remains accepted.
    LoadFailed {
        generation: FieldGeneration,
        error: FieldLoadError,
    },
    /// Projection rejected the candidate and the last good state remains accepted.
    ProjectionRejected {
        generation: FieldGeneration,
        error: ProjectionError,
    },
}

/// Explicit Session activation failed without inventing a replacement target.
#[derive(Debug)]
pub enum SessionActivationError<E> {
    /// The requested pair was not exact in current Core facts.
    InvalidTarget(SessionActivationValidationError),
    /// The binding owner could not atomically replace its live Session resources.
    Binding(E),
}

/// A Session activation committed in memory, with an explicit persistence result.
#[derive(Debug)]
pub enum SessionActivationOutcome {
    Persisted,
    PersistenceNotWritten(CurrentSelectionNotWrittenReason),
    PersistenceFailed(io::Error),
}

impl<E: fmt::Display> fmt::Display for SessionActivationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(_) => {
                formatter.write_str("Session is not exact in current Core facts")
            }
            Self::Binding(error) => {
                write!(formatter, "Session binding replacement failed: {error}")
            }
        }
    }
}

impl<E> std::error::Error for SessionActivationError<E> where E: std::error::Error + 'static {}

/// Sole coordinator for accepted Core facts and renderer-neutral Field interaction state.
pub struct FieldPresentationCoordinator {
    authority: CurrentCoreFieldAuthority,
    projection: FieldProjection,
    view: FieldViewState,
    freshness: FieldFreshness,
}

impl FieldPresentationCoordinator {
    /// Prepares the startup projection from the authority's already accepted snapshot.
    pub fn new(
        authority: CurrentCoreFieldAuthority,
        view: FieldViewState,
    ) -> Result<Self, ProjectionError> {
        let projection =
            FieldProjection::prepare(authority.accepted(), &DetailsUiState::default())?;
        let freshness = FieldFreshness::Current {
            accepted: authority.accepted_generation(),
        };
        Ok(Self {
            authority,
            projection,
            view,
            freshness,
        })
    }

    /// Returns the latest accepted Core facts.
    pub fn accepted(&self) -> &FieldSnapshot {
        self.authority.accepted()
    }

    /// Returns the generation shared by current facts and projection.
    pub fn accepted_generation(&self) -> FieldGeneration {
        self.authority.accepted_generation()
    }

    /// Returns the complete current projection.
    pub fn projection(&self) -> &FieldProjection {
        &self.projection
    }

    /// Returns independent workspace, inspector, and live Session state.
    pub fn view(&self) -> &FieldViewState {
        &self.view
    }

    /// Mutates renderer-neutral interactions that cannot replace bindings by themselves.
    pub fn view_mut(&mut self) -> &mut FieldViewState {
        &mut self.view
    }

    /// Returns whether current facts are fresh or retained after a failed request.
    pub fn freshness(&self) -> &FieldFreshness {
        &self.freshness
    }

    /// Allocates the sole ticket accepted for the newest refresh request.
    pub fn begin_refresh(&mut self) -> FieldRefreshTicket {
        self.authority.begin_refresh()
    }

    /// Applies a loaded snapshot and projection atomically, or retains all last-good state.
    pub fn apply_refresh(&mut self, update: FieldRefreshUpdate) -> ApplyFieldRefreshOutcome {
        match update {
            FieldRefreshUpdate::Failed { ticket, error } => {
                if ticket.generation() != self.authority.latest_requested_generation()
                    || ticket.generation() <= self.authority.accepted_generation()
                {
                    return ApplyFieldRefreshOutcome::IgnoredStale {
                        generation: ticket.generation(),
                        latest_requested: self.authority.latest_requested_generation(),
                    };
                }
                self.freshness = FieldFreshness::LoadFailed {
                    accepted: self.authority.accepted_generation(),
                    requested: ticket.generation(),
                    error: error.clone(),
                };
                ApplyFieldRefreshOutcome::LoadFailed {
                    generation: ticket.generation(),
                    error,
                }
            }
            FieldRefreshUpdate::Loaded { ticket, snapshot } => {
                let previous_ui = self.projection.assurance.ui.clone();
                let accepted = self.authority.accept_with(ticket, snapshot, |candidate| {
                    FieldProjection::prepare(candidate, &previous_ui)
                });
                match accepted {
                    Ok(projection) => {
                        let generation = self.authority.accepted_generation();
                        self.projection = projection;
                        let binding = self.view.reconcile(self.authority.accepted());
                        let projection = &self.projection;
                        self.view.reconcile_inspector(|selection| match selection {
                            InspectorSelection::Activity { .. } => true,
                            InspectorSelection::Assurance { key } => {
                                projection.contains_assurance(key)
                            }
                        });
                        self.freshness = FieldFreshness::Current {
                            accepted: generation,
                        };
                        ApplyFieldRefreshOutcome::Applied {
                            generation,
                            binding,
                        }
                    }
                    Err(AcceptFieldError::Stale {
                        ticket,
                        latest_requested,
                    }) => ApplyFieldRefreshOutcome::IgnoredStale {
                        generation: ticket,
                        latest_requested,
                    },
                    Err(AcceptFieldError::Prepare(error)) => {
                        self.freshness = FieldFreshness::ProjectionRejected {
                            accepted: self.authority.accepted_generation(),
                            requested: ticket.generation(),
                            error: error.clone(),
                        };
                        ApplyFieldRefreshOutcome::ProjectionRejected {
                            generation: ticket.generation(),
                            error,
                        }
                    }
                }
            }
        }
    }

    /// Replaces bindings only after exact validation, then commits view and persistence.
    pub fn activate_session<E>(
        &mut self,
        target: ExactSessionSelection,
        replace_binding: impl FnOnce(&ExactSessionSelection) -> Result<(), E>,
    ) -> Result<SessionActivationOutcome, SessionActivationError<E>> {
        let prepared = self
            .view
            .prepare_activate_session(self.authority.accepted(), target.clone())
            .map_err(SessionActivationError::InvalidTarget)?;
        if matches!(
            prepared.binding_directive(),
            BindingDirective::ReplaceWith(_)
        ) {
            replace_binding(&target).map_err(SessionActivationError::Binding)?;
        }
        self.view.commit_session_activation(prepared);
        Ok(
            match self
                .authority
                .persist_session(self.authority.accepted_generation(), &target)
            {
                Ok(CurrentSelectionWriteOutcome::Written) => SessionActivationOutcome::Persisted,
                Ok(CurrentSelectionWriteOutcome::NotWritten(reason)) => {
                    SessionActivationOutcome::PersistenceNotWritten(reason)
                }
                Err(error) => SessionActivationOutcome::PersistenceFailed(error),
            },
        )
    }

    /// Returns whether the inspector is currently closed without exposing renderer state.
    pub fn inspector_is_closed(&self) -> bool {
        matches!(self.view.inspector(), InspectorState::Closed)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use nopal_native_lifecycle::application::NativeSelectionPersistence;
    use nopal_native_lifecycle::preferences::{
        RestorePreferenceReadOutcome, RestorePreferenceStore,
    };
    use tempfile::tempdir;

    use super::*;
    use crate::field_refresh::{FieldLoadErrorKind, FieldRefreshUpdate};
    use crate::view_state::{LiveSessionState, WorkspaceSubject};

    #[test]
    fn stale_and_projection_rejected_refreshes_retain_prior_accepted_state() {
        let (mut coordinator, _) = coordinator(field(&[("plot-a", &["session-a"])]));
        let stale = coordinator.begin_refresh();
        let latest = coordinator.begin_refresh();
        let prior = coordinator.projection().clone();

        assert!(matches!(
            coordinator.apply_refresh(FieldRefreshUpdate::Loaded {
                ticket: stale,
                snapshot: field(&[("plot-stale", &["session-stale"])]),
            }),
            ApplyFieldRefreshOutcome::IgnoredStale { .. }
        ));
        assert!(matches!(
            coordinator.apply_refresh(FieldRefreshUpdate::Loaded {
                ticket: latest,
                snapshot: duplicate_plot_field(),
            }),
            ApplyFieldRefreshOutcome::ProjectionRejected { .. }
        ));
        assert_eq!(coordinator.accepted_generation(), FieldGeneration::STARTUP);
        assert_eq!(coordinator.accepted().plots[0].plot_id, "plot-a");
        assert_eq!(coordinator.projection(), &prior);
    }

    #[test]
    fn refresh_and_workspace_navigation_never_replace_the_live_binding() {
        let (mut coordinator, _) = coordinator(field(&[
            ("plot-a", &["session-a"]),
            ("plot-b", &["session-b"]),
        ]));
        assert_eq!(
            coordinator.view_mut().show_subject(WorkspaceSubject::Plot {
                plot_id: "plot-b".to_owned(),
            }),
            BindingDirective::Unchanged
        );
        let ticket = coordinator.begin_refresh();
        assert!(matches!(
            coordinator.apply_refresh(FieldRefreshUpdate::Loaded {
                ticket,
                snapshot: field(&[("plot-b", &["session-b"])]),
            }),
            ApplyFieldRefreshOutcome::Applied {
                binding: BindingDirective::Unchanged,
                ..
            }
        ));
        assert_eq!(
            coordinator.view().live_session(),
            Some(&LiveSessionState::MissingFromCurrentField(session(
                "plot-a",
                "session-a"
            )))
        );
    }

    #[test]
    fn latest_load_failure_marks_stale_without_mutating_last_good_state() {
        let (mut coordinator, _) = coordinator(field(&[("plot-a", &["session-a"])]));
        let ticket = coordinator.begin_refresh();
        let error = FieldLoadError::new(FieldLoadErrorKind::NonzeroExit, "Core inspect failed");

        assert!(matches!(
            coordinator.apply_refresh(FieldRefreshUpdate::Failed {
                ticket,
                error: error.clone(),
            }),
            ApplyFieldRefreshOutcome::LoadFailed { .. }
        ));
        assert_eq!(coordinator.accepted_generation(), FieldGeneration::STARTUP);
        assert_eq!(
            coordinator.freshness(),
            &FieldFreshness::LoadFailed {
                accepted: FieldGeneration::STARTUP,
                requested: ticket.generation(),
                error,
            }
        );
    }

    #[test]
    fn failed_binding_retains_old_session_and_success_replaces_then_persists() {
        let (mut coordinator, store) = coordinator(field(&[
            ("plot-a", &["session-a"]),
            ("plot-b", &["session-b"]),
        ]));
        let target = session("plot-b", "session-b");
        let calls = Cell::new(0);

        let failed = coordinator.activate_session(target.clone(), |_| {
            calls.set(calls.get() + 1);
            Err("binding failed")
        });
        assert!(matches!(
            failed,
            Err(SessionActivationError::Binding("binding failed"))
        ));
        assert_eq!(calls.get(), 1);
        assert_eq!(
            coordinator.view().live_session(),
            Some(&LiveSessionState::Present(session("plot-a", "session-a")))
        );

        assert!(matches!(
            coordinator
                .activate_session(target.clone(), |_| {
                    calls.set(calls.get() + 1);
                    Ok::<_, &'static str>(())
                })
                .expect("exact replacement should bind and persist"),
            SessionActivationOutcome::Persisted
        ));
        assert_eq!(calls.get(), 2);
        assert!(matches!(
            store.read().expect("read persisted preference"),
            RestorePreferenceReadOutcome::Ready(preference)
                if preference.selection == Some(target)
        ));
    }

    #[test]
    fn persistence_failure_reports_a_committed_activation() {
        let (mut coordinator, store) = coordinator(field(&[
            ("plot-a", &["session-a"]),
            ("plot-b", &["session-b"]),
        ]));
        let target = session("plot-b", "session-b");
        let parent = store.path().parent().expect("preference parent");
        std::fs::remove_dir_all(parent).expect("remove preference parent");
        std::fs::write(parent, b"blocked").expect("replace preference parent with a file");

        let outcome = coordinator
            .activate_session(target.clone(), |_| Ok::<_, &'static str>(()))
            .expect("binding and view activation must commit");

        assert!(
            matches!(outcome, SessionActivationOutcome::PersistenceNotWritten(_)),
            "unexpected activation outcome: {outcome:?}"
        );
        assert_eq!(
            coordinator.view().live_session(),
            Some(&LiveSessionState::Present(target))
        );
    }

    fn coordinator(
        snapshot: FieldSnapshot,
    ) -> (FieldPresentationCoordinator, RestorePreferenceStore) {
        let directory = tempdir().expect("create preference sandbox").keep();
        let store = RestorePreferenceStore::new(directory.join("restore.json"));
        let persistence = NativeSelectionPersistence::for_restore_path(store.path(), &snapshot);
        let authority = CurrentCoreFieldAuthority::from_startup(snapshot, persistence);
        let selected = session("plot-a", "session-a");
        let view = FieldViewState::new(WorkspaceSubject::Session(selected.clone()), Some(selected));
        (
            FieldPresentationCoordinator::new(authority, view)
                .expect("startup projection should be valid"),
            store,
        )
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

    fn duplicate_plot_field() -> FieldSnapshot {
        field(&[
            ("plot-duplicate", &["session-a"]),
            ("plot-duplicate", &["session-b"]),
        ])
    }
}
