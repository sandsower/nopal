//! Generation-fenced ownership of the latest accepted Core Field snapshot.

use std::fmt;
use std::io;
use std::sync::Arc;

use nopal_feed_client::field::FieldSnapshot;

use crate::application::{
    NativeSelectionNotWrittenReason, NativeSelectionPersistence, NativeSelectionWriteOutcome,
};
use crate::reconcile::ExactSessionSelection;

/// Monotonic identity of one accepted or requested Core Field snapshot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FieldGeneration(u64);

impl FieldGeneration {
    /// The immutable snapshot loaded during native application startup.
    pub const STARTUP: Self = Self(0);

    /// Creates a generation identity for transport and refresh adapters.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the wire-safe generation value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Unforgeable authority to attempt committing one requested refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldRefreshTicket {
    generation: FieldGeneration,
}

impl FieldRefreshTicket {
    /// Reconstitutes a typed ticket at a refresh transport boundary.
    ///
    /// Acceptance is still authorized by `CurrentCoreFieldAuthority`, which
    /// rejects any generation other than its latest outstanding request.
    pub const fn new(generation: FieldGeneration) -> Self {
        Self { generation }
    }

    /// Returns this request's monotonic generation.
    pub const fn generation(self) -> FieldGeneration {
        self.generation
    }
}

/// A refresh failed before it could atomically replace current Core facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcceptFieldError<E> {
    /// A newer request exists or this ticket was already accepted.
    Stale {
        /// Generation carried by the rejected ticket.
        ticket: FieldGeneration,
        /// Newest generation requested from Core.
        latest_requested: FieldGeneration,
    },
    /// The consumer could not prepare a complete projection from the candidate.
    Prepare(E),
}

impl<E: fmt::Display> fmt::Display for AcceptFieldError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stale {
                ticket,
                latest_requested,
            } => write!(
                formatter,
                "Field refresh generation {} is stale; latest requested generation is {}",
                ticket.get(),
                latest_requested.get()
            ),
            Self::Prepare(error) => {
                write!(formatter, "Field projection preparation failed: {error}")
            }
        }
    }
}

impl<E> std::error::Error for AcceptFieldError<E> where E: std::error::Error + 'static {}

/// Why an exact Session selection was not persisted against current Core facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CurrentSelectionNotWrittenReason {
    /// The caller observed an older accepted Field generation.
    StaleGeneration {
        /// Generation the caller used for validation.
        expected: FieldGeneration,
        /// Generation currently accepted by the application.
        accepted: FieldGeneration,
    },
    /// The selection was not exact in the latest accepted snapshot or storage rejected it.
    SelectionNotCurrent(NativeSelectionNotWrittenReason),
}

/// Generation-fenced result of persisting one exact live Session selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CurrentSelectionWriteOutcome {
    /// The exact selection was atomically persisted.
    Written,
    /// No preference mutation occurred.
    NotWritten(CurrentSelectionNotWrittenReason),
}

/// Sole mutable authority for the Core Field facts accepted by a native host.
///
/// The startup snapshot is moved into this authority for live use while the
/// application separately retains immutable startup evidence. A candidate is
/// committed only after its consumer projection has been prepared completely.
pub struct CurrentCoreFieldAuthority {
    accepted_generation: FieldGeneration,
    latest_requested_generation: FieldGeneration,
    accepted: Arc<FieldSnapshot>,
    persistence: NativeSelectionPersistence,
}

impl CurrentCoreFieldAuthority {
    /// Creates live authority from the exact startup snapshot and persistence scope.
    pub fn from_startup(snapshot: FieldSnapshot, persistence: NativeSelectionPersistence) -> Self {
        Self {
            accepted_generation: FieldGeneration::STARTUP,
            latest_requested_generation: FieldGeneration::STARTUP,
            accepted: Arc::new(snapshot),
            persistence,
        }
    }

    /// Returns the generation of the last snapshot committed after preparation.
    pub const fn accepted_generation(&self) -> FieldGeneration {
        self.accepted_generation
    }

    /// Returns the newest requested refresh generation.
    pub const fn latest_requested_generation(&self) -> FieldGeneration {
        self.latest_requested_generation
    }

    /// Returns shared immutable access to the latest accepted Core facts.
    pub fn accepted(&self) -> &Arc<FieldSnapshot> {
        &self.accepted
    }

    #[cfg(test)]
    pub(crate) fn persistence(&self) -> &NativeSelectionPersistence {
        &self.persistence
    }

    /// Begins a newer refresh and invalidates all older outstanding tickets.
    pub fn begin_refresh(&mut self) -> FieldRefreshTicket {
        let next = self.latest_requested_generation.get().saturating_add(1);
        self.latest_requested_generation = FieldGeneration(next);
        FieldRefreshTicket {
            generation: self.latest_requested_generation,
        }
    }

    /// Prepares a complete consumer value, then atomically accepts both it and the snapshot.
    ///
    /// Stale candidates never execute `prepare`. A preparation error leaves the
    /// prior generation, snapshot, and persistence validator untouched.
    pub fn accept_with<T, E>(
        &mut self,
        ticket: FieldRefreshTicket,
        snapshot: FieldSnapshot,
        prepare: impl FnOnce(&FieldSnapshot) -> Result<T, E>,
    ) -> Result<T, AcceptFieldError<E>> {
        if ticket.generation != self.latest_requested_generation
            || ticket.generation <= self.accepted_generation
        {
            return Err(AcceptFieldError::Stale {
                ticket: ticket.generation,
                latest_requested: self.latest_requested_generation,
            });
        }

        let prepared = prepare(&snapshot).map_err(AcceptFieldError::Prepare)?;
        self.persistence.replace_field(snapshot.clone());
        self.accepted = Arc::new(snapshot);
        self.accepted_generation = ticket.generation;
        Ok(prepared)
    }

