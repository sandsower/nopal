//! Exact reconciliation of desktop restore intent with Core Field facts.

use std::collections::BTreeSet;

use nopal_feed_client::field::{FieldPlot, FieldSnapshot};

/// Desktop intent for one exact Core-owned Plot and Session pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactSessionSelection {
    plot_id: String,
    session_id: String,
}

impl ExactSessionSelection {
    /// Creates a selection intent without asserting that either identity exists.
    pub fn new(plot_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            plot_id: plot_id.into(),
            session_id: session_id.into(),
        }
    }

    /// Returns the intended Core Plot identity.
    pub fn plot_id(&self) -> &str {
        &self.plot_id
    }

    /// Returns the intended Core Session identity.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// A selection derived exclusively from one immutable Core Field snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreSelection {
    /// One Session that belongs to its paired Plot in the same snapshot.
    Session(ExactSessionSelection),
    /// A Plot without an available Session.
    PlotOnly {
        /// Core Plot identity.
        plot_id: String,
    },
}

impl RestoreSelection {
    /// Returns the selected Plot identity.
    pub fn plot_id(&self) -> &str {
        match self {
            Self::Session(selection) => selection.plot_id(),
            Self::PlotOnly { plot_id } => plot_id,
        }
    }

    /// Returns the selected Session identity when the resolution has one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Session(selection) => Some(selection.session_id()),
            Self::PlotOnly { .. } => None,
        }
    }
}

/// Why exact restoration was unavailable or unsafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreFallbackReason {
    /// No desktop restore intent was available.
    NoPreviousSelection,
    /// The intended Plot is absent from the Field snapshot.
    PlotMissing {
        /// Missing Plot identity.
        plot_id: String,
    },
    /// The intended Session is absent from its intended Plot and every other Plot.
    SessionMissing {
        /// Intended Plot identity.
        plot_id: String,
        /// Missing Session identity.
        session_id: String,
    },
    /// The intended Session exists, but belongs to a different Plot.
    SessionBelongsToAnotherPlot {
        /// Plot stored by the desktop preference.
        requested_plot_id: String,
        /// Plot that Core actually associates with the Session.
        actual_plot_id: String,
        /// Core Session identity.
        session_id: String,
    },
    /// Core supplied the same Plot identity more than once.
    DuplicatePlotIdentity {
        /// Ambiguous Plot identity.
        plot_id: String,
    },
    /// Core supplied the same Session identity more than once.
    DuplicateSessionIdentity {
        /// Ambiguous Session identity.
        session_id: String,
    },
    /// The Field has no Plot to select.
    NoPlotsAvailable,
}

impl RestoreFallbackReason {
    /// Returns a copy suitable for a visible native restore notice.
    pub fn visible_message(&self) -> String {
        match self {
            Self::NoPreviousSelection => {
                "No previous native selection was available. Nopal selected the first available Plot using Core's Field order."
                    .to_owned()
            }
            Self::PlotMissing { plot_id } => format!(
                "The previously selected Plot '{plot_id}' is no longer available. Nopal selected the first available Plot using Core's Field order."
            ),
            Self::SessionMissing {
                plot_id,
                session_id,
            } => format!(
                "The previously selected Session '{session_id}' is no longer available under Plot '{plot_id}'. Nopal selected a fallback using Core's Field order."
            ),
            Self::SessionBelongsToAnotherPlot {
                requested_plot_id,
                actual_plot_id,
                session_id,
            } => format!(
                "Session '{session_id}' now belongs to Plot '{actual_plot_id}', not the previously selected Plot '{requested_plot_id}'. Nopal selected a fallback using Core's Field order."
            ),
            Self::DuplicatePlotIdentity { plot_id } => format!(
                "Core reported Plot identity '{plot_id}' more than once. Nopal did not restore a selection."
            ),
            Self::DuplicateSessionIdentity { session_id } => format!(
                "Core reported Session identity '{session_id}' more than once. Nopal did not restore a selection."
            ),
            Self::NoPlotsAvailable => {
                "Core's Field has no available Plot. Nopal did not restore a selection.".to_owned()
            }
        }
    }
}

/// Exact restore, explicit deterministic fallback, or fail-closed unavailability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreResolution {
    /// The complete desktop-intended pair exists unchanged in Core.
    Exact(ExactSessionSelection),
    /// Exact restoration failed and Core facts selected a deterministic target.
    Fallback {
        /// Target derived from Core contract order and Core's selected Session fact.
        selection: RestoreSelection,
        /// Visible reason exact restore was not used.
        reason: RestoreFallbackReason,
    },
    /// No safe selection can be made.
    Unavailable {
        /// Visible fail-closed reason.
        reason: RestoreFallbackReason,
    },
}

impl RestoreResolution {
    /// Returns the typed reason when exact restoration was not used.
    pub fn fallback_reason(&self) -> Option<&RestoreFallbackReason> {
        match self {
            Self::Exact(_) => None,
            Self::Fallback { reason, .. } | Self::Unavailable { reason } => Some(reason),
        }
    }

