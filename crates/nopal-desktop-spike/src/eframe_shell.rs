//! Codex-inspired eframe presentation over renderer-neutral Nopal Field state.

use std::collections::VecDeque;
use std::time::Duration;

use eframe::egui;
use nopal_feed_client::session::SessionEventPayload;
use nopal_field_presentation::composer::{
    ComposerAuthority, ComposerIntent, ComposerTarget, SubmissionResolution,
};
use nopal_field_presentation::coordinator::SessionActivationOutcome;
use nopal_field_presentation::model_picker::ModelPickerAuthority;
use nopal_field_presentation::view_state::{LiveSessionState, WorkspaceSubject};
use nopal_native_lifecycle::model_preferences::{ModelRecentsStore, ModelRecentsWriteOutcome};
use nopal_native_lifecycle::reconcile::ExactSessionSelection;

use crate::eframe_host::{EframeAppSeed, EframeUiBridge, session_runtime};
use crate::session_runtime::{
    LiveSessionRuntime, RuntimePresentation, RuntimeStatus, SubmitOutcome,
};

const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(24, 25, 28);
const PANEL: egui::Color32 = egui::Color32::from_rgb(31, 32, 36);
const RAISED: egui::Color32 = egui::Color32::from_rgb(39, 41, 46);
const BORDER: egui::Color32 = egui::Color32::from_rgb(55, 57, 64);
const TEXT: egui::Color32 = egui::Color32::from_rgb(232, 233, 236);
const MUTED: egui::Color32 = egui::Color32::from_rgb(157, 160, 170);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(104, 143, 240);

/// First-class eframe application retaining the renderer-neutral authorities.
pub struct EframeFieldApp {
    bridge: EframeUiBridge,
    shell: FieldShell,
    quit_requested: bool,
}

impl EframeFieldApp {
    pub fn new(seed: EframeAppSeed, bridge: EframeUiBridge, context: &egui::Context) -> Self {
        configure_context(context);
        Self {
            bridge,
            shell: FieldShell::new(seed),
            quit_requested: false,
        }
    }
}

