//! eframe adapter for native host construction, focus, and reopen acknowledgement.

use std::sync::{Arc, Condvar, Mutex};

use nopal_feed_client::field::FieldSnapshot;
use nopal_feed_client::session::SessionModelReference;
use nopal_field_presentation::composer::{ComposerAuthority, ComposerTarget};
use nopal_field_presentation::coordinator::FieldPresentationCoordinator;
use nopal_field_presentation::view_state::{FieldViewState, WorkspaceSubject};
use nopal_native_lifecycle::activation::ActivationDeadline;
use nopal_native_lifecycle::application::{
    OwnedResourceRecoveryReport, ResolvedNativeApplicationHostFactory, RestorePreferenceNotice,
};
use nopal_native_lifecycle::current_field::CurrentCoreFieldAuthority;
use nopal_native_lifecycle::model_preferences::{ModelRecentsReadOutcome, ModelRecentsStore};
use nopal_native_lifecycle::reconcile::{
    ExactSessionSelection, RestoreResolution, RestoreSelection,
};
use nopal_native_lifecycle::supervisor::{
    NativeApplicationAck, NativeApplicationHost, NativeApplicationUnavailable,
};

use crate::model::{DesktopActivityKey, DesktopField};
use crate::session_runtime::{LiveSessionRuntime, ProductionRuntimeConnector, SessionRuntime};

/// Fully prepared renderer state transferred once from lifecycle composition to eframe.
pub struct EframeAppSeed {
    pub coordinator: FieldPresentationCoordinator,
    pub composer: ComposerAuthority,
    pub runtime: Option<LiveSessionRuntime>,
    pub startup_diagnostics: Vec<String>,
    pub recent_models: Vec<SessionModelReference>,
    pub model_recents_store: Option<ModelRecentsStore>,
}

#[derive(Default)]
struct BridgeState {
    context: Option<egui::Context>,
    requested: u64,
    acknowledged: u64,
    next_outcome: Option<NativeApplicationAck>,
    hidden: bool,
    shutdown: bool,
}

/// Thread-safe capability shared by the native activation host and eframe event loop.
#[derive(Clone, Default)]
pub struct EframeUiBridge {
    state: Arc<(Mutex<BridgeState>, Condvar)>,
}

impl EframeUiBridge {
    /// Registers the live eframe context and acknowledges focus after a focused frame.
    pub fn pump(&self, context: &egui::Context) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.context = Some(context.clone());
        if state.acknowledged < state.requested && context.input(|input| input.focused) {
            state.acknowledged = state.requested;
            state.hidden = false;
            changed.notify_all();
        }
    }

    /// Records that ordinary window close hid the resident application.
    pub fn mark_hidden(&self) {
        let (state, _) = &*self.state;
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .hidden = true;
    }

    /// Ends pending activation waits when the application is intentionally quitting.
    pub fn shutdown(&self) {
        let (state, changed) = &*self.state;
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown = true;
        changed.notify_all();
    }

    /// Returns whether the resident native application is shutting down.
    pub fn is_shutdown(&self) -> bool {
        let (state, _) = &*self.state;
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .shutdown
    }

    fn activate(
        &self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.context.is_none() && !state.shutdown {
            let remaining = deadline.remaining().map_err(|error| {
                NativeApplicationUnavailable::new(format!(
                    "eframe context was unavailable before activation: {error}"
                ))
            })?;
            let waited = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = waited.0;
        }
        if state.shutdown {
            return Err(NativeApplicationUnavailable::new(
                "eframe application is shutting down",
            ));
        }
        let context = state
            .context
            .clone()
            .ok_or_else(|| NativeApplicationUnavailable::new("eframe context is unavailable"))?;
        let outcome = if state.hidden {
            NativeApplicationAck::Reopened
        } else {
            NativeApplicationAck::Focused
        };
        state.requested = state.requested.saturating_add(1);
        let generation = state.requested;
        state.next_outcome = Some(outcome);
        context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        context.send_viewport_cmd(egui::ViewportCommand::Focus);
        context.request_repaint();

        while state.acknowledged < generation && !state.shutdown {
            let remaining = deadline.remaining().map_err(|error| {
                NativeApplicationUnavailable::new(format!(
                    "eframe did not complete activation before its deadline: {error}"
                ))
            })?;
            let waited = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = waited.0;
        }
        if state.shutdown {
            return Err(NativeApplicationUnavailable::new(
                "eframe application shut down during activation",
            ));
        }
        Ok(state.next_outcome.take().unwrap_or(outcome))
    }
}