    /// Returns the reason to show when the exact desktop intent was not restored.
    pub fn visible_reason(&self) -> Option<String> {
        self.fallback_reason()
            .map(RestoreFallbackReason::visible_message)
    }
}

/// Reconciles desktop intent against one immutable Core Field snapshot.
///
/// Snapshot identities are validated before the intent is considered. Any duplicate Plot or
/// Session identity fails closed. Stale intent is discarded as a whole, then fallback uses the
/// first Plot in contract order, its valid Core-selected Session, its first Session, or Plot-only.
pub fn reconcile_restore(
    field: &FieldSnapshot,
    intent: Option<&ExactSessionSelection>,
) -> RestoreResolution {
    if let Some(reason) = duplicate_identity_reason(field) {
        return RestoreResolution::Unavailable { reason };
    }

    let fallback_reason = match intent {
        None => RestoreFallbackReason::NoPreviousSelection,
        Some(intent) => match exact_intent_failure(field, intent) {
            None => return RestoreResolution::Exact(intent.clone()),
            Some(reason) => reason,
        },
    };

    match field.plots.first() {
        Some(plot) => RestoreResolution::Fallback {
            selection: fallback_selection(plot),
            reason: fallback_reason,
        },
        None => RestoreResolution::Unavailable {
            reason: RestoreFallbackReason::NoPlotsAvailable,
        },
    }
}

fn duplicate_identity_reason(field: &FieldSnapshot) -> Option<RestoreFallbackReason> {
    let mut plot_ids = BTreeSet::new();
    for plot in &field.plots {
        if !plot_ids.insert(plot.plot_id.as_str()) {
            return Some(RestoreFallbackReason::DuplicatePlotIdentity {
                plot_id: plot.plot_id.clone(),
            });
        }
    }

    let mut session_ids = BTreeSet::new();
    for plot in &field.plots {
        for session in &plot.sessions {
            if !session_ids.insert(session.session_id.as_str()) {
                return Some(RestoreFallbackReason::DuplicateSessionIdentity {
                    session_id: session.session_id.clone(),
                });
            }
        }
    }

    None
}

fn exact_intent_failure(
    field: &FieldSnapshot,
    intent: &ExactSessionSelection,
) -> Option<RestoreFallbackReason> {
    let intended_plot = match field
        .plots
        .iter()
        .find(|plot| plot.plot_id == intent.plot_id)
    {
        Some(plot) => plot,
        None => {
            return Some(RestoreFallbackReason::PlotMissing {
                plot_id: intent.plot_id.clone(),
            });
        }
    };

    if intended_plot
        .sessions
        .iter()
        .any(|session| session.session_id == intent.session_id)
    {
        return None;
    }

    match field.plots.iter().find(|plot| {
        plot.sessions
            .iter()
            .any(|session| session.session_id == intent.session_id)
    }) {
        Some(actual_plot) => Some(RestoreFallbackReason::SessionBelongsToAnotherPlot {
            requested_plot_id: intent.plot_id.clone(),
            actual_plot_id: actual_plot.plot_id.clone(),
            session_id: intent.session_id.clone(),
        }),
        None => Some(RestoreFallbackReason::SessionMissing {
            plot_id: intent.plot_id.clone(),
            session_id: intent.session_id.clone(),
        }),
    }
}