impl eframe::App for EframeFieldApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.bridge.pump(context);
        let close_requested = context.input(|input| input.viewport().close_requested());
        if close_requested && !self.quit_requested {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.bridge.mark_hidden();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.render(ui);
        if self.shell.take_quit_request() {
            self.quit_requested = true;
            self.bridge.shutdown();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Testable eframe shell with no windowing or lifecycle implementation details.
pub struct FieldShell {
    coordinator: nopal_field_presentation::coordinator::FieldPresentationCoordinator,
    composer: ComposerAuthority,
    runtime: Option<LiveSessionRuntime>,
    startup_diagnostics: Vec<String>,
    runtime_diagnostics: VecDeque<String>,
    outgoing: Vec<ComposerIntent>,
    terminal_input: String,
    model_picker: ModelPickerAuthority,
    model_recents_store: Option<ModelRecentsStore>,
    quit_requested: bool,
}

impl FieldShell {
    pub fn new(seed: EframeAppSeed) -> Self {
        let model_target = seed
            .coordinator
            .view()
            .live_session()
            .map(LiveSessionState::selection)
            .cloned();
        Self {
            coordinator: seed.coordinator,
            composer: seed.composer,
            runtime: seed.runtime,
            startup_diagnostics: seed.startup_diagnostics,
            runtime_diagnostics: VecDeque::new(),
            outgoing: Vec::new(),
            terminal_input: String::new(),
            model_picker: ModelPickerAuthority::new(model_target, seed.recent_models),
            model_recents_store: seed.model_recents_store,
            quit_requested: false,
        }
    }

    /// Drains exact structured Composer intents for the Session transport owner.
    pub fn take_outgoing(&mut self) -> Vec<ComposerIntent> {
        std::mem::take(&mut self.outgoing)
    }

    pub fn take_quit_request(&mut self) -> bool {
        std::mem::take(&mut self.quit_requested)
    }

    pub fn render(&mut self, ui: &mut egui::Ui) {
        self.drain_runtime();
        top_bar(ui, &mut self.quit_requested);
        self.plot_rail(ui);
        self.inspector(ui);
        self.composer(ui);
        self.main_stage(ui);
        self.dispatch_outgoing();
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }

    fn plot_rail(&mut self, ui: &mut egui::Ui) {
        let selected_plot =
            selected_plot(self.coordinator.view().workspace_subject()).map(str::to_owned);
        let plots = self
            .coordinator
            .accepted()
            .plots
            .iter()
            .map(|plot| {
                (
                    plot.plot_id.clone(),
                    if plot.title.is_empty() {
                        plot.plot_id.clone()
                    } else {
                        plot.title.clone()
                    },
                    plot.progress.clone(),
                )
            })
            .collect::<Vec<_>>();
        egui::Panel::left("plot-rail")
            .resizable(false)
            .default_size(240.0)
            .frame(panel_frame(PANEL))
            .show(ui, |ui| {
                ui.add_space(12.0);
                ui.strong(egui::RichText::new("PLOTS").color(MUTED).size(11.0));
                ui.add_space(8.0);
                for (plot_id, title, progress) in plots {
                    let selected = selected_plot.as_deref() == Some(plot_id.as_str());
                    let response = ui.add_sized(
                        [ui.available_width(), 48.0],
                        egui::Button::new(
                            egui::RichText::new(format!("{title}\n{progress}"))
                                .color(if selected { TEXT } else { MUTED }),
                        )
                        .fill(if selected { RAISED } else { PANEL })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if selected { ACCENT } else { BORDER },
                        ))
                        .corner_radius(8.0),
                    );
                    if response.clicked() {
                        self.coordinator
                            .view_mut()
                            .show_subject(WorkspaceSubject::Plot { plot_id });
                    }
                    ui.add_space(5.0);
                }
            });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        if ui.ctx().content_rect().width() < 1120.0 {
            return;
        }
        let subject = self.coordinator.view().workspace_subject().clone();
        let plot = selected_plot(&subject).and_then(|plot_id| {
            self.coordinator
                .accepted()
                .plots
                .iter()
                .find(|plot| plot.plot_id == plot_id)
                .cloned()
        });
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(280.0)
            .size_range(220.0..=380.0)
            .frame(panel_frame(PANEL))
            .show(ui, |ui| {
                ui.add_space(12.0);
                ui.strong(egui::RichText::new("INSPECTOR").color(MUTED).size(11.0));
                ui.add_space(12.0);
                if let Some(plot) = plot {
                    fact(ui, "Progress", &plot.progress);
                    fact(ui, "Fruit", &plot.fruit.state);
                    fact(ui, "Sessions", &plot.sessions.len().to_string());
                    fact(ui, "Executions", &plot.executions.len().to_string());
                    if !plot.conditions.is_empty() {
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("Conditions").color(MUTED));
                        for condition in plot.conditions {
                            ui.label(egui::RichText::new(format!("• {condition}")).color(TEXT));
                        }
                    }
                } else {
                    ui.label(
                        egui::RichText::new("Select a Plot to inspect its facts.").color(MUTED),
                    );
                }
            });
    }

    fn composer(&mut self, ui: &mut egui::Ui) {
        let live = self
            .coordinator
            .view()
            .live_session()
            .map(LiveSessionState::selection)
            .cloned();
        self.sync_composer_target(live.as_ref());
        egui::Panel::bottom("composer")
            .resizable(false)
            .min_size(132.0)
            .frame(panel_frame(BACKGROUND))
            .show(ui, |ui| {
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .corner_radius(12.0)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        let enabled = self.composer_submission_enabled();
                        let mut text = self
                            .composer
                            .active_draft()
                            .map(|draft| draft.text().to_owned())
                            .unwrap_or_default();
                        let response = ui.add_enabled(
                            enabled,
                            egui::TextEdit::multiline(&mut text)
                                .id(egui::Id::new("nopal-composer-text"))
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("Send an instruction to the selected Session…")
                                .text_color(TEXT),
                        );
                        if response.changed() {
                            self.composer.edit_active(|draft| {
                                draft.select_all();
                                draft.replace(None, &text);
                            });
                        }
                        ui.horizontal(|ui| {
                            let label = self
                                .composer
                                .active_target()
                                .map(|target| {
                                    format!("{} / {}", target.plot_id(), target.session_id())
                                })
                                .unwrap_or_else(|| "No live Session selected".to_owned());
                            ui.label(egui::RichText::new(label).color(MUTED).size(11.0));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let send = ui.add_enabled(
                                        enabled
                                            && !self.composer.is_pending()
                                            && !text.trim().is_empty(),
                                        egui::Button::new("Send").fill(ACCENT).corner_radius(7.0),
                                    );
                                    let shortcut = response.has_focus()
                                        && ui.input(|input| {
                                            input.key_pressed(egui::Key::Enter)
                                                && input.modifiers.command
                                        });
                                    if (send.clicked() || shortcut)
                                        && let Some(intent) = self.composer.prepare_submission()
                                    {
                                        self.outgoing.push(intent);
                                    }
                                },
                            );
                        });
                        if self.composer.is_pending() {
                            ui.label(
                                egui::RichText::new(
                                    "Waiting for structured Session acknowledgement",
                                )
                                .color(MUTED)
                                .size(11.0),
                            );
                        }
                        if let Some(diagnostic) = self.composer.diagnostic() {
                            ui.label(
                                egui::RichText::new(diagnostic)
                                    .color(egui::Color32::from_rgb(239, 128, 132))
                                    .size(11.0),
                            );
                        }
                    });
                ui.add_space(8.0);
            });
    }

    fn main_stage(&mut self, ui: &mut egui::Ui) {
        let subject = self.coordinator.view().workspace_subject().clone();
        let live = self.coordinator.view().live_session().cloned();
        let plots = self.coordinator.accepted().plots.clone();
        let diagnostics = self
            .startup_diagnostics
            .iter()
            .chain(&self.runtime_diagnostics)
            .cloned()
            .collect::<Vec<_>>();
        egui::CentralPanel::default()
            .frame(panel_frame(BACKGROUND))
            .show(ui, |ui| {
                ui.add_space(14.0);
                for diagnostic in diagnostics {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(55, 42, 31))
                        .corner_radius(8.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(diagnostic).color(TEXT));
                        });
                    ui.add_space(8.0);
                }
                match subject {
                    WorkspaceSubject::Unavailable { reason } => {
                        ui.heading(egui::RichText::new("No Field selection").color(TEXT));
                        ui.label(egui::RichText::new(reason).color(MUTED));
                    }
                    WorkspaceSubject::Plot { plot_id } => {
                        let Some(plot) = plots.iter().find(|plot| plot.plot_id == plot_id) else {
                            ui.label(
                                egui::RichText::new("Plot is no longer present in Core.")
                                    .color(MUTED),
                            );
                            return;
                        };
                        ui.heading(
                            egui::RichText::new(if plot.title.is_empty() {
                                &plot.plot_id
                            } else {
                                &plot.title
                            })
                            .color(TEXT),
                        );
                        ui.label(egui::RichText::new(&plot.intent).color(MUTED));
                        ui.add_space(18.0);
                        ui.strong(egui::RichText::new("Sessions").color(TEXT));
                        ui.add_space(6.0);
                        for session in &plot.sessions {
                            let exact =
                                ExactSessionSelection::new(&plot.plot_id, &session.session_id);
                            let selected =
                                live.as_ref().is_some_and(|live| live.selection() == &exact);
                            let response =
                                ui.add_sized(
                                    [ui.available_width(), 52.0],
                                    egui::Button::new(
                                        egui::RichText::new(format!(
                                            "{}    {}",
                                            session.session_id, session.state
                                        ))
                                        .color(if selected { TEXT } else { MUTED }),
                                    )
                                    .fill(if selected { RAISED } else { PANEL })
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        if selected { ACCENT } else { BORDER },
                                    ))
                                    .corner_radius(8.0),
                                );
                            if response.clicked() {
                                self.activate_session(exact);
                            }
                            ui.add_space(6.0);
                        }
                    }
                    WorkspaceSubject::Session(selection) => {
                        self.session_stage(ui, &selection);
                    }
                    WorkspaceSubject::Execution(execution) => {
                        ui.heading(egui::RichText::new(execution.run_id()).color(TEXT));
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / {}",
                                execution.service_id(),
                                execution.repo_id()
                            ))
                            .color(MUTED),
                        );
                    }
                }
            });
    }

    fn drain_runtime(&mut self) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let outcome = runtime.drain();
        if let Some(model) = runtime.take_confirmed_model_switch() {
            self.model_picker.record_confirmed(&model);
            if let Some(store) = &self.model_recents_store {
                match store.write(self.model_picker.recent()) {
                    Ok(ModelRecentsWriteOutcome::Written) => {}
                    Ok(ModelRecentsWriteOutcome::PreservedExisting(outcome)) => {
                        self.runtime_diagnostics.push_back(format!(
                            "Recent model ordering was not saved because the existing preference was preserved: {outcome:?}"
                        ));
                    }
                    Err(error) => self
                        .runtime_diagnostics
                        .push_back(format!("Recent model ordering could not be saved: {error}")),
                }
            }
        }
        for error in outcome.errors {
            if self.runtime_diagnostics.back() != Some(&error) {
                self.runtime_diagnostics.push_back(error);
            }
        }
        while self.runtime_diagnostics.len() > 8 {
            self.runtime_diagnostics.pop_front();
        }
    }

    fn activate_session(&mut self, target: ExactSessionSelection) {
        self.activate_session_with(target, session_runtime);
    }

    fn activate_session_with<F>(&mut self, target: ExactSessionSelection, mut prepare: F)
    where
        F: FnMut(
            nopal_feed_client::field::FieldSnapshot,
            &ExactSessionSelection,
        ) -> Result<LiveSessionRuntime, String>,
    {
        let snapshot = self.coordinator.accepted().clone();
        let mut replacement = None;
        let outcome = self.coordinator.activate_session(target, |selection| {
            replacement = Some(prepare(snapshot, selection)?);
            Ok::<(), String>(())
        });
        match outcome {
            Ok(persistence) => {
                if let Some(runtime) = replacement {
                    self.runtime = Some(runtime);
                }
                match persistence {
                    SessionActivationOutcome::Persisted => {}
                    SessionActivationOutcome::PersistenceNotWritten(reason) => {
                        self.runtime_diagnostics.push_back(format!(
                            "Session is active, but selection was not persisted: {reason:?}"
                        ));
                    }
                    SessionActivationOutcome::PersistenceFailed(error) => {
                        self.runtime_diagnostics.push_back(format!(
                            "Session is active, but selection persistence failed: {error}"
                        ));
                    }
                }
            }
            Err(error) => self
                .runtime_diagnostics
                .push_back(format!("Session activation failed: {error}")),
        }
    }

    fn dispatch_outgoing(&mut self) {
        for intent in std::mem::take(&mut self.outgoing) {
            let ComposerIntent::Submit {
                target,
                revision,
                submission,
            } = intent;
            let resolution = match self.runtime.as_mut() {
                Some(runtime)
                    if runtime.selected_session_context().is_some_and(|context| {
                        context.plot_id == target.plot_id()
                            && context.session_id == target.session_id()
                    }) =>
                {
                    match runtime.submit_prompt(submission.text()) {
                        SubmitOutcome::Sent { command_id } => SubmissionResolution::Sent {
                            target,
                            revision,
                            command_id,
                        },
                        SubmitOutcome::RestoreText { reason, .. } => {
                            SubmissionResolution::Rejected {
                                target,
                                revision,
                                reason,
                            }
                        }
                    }
                }
                _ => SubmissionResolution::Rejected {
                    target,
                    revision,
                    reason: "structured Session binding does not match the Composer target"
                        .to_owned(),
                },
            };
            self.composer.resolve(resolution);
        }
    }

    fn session_stage(&mut self, ui: &mut egui::Ui, selection: &ExactSessionSelection) {
        self.model_picker.retarget(Some(selection.clone()));
        let presentation = self
            .runtime
            .as_ref()
            .map(LiveSessionRuntime::presentation)
            .unwrap_or_default();
        let status = self
            .runtime
            .as_ref()
            .map(LiveSessionRuntime::status)
            .cloned()
            .unwrap_or_else(|| RuntimeStatus::Unavailable {
                reason: "Session bindings are unavailable".to_owned(),
            });
        let model_state = self
            .runtime
            .as_ref()
            .and_then(LiveSessionRuntime::model_state)
            .cloned();
        let can_switch_model = self
            .runtime
            .as_ref()
            .is_some_and(LiveSessionRuntime::can_switch_model);
        let model_pending = self
            .runtime
            .as_ref()
            .is_some_and(LiveSessionRuntime::model_switch_pending);
        let mut selected_model = None;
        let mut refresh_models = false;
        ui.horizontal(|ui| {
            ui.heading(
                egui::RichText::new(format!("Session {}", selection.session_id())).color(TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let terminal = ui
                    .selectable_label(presentation == RuntimePresentation::Terminal, "Terminal")
                    .on_hover_text("Open the same Session's explicit Terminal escape hatch");
                let output = ui.selectable_label(
                    presentation == RuntimePresentation::Output,
                    "Structured output",
                );
                let model_label = if model_pending {
                    "Switching model...".to_owned()
                } else {
                    model_state
                        .as_ref()
                        .and_then(|state| state.current.as_ref())
                        .map(|model| model.name.clone())
                        .unwrap_or_else(|| "Model unavailable".to_owned())
                };
                let picker = ui.add_enabled_ui(model_state.is_some(), |ui| {
                    egui::ComboBox::from_id_salt("session-model-picker")
                        .selected_text(model_label)
                        .show_ui(ui, |ui| {
                            let mut query = self.model_picker.query().to_owned();
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut query)
                                        .hint_text("Filter models")
                                        .desired_width(280.0),
                                )
                                .changed()
                            {
                                self.model_picker.set_query(query);
                            }
                            ui.separator();
                            if let Some(state) = &model_state {
                                for model in self.model_picker.visible(&state.available) {
                                    let active = state.current.as_ref().is_some_and(|current| {
                                        current.provider == model.provider && current.id == model.id
                                    });
                                    let label = format!(
                                        "{}  {} / {}",
                                        model.name, model.provider, model.id
                                    );
                                    if ui
                                        .add_enabled(
                                            can_switch_model && !active,
                                            egui::Button::new(label).selected(active),
                                        )
                                        .clicked()
                                    {
                                        selected_model = Some(model);
                                        ui.close();
                                    }
                                }
                            }
                        })
                });
                refresh_models = picker.inner.response.clicked();
                if terminal.clicked()
                    && let Some(runtime) = self.runtime.as_mut()
                {
                    runtime.set_presentation(RuntimePresentation::Terminal);
                }
                if output.clicked()
                    && let Some(runtime) = self.runtime.as_mut()
                {
                    runtime.set_presentation(RuntimePresentation::Output);
                }
            });
        });
        if refresh_models
            && let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.refresh_models()
        {
            self.runtime_diagnostics.push_back(error);
        }
        if let Some(model) = selected_model
            && let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.switch_model(&model)
        {
            self.runtime_diagnostics.push_back(error);
        }
        ui.label(
            egui::RichText::new(runtime_status_label(&status))
                .color(runtime_status_color(&status))
                .size(12.0),
        );
        if let Some(error) = self
            .runtime
            .as_ref()
            .and_then(LiveSessionRuntime::model_error)
        {
            ui.label(
                egui::RichText::new(format!("Model switch: {error}"))
                    .color(egui::Color32::from_rgb(235, 141, 141))
                    .size(12.0),
            );
        }
        if let Some(state) = &model_state
            && !state.available_complete
        {
            ui.label(
                egui::RichText::new(format!(
                    "Showing {} of {} Pi models because the catalog reached the Session transport bound",
                    state.available.len(), state.available_total
                ))
                .color(egui::Color32::from_rgb(235, 190, 120))
                .size(12.0),
            );
        }
        if matches!(
            status,
            RuntimeStatus::TerminalOnly { .. }
                | RuntimeStatus::Unavailable { .. }
                | RuntimeStatus::Degraded { .. }
        ) && ui.small_button("Retry structured output").clicked()
            && let Some(runtime) = self.runtime.as_mut()
            && !runtime.retry_now()
        {
            self.runtime_diagnostics
                .push_back("Structured output is not currently retryable".to_owned());
        }
        ui.add_space(14.0);
        match presentation {
            RuntimePresentation::Output => self.structured_output(ui),
            RuntimePresentation::Terminal => self.terminal_output(ui),
        }
    }

    fn structured_output(&mut self, ui: &mut egui::Ui) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Some(binding) = runtime.terminal_binding_mut()
            && let Some(controller) = binding.controller_mut()
        {
            controller.set_focused(false);
        }
        let events = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.current_events().to_vec())
            .unwrap_or_default();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if events.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Waiting for the verified structured Session timeline.",
                        )
                        .color(MUTED),
                    );
                }
                for event in events {
                    let (role, text, fill) = match event.event {
                        SessionEventPayload::UserMessage { text, .. } => {
                            ("You", text, egui::Color32::from_rgb(36, 41, 52))
                        }
                        SessionEventPayload::AssistantMessage { text, .. } => {
                            ("Assistant", text, PANEL)
                        }
                        SessionEventPayload::SessionReady { .. } => {
                            ("Session", "Structured Session is ready".to_owned(), PANEL)
                        }
                        SessionEventPayload::SessionError { message, .. } => (
                            "Session error",
                            message,
                            egui::Color32::from_rgb(55, 34, 36),
                        ),
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0, BORDER))
                        .corner_radius(10.0)
                        .inner_margin(14.0)
                        .show(ui, |ui| {
                            ui.strong(egui::RichText::new(role).color(MUTED).size(11.0));
                            ui.add_space(5.0);
                            ui.label(egui::RichText::new(text).color(TEXT));
                        });
                    ui.add_space(8.0);
                }
            });
    }

    fn terminal_output(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.runtime.as_mut().and_then(|runtime| {
            runtime.terminal_binding_mut().and_then(|binding| {
                binding.controller_mut().map(|controller| {
                    controller.set_focused(true);
                    controller.snapshot()
                })
            })
        });
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(15, 16, 18))
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height((ui.available_height() - 62.0).max(160.0))
                    .show(ui, |ui| {
                        if let Some(snapshot) = snapshot {
                            for row in snapshot.rows {
                                let mut line = String::new();
                                for run in row.runs {
                                    if line.len() < run.start_column {
                                        line.push_str(&" ".repeat(run.start_column - line.len()));
                                    }
                                    line.push_str(&run.text);
                                }
                                ui.label(
                                    egui::RichText::new(if line.is_empty() { " " } else { &line })
                                        .font(egui::FontId::monospace(12.0))
                                        .color(TEXT),
                                );
                            }
                        } else {
                            ui.label(
                                egui::RichText::new(
                                    "This Session has no available Terminal binding.",
                                )
                                .color(MUTED),
                            );
                        }
                    });
                ui.add_space(8.0);
                let row = terminal_input_row(ui, &mut self.terminal_input);
                let submit = row.submitted;
                if submit && !self.terminal_input.is_empty() {
                    let sent = self.runtime.as_mut().is_some_and(|runtime| {
                        runtime.terminal_binding_mut().is_some_and(|binding| {
                            binding.controller_mut().is_some_and(|controller| {
                                controller.submit_instruction(&self.terminal_input)
                            })
                        })
                    });
                    if sent {
                        self.terminal_input.clear();
                    }
                }
            });
    }

    fn sync_composer_target(&mut self, selection: Option<&ExactSessionSelection>) {
        let target = selection.and_then(|selection| {
            ComposerTarget::new(selection.plot_id(), selection.session_id()).ok()
        });
        if self.composer.active_target() != target.as_ref() {
            self.composer.retarget(target);
        }
    }

    fn composer_submission_enabled(&self) -> bool {
        self.composer.active_target().is_some()
            && self
                .runtime
                .as_ref()
                .is_some_and(LiveSessionRuntime::can_submit)
    }
}