/// Native host whose only renderer authority is focus and reopen through eframe.
pub struct EframeNativeHost {
    bridge: EframeUiBridge,
}

impl NativeApplicationHost for EframeNativeHost {
    fn activate(
        &mut self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        self.bridge.activate(deadline)
    }
}

/// Prepares one eframe app seed while returning the narrow native activation host.
#[derive(Clone, Default)]
pub struct EframeHostFactory {
    bridge: EframeUiBridge,
    seed: Arc<Mutex<Option<EframeAppSeed>>>,
    model_recents_store: Option<ModelRecentsStore>,
}

impl EframeHostFactory {
    pub fn with_model_recents(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            model_recents_store: Some(ModelRecentsStore::new(path)),
            ..Self::default()
        }
    }

    pub fn bridge(&self) -> EframeUiBridge {
        self.bridge.clone()
    }

    /// Transfers the prepared app state exactly once to the eframe event loop.
    pub fn take_seed(&self) -> Option<EframeAppSeed> {
        self.seed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

impl ResolvedNativeApplicationHostFactory for EframeHostFactory {
    type Host = EframeNativeHost;

    fn create_host(
        &self,
        field: &FieldSnapshot,
        restore: &RestoreResolution,
        _recovery_report: &OwnedResourceRecoveryReport,
        preference_notice: Option<&RestorePreferenceNotice>,
        current_field: CurrentCoreFieldAuthority,
    ) -> Result<Self::Host, NativeApplicationUnavailable> {
        let (subject, live_session) = initial_view(restore);
        let coordinator = FieldPresentationCoordinator::new(
            current_field,
            FieldViewState::new(subject, live_session.clone()),
        )
        .map_err(|error| {
            NativeApplicationUnavailable::new(format!(
                "cannot prepare native Field projection: {error:?}"
            ))
        })?;
        let composer_target = live_session.as_ref().and_then(|selection| {
            ComposerTarget::new(selection.plot_id(), selection.session_id()).ok()
        });
        let mut slot = self
            .seed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            return Err(NativeApplicationUnavailable::new(
                "eframe host factory already prepared an application",
            ));
        }
        let mut startup_diagnostics = Vec::new();
        if let Some(notice) = preference_notice {
            startup_diagnostics.push(format!("Restore preference was ignored: {notice:?}"));
        }
        if field.plots.is_empty() {
            startup_diagnostics.push("Core reported no available Plots".to_owned());
        }
        if let Some(reason) = fallback_restore_notice(restore) {
            startup_diagnostics.push(reason);
        }
        let recent_models = match self
            .model_recents_store
            .as_ref()
            .map(ModelRecentsStore::read)
        {
            None | Some(Ok(ModelRecentsReadOutcome::Missing)) => Vec::new(),
            Some(Ok(ModelRecentsReadOutcome::Ready(recents))) => recents,
            Some(Ok(outcome)) => {
                startup_diagnostics.push(format!(
                    "Model recency preference was ignored and preserved: {outcome:?}"
                ));
                Vec::new()
            }
            Some(Err(error)) => {
                startup_diagnostics.push(format!(
                    "Model recency preference could not be read: {error}"
                ));
                Vec::new()
            }
        };
        let runtime = live_session
            .as_ref()
            .map(|selection| session_runtime(field.clone(), selection))
            .transpose()
            .map_err(NativeApplicationUnavailable::new)?;
        *slot = Some(EframeAppSeed {
            coordinator,
            composer: ComposerAuthority::new(composer_target),
            runtime,
            startup_diagnostics,
            recent_models,
            model_recents_store: self.model_recents_store.clone(),
        });
        Ok(EframeNativeHost {
            bridge: self.bridge.clone(),
        })
    }
}