    /// Persists a Session only against the exact generation already shown to the caller.
    pub fn persist_session(
        &self,
        expected_generation: FieldGeneration,
        selection: &ExactSessionSelection,
    ) -> io::Result<CurrentSelectionWriteOutcome> {
        if expected_generation != self.accepted_generation {
            return Ok(CurrentSelectionWriteOutcome::NotWritten(
                CurrentSelectionNotWrittenReason::StaleGeneration {
                    expected: expected_generation,
                    accepted: self.accepted_generation,
                },
            ));
        }

        self.persistence
            .select(selection)
            .map(|outcome| match outcome {
                NativeSelectionWriteOutcome::Written => CurrentSelectionWriteOutcome::Written,
                NativeSelectionWriteOutcome::NotWritten(reason) => {
                    CurrentSelectionWriteOutcome::NotWritten(
                        CurrentSelectionNotWrittenReason::SelectionNotCurrent(reason),
                    )
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use nopal_feed_client::field::FieldSnapshot;
    use tempfile::tempdir;

    use super::{
        AcceptFieldError, CurrentCoreFieldAuthority, CurrentSelectionNotWrittenReason,
        CurrentSelectionWriteOutcome, FieldGeneration,
    };
    use crate::application::NativeSelectionPersistence;
    use crate::reconcile::ExactSessionSelection;
    use crate::state_root::{CanonicalStateRoot, NativeInstanceScope, ReleaseChannel};

    #[test]
    fn stale_ticket_never_prepares_or_commits() {
        let mut authority = authority(field(&[("plot-a", &["session-a"])]));
        let stale = authority.begin_refresh();
        let latest = authority.begin_refresh();
        let prepare_calls = Cell::new(0);

        let result = authority.accept_with(
            stale,
            field(&[("plot-b", &["session-b"])]),
            |_| -> Result<(), &'static str> {
                prepare_calls.set(prepare_calls.get() + 1);
                Ok(())
            },
        );

        assert_eq!(
            result,
            Err(AcceptFieldError::Stale {
                ticket: stale.generation(),
                latest_requested: latest.generation(),
            })
        );
        assert_eq!(prepare_calls.get(), 0);
        assert_eq!(authority.accepted_generation(), FieldGeneration::STARTUP);
        assert_eq!(authority.accepted().plots[0].plot_id, "plot-a");
    }

    #[test]
    fn preparation_failure_retains_the_prior_snapshot_and_generation() {
        let mut authority = authority(field(&[("plot-a", &["session-a"])]));
        let ticket = authority.begin_refresh();

        let result = authority.accept_with(
            ticket,
            field(&[("plot-b", &["session-b"])]),
            |_| -> Result<(), &'static str> { Err("projection rejected") },
        );

        assert_eq!(
            result,
            Err(AcceptFieldError::Prepare("projection rejected"))
        );
        assert_eq!(authority.accepted_generation(), FieldGeneration::STARTUP);
        assert_eq!(authority.accepted().plots[0].plot_id, "plot-a");
    }

    #[test]
    fn successful_preparation_commits_snapshot_before_returning_projection() {
        let mut authority = authority(field(&[("plot-a", &["session-a"])]));
        let ticket = authority.begin_refresh();

        let projection = authority
            .accept_with(ticket, field(&[("plot-b", &["session-b"])]), |snapshot| {
                Ok::<_, ()>(snapshot.plots[0].plot_id.clone())
            })
            .expect("latest prepared snapshot should commit");

        assert_eq!(projection, "plot-b");
        assert_eq!(authority.accepted_generation(), ticket.generation());
        assert_eq!(authority.accepted().plots[0].plot_id, "plot-b");
    }

    #[test]
    fn persistence_uses_only_the_latest_accepted_generation_and_snapshot() {
        let mut authority = authority(field(&[("plot-a", &["session-a"])]));
        let ticket = authority.begin_refresh();
        authority
            .accept_with(
                ticket,
                field(&[("plot-a", &["session-a", "session-new"])]),
                |_| Ok::<_, ()>(()),
            )
            .expect("latest refresh should commit");

        let newly_appeared = ExactSessionSelection::new("plot-a", "session-new");
        assert_eq!(
            authority
                .persist_session(ticket.generation(), &newly_appeared)
                .expect("current selection write should complete"),
            CurrentSelectionWriteOutcome::Written
        );
        assert_eq!(
            authority
                .persist_session(FieldGeneration::STARTUP, &newly_appeared)
                .expect("stale selection write should be rejected without I/O"),
            CurrentSelectionWriteOutcome::NotWritten(
                CurrentSelectionNotWrittenReason::StaleGeneration {
                    expected: FieldGeneration::STARTUP,
                    accepted: ticket.generation(),
                }
            )
        );
        let removed = ExactSessionSelection::new("plot-a", "removed");
        assert!(matches!(
            authority
                .persist_session(ticket.generation(), &removed)
                .expect("non-current selection should be rejected without mutation"),
            CurrentSelectionWriteOutcome::NotWritten(
                CurrentSelectionNotWrittenReason::SelectionNotCurrent(_)
            )
        ));
    }

    fn authority(snapshot: FieldSnapshot) -> CurrentCoreFieldAuthority {
        let directory = tempdir().expect("create temporary state root");
        let root = CanonicalStateRoot::create(directory.keep()).expect("canonicalize state root");
        let scope = NativeInstanceScope::new(root, ReleaseChannel::Stable);
        let persistence = NativeSelectionPersistence::for_scope(&scope, &snapshot);
        CurrentCoreFieldAuthority::from_startup(snapshot, persistence)
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