fn fallback_selection(plot: &FieldPlot) -> RestoreSelection {
    let selected_session = plot.selected_session_id.as_deref().and_then(|selected_id| {
        plot.sessions
            .iter()
            .find(|session| session.session_id == selected_id)
    });
    match selected_session.or_else(|| plot.sessions.first()) {
        Some(session) => RestoreSelection::Session(ExactSessionSelection::new(
            plot.plot_id.clone(),
            session.session_id.clone(),
        )),
        None => RestoreSelection::PlotOnly {
            plot_id: plot.plot_id.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use nopal_feed_client::field::FieldSnapshot;

    use super::{
        ExactSessionSelection, RestoreFallbackReason, RestoreResolution, RestoreSelection,
        reconcile_restore,
    };

    #[test]
    fn restores_only_the_exact_plot_and_session_pair() {
        let field = field(&[("plot-a", Some("session-a2"), &["session-a1", "session-a2"])]);
        let intent = ExactSessionSelection::new("plot-a", "session-a2");

        assert_eq!(
            reconcile_restore(&field, Some(&intent)),
            RestoreResolution::Exact(intent)
        );
    }

    #[test]
    fn missing_plot_uses_deterministic_field_fallback_with_visible_reason() {
        let field = field(&[("plot-a", Some("session-a2"), &["session-a1", "session-a2"])]);
        let intent = ExactSessionSelection::new("missing", "session-a2");

        let resolution = reconcile_restore(&field, Some(&intent));

        assert_eq!(
            resolution,
            RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-a",
                    "session-a2",
                )),
                reason: RestoreFallbackReason::PlotMissing {
                    plot_id: "missing".to_owned(),
                },
            }
        );
        assert_eq!(
            resolution.visible_reason().as_deref(),
            Some(
                "The previously selected Plot 'missing' is no longer available. Nopal selected the first available Plot using Core's Field order."
            )
        );
    }

    #[test]
    fn missing_session_does_not_partially_restore_the_intended_plot() {
        let field = field(&[
            ("plot-first", Some("session-first"), &["session-first"]),
            ("plot-intended", None, &["session-other"]),
        ]);
        let intent = ExactSessionSelection::new("plot-intended", "missing");

        assert_eq!(
            reconcile_restore(&field, Some(&intent)),
            RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-first",
                    "session-first",
                )),
                reason: RestoreFallbackReason::SessionMissing {
                    plot_id: "plot-intended".to_owned(),
                    session_id: "missing".to_owned(),
                },
            }
        );
    }

    #[test]
    fn session_under_another_plot_is_not_restored_across_plot_boundaries() {
        let field = field(&[
            ("plot-a", None, &["session-a"]),
            ("plot-b", None, &["session-b"]),
        ]);
        let intent = ExactSessionSelection::new("plot-a", "session-b");

        assert_eq!(
            reconcile_restore(&field, Some(&intent)),
            RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-a",
                    "session-a",
                )),
                reason: RestoreFallbackReason::SessionBelongsToAnotherPlot {
                    requested_plot_id: "plot-a".to_owned(),
                    actual_plot_id: "plot-b".to_owned(),
                    session_id: "session-b".to_owned(),
                },
            }
        );
    }

    #[test]
    fn duplicate_plot_identities_fail_closed_without_selecting_anything() {
        let field = field(&[("plot-a", None, &[]), ("plot-a", None, &[])]);

        assert_eq!(
            reconcile_restore(&field, None),
            RestoreResolution::Unavailable {
                reason: RestoreFallbackReason::DuplicatePlotIdentity {
                    plot_id: "plot-a".to_owned(),
                },
            }
        );
    }

    #[test]
    fn duplicate_session_identities_fail_closed_without_selecting_anything() {
        let field = field(&[
            ("plot-a", None, &["session-duplicate"]),
            ("plot-b", None, &["session-duplicate"]),
        ]);

        assert_eq!(
            reconcile_restore(&field, None),
            RestoreResolution::Unavailable {
                reason: RestoreFallbackReason::DuplicateSessionIdentity {
                    session_id: "session-duplicate".to_owned(),
                },
            }
        );
    }

    #[test]
    fn fallback_uses_contract_plot_order_then_core_selected_session() {
        let field = field(&[
            (
                "plot-first",
                Some("session-selected"),
                &["session-one", "session-selected"],
            ),
            ("plot-second", Some("session-second"), &["session-second"]),
        ]);

        assert_eq!(
            reconcile_restore(&field, None),
            RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-first",
                    "session-selected",
                )),
                reason: RestoreFallbackReason::NoPreviousSelection,
            }
        );
    }

    #[test]
    fn fallback_uses_first_session_when_core_has_no_valid_selection() {
        let field = field(&[(
            "plot-a",
            Some("stale"),
            &["session-first", "session-second"],
        )]);

        assert_eq!(
            reconcile_restore(&field, None),
            RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-a",
                    "session-first",
                )),
                reason: RestoreFallbackReason::NoPreviousSelection,
            }
        );
    }

    #[test]
    fn fallback_is_plot_only_when_the_first_plot_has_no_session() {
        let field = field(&[
            ("plot-only", None, &[]),
            ("plot-later", None, &["session-later"]),
        ]);

        assert_eq!(
            reconcile_restore(&field, None),
            RestoreResolution::Fallback {
                selection: RestoreSelection::PlotOnly {
                    plot_id: "plot-only".to_owned(),
                },
                reason: RestoreFallbackReason::NoPreviousSelection,
            }
        );
    }

    #[test]
    fn empty_field_has_no_restore_target_and_an_explicit_reason() {
        let field = field(&[]);

        assert_eq!(
            reconcile_restore(&field, None),
            RestoreResolution::Unavailable {
                reason: RestoreFallbackReason::NoPlotsAvailable,
            }
        );
    }

    fn field(plots: &[(&str, Option<&str>, &[&str])]) -> FieldSnapshot {
        let parsed = serde_json::from_value(serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": plots
                .iter()
                .map(|(plot_id, selected_session_id, sessions)| serde_json::json!({
                    "kind": "nopal.plot/v1",
                    "plot_id": plot_id,
                    "selected_session_id": selected_session_id,
                    "sessions": sessions
                        .iter()
                        .map(|session_id| serde_json::json!({ "session_id": session_id }))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "entries": [],
        }));
        match parsed {
            Ok(field) => field,
            Err(error) => panic!("fixture should satisfy the Field contract: {error}"),
        }
    }
}