struct TerminalInputRow {
    submitted: bool,
    #[cfg(test)]
    input_bounds: egui::Rect,
    #[cfg(test)]
    button_bounds: egui::Rect,
    #[cfg(test)]
    clip_bounds: egui::Rect,
}

fn terminal_input_row(ui: &mut egui::Ui, terminal_input: &mut String) -> TerminalInputRow {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Reserve the fixed-width action first so the expanding editor can only consume
            // the space that genuinely remains inside the row.
            let button = ui.button("Send Enter");
            let response = ui.add(
                egui::TextEdit::singleline(terminal_input)
                    .desired_width(f32::INFINITY)
                    .hint_text("Terminal input for this same Session"),
            );
            TerminalInputRow {
                submitted: button.clicked()
                    || (response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))),
                #[cfg(test)]
                input_bounds: response.rect,
                #[cfg(test)]
                button_bounds: button.rect,
                #[cfg(test)]
                clip_bounds: ui.clip_rect(),
            }
        })
        .inner
    })
    .inner
}

fn top_bar(ui: &mut egui::Ui, quit_requested: &mut bool) {
    egui::Panel::top("field-title-bar")
        .resizable(false)
        .exact_size(48.0)
        .frame(panel_frame(PANEL))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.strong(egui::RichText::new("Nopal").color(TEXT).size(15.0));
                ui.label(egui::RichText::new("Field").color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Quit").color(MUTED))
                                .fill(PANEL)
                                .corner_radius(7.0),
                        )
                        .on_hover_text("Quit Nopal and release resident Sessions")
                        .clicked()
                    {
                        *quit_requested = true;
                    }
                });
            });
        });
}