/// Builds the binding owner for one Core-validated exact Session.
pub fn session_runtime(
    snapshot: FieldSnapshot,
    selection: &ExactSessionSelection,
) -> Result<LiveSessionRuntime, String> {
    let mut field = DesktopField::from_snapshot(snapshot, Some(selection.plot_id()));
    if let Some(plot) = field
        .plots
        .iter_mut()
        .find(|plot| plot.plot_id == selection.plot_id())
    {
        plot.selected_session_id = Some(selection.session_id().to_owned());
    }
    let runtime = SessionRuntime::prepare(field, ProductionRuntimeConnector)?;
    if runtime.selected_activity()
        != Some(&DesktopActivityKey::Session(
            selection.session_id().to_owned(),
        ))
    {
        return Err("prepared Session runtime did not retain the exact Core selection".to_owned());
    }
    Ok(runtime)
}

fn initial_view(restore: &RestoreResolution) -> (WorkspaceSubject, Option<ExactSessionSelection>) {
    match restore {
        RestoreResolution::Exact(selection) => (
            WorkspaceSubject::Session(selection.clone()),
            Some(selection.clone()),
        ),
        RestoreResolution::Fallback {
            selection: RestoreSelection::Session(selection),
            ..
        } => (
            WorkspaceSubject::Session(selection.clone()),
            Some(selection.clone()),
        ),
        RestoreResolution::Fallback {
            selection: RestoreSelection::PlotOnly { plot_id },
            ..
        } => (
            WorkspaceSubject::Plot {
                plot_id: plot_id.clone(),
            },
            None,
        ),
        RestoreResolution::Unavailable { .. } => (
            WorkspaceSubject::Unavailable {
                reason: restore
                    .visible_reason()
                    .unwrap_or_else(|| "No safe Core selection is available".to_owned()),
            },
            None,
        ),
    }
}

fn fallback_restore_notice(restore: &RestoreResolution) -> Option<String> {
    matches!(restore, RestoreResolution::Fallback { .. })
        .then(|| restore.visible_reason())
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn hidden_resident_window_acknowledges_reopen_only_after_a_focused_frame() {
        let bridge = EframeUiBridge::default();
        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            focused: true,
            ..Default::default()
        });
        bridge.pump(&context);
        let _ = context.end_pass();
        bridge.mark_hidden();
        let mut host = EframeNativeHost {
            bridge: bridge.clone(),
        };
        let activation = thread::spawn(move || {
            host.activate(
                ActivationDeadline::after(Duration::from_secs(1))
                    .expect("valid activation deadline"),
            )
        });

        for _ in 0..100 {
            context.begin_pass(egui::RawInput {
                focused: true,
                ..Default::default()
            });
            bridge.pump(&context);
            let _ = context.end_pass();
            if activation.is_finished() {
                break;
            }
            thread::yield_now();
        }

        assert_eq!(
            activation
                .join()
                .expect("activation thread should not panic")
                .expect("focused frame should acknowledge activation"),
            NativeApplicationAck::Reopened
        );
    }

    #[test]
    fn stale_restore_fallback_keeps_its_visible_reason() {
        let restore = RestoreResolution::Fallback {
            selection: RestoreSelection::PlotOnly {
                plot_id: "plot-fallback".to_owned(),
            },
            reason: nopal_native_lifecycle::reconcile::RestoreFallbackReason::PlotMissing {
                plot_id: "plot-stale".to_owned(),
            },
        };

        assert!(fallback_restore_notice(&restore).is_some_and(
            |notice| notice.contains("plot-stale") && notice.contains("first available")
        ));
    }
}