fn selected_plot(subject: &WorkspaceSubject) -> Option<&str> {
    match subject {
        WorkspaceSubject::Plot { plot_id } => Some(plot_id),
        WorkspaceSubject::Session(selection) => Some(selection.plot_id()),
        WorkspaceSubject::Execution(execution) => Some(execution.plot_id()),
        WorkspaceSubject::Unavailable { .. } => None,
    }
}

fn runtime_status_label(status: &RuntimeStatus) -> String {
    match status {
        RuntimeStatus::Ready => {
            "Structured Session is live; Terminal attaches only when needed".to_owned()
        }
        RuntimeStatus::StructuredOnly { terminal_error } => {
            format!("Structured Session is live; Terminal unavailable: {terminal_error}")
        }
        RuntimeStatus::TerminalOnly { structured_error } => {
            format!("Terminal fallback only; structured Session unavailable: {structured_error}")
        }
        RuntimeStatus::ExecutionSelected => "An Execution is selected".to_owned(),
        RuntimeStatus::Unavailable { reason } => format!("Session unavailable: {reason}"),
        RuntimeStatus::Degraded { detail } => detail.clone(),
    }
}

fn runtime_status_color(status: &RuntimeStatus) -> egui::Color32 {
    match status {
        RuntimeStatus::Ready => egui::Color32::from_rgb(111, 207, 151),
        RuntimeStatus::StructuredOnly { .. }
        | RuntimeStatus::TerminalOnly { .. }
        | RuntimeStatus::Degraded { .. } => egui::Color32::from_rgb(232, 182, 105),
        RuntimeStatus::ExecutionSelected | RuntimeStatus::Unavailable { .. } => MUTED,
    }
}

fn configure_context(context: &egui::Context) {
    context.enable_accesskit();
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = PANEL;
    visuals.faint_bg_color = RAISED;
    visuals.widgets.noninteractive.fg_stroke.color = TEXT;
    visuals.widgets.inactive.fg_stroke.color = MUTED;
    visuals.widgets.hovered.fg_stroke.color = TEXT;
    visuals.selection.bg_fill = ACCENT;
    context.set_visuals(visuals);
}

fn panel_frame(fill: egui::Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::same(10))
}

fn fact(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).color(TEXT));
        });
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use nopal_feed_client::field::parse_field;
    use nopal_field_presentation::coordinator::FieldPresentationCoordinator;
    use nopal_field_presentation::view_state::{FieldViewState, WorkspaceSubject};
    use nopal_native_lifecycle::application::NativeSelectionPersistence;
    use nopal_native_lifecycle::current_field::CurrentCoreFieldAuthority;
    use nopal_native_lifecycle::reconcile::ExactSessionSelection;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn terminal_input_row_keeps_every_control_inside_its_clip_bounds() {
        let context = egui::Context::default();
        configure_context(&context);
        let mut terminal_input = String::new();
        let mut bounds = None;

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(520.0, 120.0),
                )),
                focused: true,
                ..Default::default()
            },
            |ui| bounds = Some(terminal_input_row(ui, &mut terminal_input)),
        );

        let bounds = bounds.expect("terminal input row should render");
        assert!(
            bounds.clip_bounds.contains_rect(bounds.input_bounds),
            "terminal input extends beyond the row clip: {:?} outside {:?}",
            bounds.input_bounds,
            bounds.clip_bounds
        );
        assert!(
            bounds.clip_bounds.contains_rect(bounds.button_bounds),
            "terminal submit button extends beyond the row clip: {:?} outside {:?}",
            bounds.button_bounds,
            bounds.clip_bounds
        );
    }

    #[test]
    fn headless_field_renders_structured_ui_and_accessibility_tree() {
        let snapshot = parse_field(&serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": [{
                "kind": "nopal.plot/v1",
                "plot_id": "plot-a",
                "title": "Ship native Field",
                "intent": "Dogfood Nopal without losing the Terminal escape hatch",
                "progress": "active",
                "conditions": ["Keep Core authoritative"],
                "sessions": [{
                    "session_id": "session-a",
                    "state": "active",
                    "protocol": {
                        "kind": "nopal.session/v1",
                        "transport": "unix",
                        "address": "/tmp/session-a.sock",
                        "state": "ready"
                    }
                }, {
                    "session_id": "session-b",
                    "state": "active",
                    "protocol": {
                        "kind": "nopal.session/v1",
                        "transport": "unix",
                        "address": "/tmp/session-b.sock",
                        "state": "ready"
                    }
                }],
                "executions": [{
                    "service_id": "rondo",
                    "repo_id": "nopal",
                    "run_id": "run-a",
                    "status": "running"
                }]
            }],
            "entries": []
        }))
        .expect("fixture should satisfy the Field contract");
        let directory = tempdir().expect("create restore sandbox");
        let restore_path = directory.path().join("restore.json");
        let selection = ExactSessionSelection::new("plot-a", "session-a");
        let runtime = None;
        let persistence =
            NativeSelectionPersistence::for_restore_path(restore_path.clone(), &snapshot);
        let authority = CurrentCoreFieldAuthority::from_startup(snapshot, persistence);
        let coordinator = FieldPresentationCoordinator::new(
            authority,
            FieldViewState::new(
                WorkspaceSubject::Session(selection.clone()),
                Some(selection),
            ),
        )
        .expect("fixture should project");
        let mut shell = FieldShell::new(EframeAppSeed {
            coordinator,
            composer: ComposerAuthority::new(ComposerTarget::new("plot-a", "session-a").ok()),
            runtime,
            startup_diagnostics: Vec::new(),
            recent_models: Vec::new(),
            model_recents_store: None,
        });
        assert!(
            !shell.composer_submission_enabled(),
            "Composer must not advertise submission without a live structured Session"
        );
        shell.activate_session_with(ExactSessionSelection::new("plot-a", "session-b"), |_, _| {
            Err("replacement binding unavailable".to_owned())
        });
        assert_eq!(
            shell
                .coordinator
                .view()
                .live_session()
                .map(LiveSessionState::selection),
            Some(&ExactSessionSelection::new("plot-a", "session-a"))
        );
        assert!(shell.runtime.is_none());
        assert!(
            shell
                .runtime_diagnostics
                .back()
                .is_some_and(|diagnostic| diagnostic.contains("replacement binding unavailable"))
        );
        std::fs::remove_dir_all(directory.path()).expect("remove restore parent");
        std::fs::write(directory.path(), b"blocked").expect("replace restore parent with a file");
        shell.activate_session_with(
            ExactSessionSelection::new("plot-a", "session-b"),
            |snapshot, selection| {
                let mut field =
                    crate::model::DesktopField::from_snapshot(snapshot, Some(selection.plot_id()));
                if let Some(plot) = field
                    .plots
                    .iter_mut()
                    .find(|plot| plot.plot_id == selection.plot_id())
                {
                    plot.selected_session_id = Some(selection.session_id().to_owned());
                }
                Ok(crate::session_runtime::SessionRuntime::new(
                    field,
                    crate::session_runtime::ProductionRuntimeConnector,
                ))
            },
        );
        assert_eq!(
            shell
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.selected_session_context())
                .map(|context| (context.plot_id, context.session_id)),
            Some(("plot-a".to_owned(), "session-b".to_owned()))
        );
        assert!(shell.runtime_diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("Session is active")
                && diagnostic.contains("selection was not persisted")
        }));
        let context = egui::Context::default();
        configure_context(&context);

        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                focused: true,
                ..Default::default()
            },
            |ui| shell.render(ui),
        );

        assert!(
            !output.shapes.is_empty(),
            "Field must produce drawable output"
        );
        assert!(
            output.platform_output.accesskit_update.is_some(),
            "Field must emit an AccessKit tree"
        );
    }
}
