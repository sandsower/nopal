use std::ops::Range;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, FontWeight, IntoElement, MouseButton, Pixels, Point, Render,
    SharedString, TextRun, UTF16Selection, Window, actions, canvas, div, fill, font, point,
    prelude::*, px, rgb, size,
};
use nopal_feed_client::session::{SessionEvent, SessionEventPayload};

use crate::composer::Composer;
use crate::interaction::{ConnectionState, TerminalController};
use crate::model::{DesktopActivity, DesktopActivityKey, DesktopPlot};
use crate::session_feed::FeedState;
use crate::session_runtime::{
    LiveTerminalBinding, RuntimeConnector, RuntimePresentation, RuntimeStatus, SessionRuntime,
    SubmitOutcome,
};
use crate::terminal::{TerminalSnapshot, TerminalStyle};
use crate::theme;
use crate::timeline::{ReplayState, TimelineFailure};
use crate::tmux::OwnedDemoSession;

actions!(nopal_terminal, [PasteTerminal, CopyTerminal, PasteComposer]);

const TERMINAL_HISTORY_LABEL: &str = "Live Terminal - not part of Session history";

#[derive(Clone)]
struct ComposerLineGeometry {
    text_range: Range<usize>,
    bounds: Bounds<Pixels>,
    line: gpui::ShapedLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Narrow,
    Laptop,
    Wide,
}

impl LayoutMode {
    fn for_width(width: f32) -> Self {
        if width < 860.0 {
            Self::Narrow
        } else if width < 1180.0 {
            Self::Laptop
        } else {
            Self::Wide
        }
    }

    fn rail_width(self) -> f32 {
        match self {
            Self::Narrow => 0.0,
            Self::Laptop => theme::COMPACT_RAIL_WIDTH,
            Self::Wide => theme::RAIL_WIDTH,
        }
    }

    fn shows_inspector(self) -> bool {
        matches!(self, Self::Wide)
    }
}

pub struct DesktopShell<C>
where
    C: RuntimeConnector<Terminal = LiveTerminalBinding>,
{
    runtime: SessionRuntime<C>,
    diagnostic: Option<String>,
    inspector_open: bool,
    narrow_rail_open: bool,
    terminal_focus: FocusHandle,
    composer_focus: FocusHandle,
    _owned_demo: Option<OwnedDemoSession>,
    marked_text: String,
    composer: Composer,
    composer_bounds: Option<Bounds<Pixels>>,
    composer_lines: Vec<ComposerLineGeometry>,
    composer_selecting: bool,
    terminal_bounds: Option<Bounds<Pixels>>,
    cell_width: Pixels,
    line_height: Pixels,
    selecting: bool,
}

impl<C> DesktopShell<C>
where
    C: RuntimeConnector<Terminal = LiveTerminalBinding> + 'static,
{
    pub fn new(
        runtime: SessionRuntime<C>,
        diagnostic: Option<String>,
        terminal_focus: FocusHandle,
        composer_focus: FocusHandle,
        owned_demo: Option<OwnedDemoSession>,
    ) -> Self {
        let mut composer = Composer::default();
        let draft = runtime.composer_draft();
        if !draft.is_empty() {
            composer.replace(None, draft);
        }
        Self {
            runtime,
            diagnostic,
            inspector_open: true,
            narrow_rail_open: false,
            terminal_focus,
            composer_focus,
            _owned_demo: owned_demo,
            marked_text: String::new(),
            composer,
            composer_bounds: None,
            composer_lines: Vec::new(),
            composer_selecting: false,
            terminal_bounds: None,
            cell_width: px(8.0),
            line_height: px(18.0),
            selecting: false,
        }
    }

    fn save_composer_draft(&mut self) {
        self.runtime
            .set_composer_draft(self.composer.text().to_owned());
    }

    fn load_composer_draft(&mut self) {
        let draft = self.runtime.composer_draft().to_owned();
        self.composer = Composer::default();
        if !draft.is_empty() {
            self.composer.replace(None, &draft);
        }
        self.marked_text.clear();
    }

    pub fn drain_runtime(&mut self, cx: &mut Context<Self>) {
        let outcome = self.runtime.drain();
        if let Some(error) = outcome.errors.last() {
            self.diagnostic = Some(error.clone());
        }
        if outcome.events_applied > 0
            || outcome.terminal_chunks_applied > 0
            || outcome.visible_changed
            || !outcome.errors.is_empty()
        {
            cx.notify();
        }
    }

    fn terminal_controller(
        &self,
    ) -> Option<&TerminalController<Box<dyn crate::tmux::PaneTransport>>> {
        self.runtime
            .terminal_binding()
            .and_then(LiveTerminalBinding::controller)
    }

    fn terminal_controller_mut(
        &mut self,
    ) -> Option<&mut TerminalController<Box<dyn crate::tmux::PaneTransport>>> {
        self.runtime
            .terminal_binding_mut()
            .and_then(LiveTerminalBinding::controller_mut)
    }

    fn plot_row(plot: DesktopPlot, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let plot_id = plot.plot_id.clone();
        let condition = plot.conditions.first().cloned();
        div()
            .id(SharedString::from(plot.plot_id.clone()))
            .debug_selector(|| format!("plot-row-{plot_id}"))
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded(px(theme::ROW_RADIUS))
            .cursor_pointer()
            .text_size(px(theme::TEXT_SMALL))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .when(selected, |row| row.bg(rgb(theme::SELECTED)))
            .when(!selected, |row| row.hover(|row| row.bg(rgb(theme::HOVER))))
            .child(plot.title.clone())
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(theme::TEXT_TINY))
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(plot.progress.clone())
                    .when_some(condition, |row, condition| row.child(condition)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.save_composer_draft();
                match this.runtime.select_plot(&plot_id) {
                    Ok(outcome) => {
                        if outcome.changed {
                            this.load_composer_draft();
                        }
                        this.diagnostic = None;
                    }
                    Err(error) => this.diagnostic = Some(format!("cannot select Plot: {error:?}")),
                }
                this.narrow_rail_open = false;
                cx.notify();
            }))
            .into_any_element()
    }

    fn rail(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_plot_id = self.runtime.field().selected_plot_id.as_deref();
        let rows = self
            .runtime
            .field()
            .plots
            .clone()
            .into_iter()
            .map(|plot| {
                let selected = selected_plot_id == Some(plot.plot_id.as_str());
                Self::plot_row(plot, selected, cx)
            })
            .collect::<Vec<_>>();
        div()
            .id("plot-rail")
            .cursor_text()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .px_2()
            .pt_4()
            .pb_3()
            .gap_1()
            .bg(rgb(theme::RAIL_BACKGROUND))
            .border_r_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .h(px(36.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_size(px(theme::TEXT_BASE))
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .child("Nopal"),
            )
            .child(
                div()
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .text_size(px(theme::TEXT_TINY))
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child("PLOTS"),
            )
            .children(rows)
    }

    fn activity_tab(
        activity: DesktopActivity,
        selected_activity: Option<&DesktopActivityKey>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = activity.key();
        let selected = selected_activity == Some(&key);
        let label = match &activity {
            DesktopActivity::Session { session_id, .. } => {
                format!("Session {}", short_id(session_id, "session-"))
            }
            DesktopActivity::Execution { run_id, .. } => {
                format!("Run {}", short_id(run_id, "run-"))
            }
        };
        div()
            .id(SharedString::from(format!(
                "activity-{}",
                activity_id(&key)
            )))
            .debug_selector(|| format!("activity-{}", activity_id(&key)))
            .cursor_pointer()
            .flex_none()
            .px_3()
            .py_1()
            .rounded(px(theme::ROW_RADIUS))
            .text_size(px(theme::TEXT_SMALL))
            .text_color(rgb(if selected {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            }))
            .when(selected, |tab| tab.bg(rgb(theme::SELECTED)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.save_composer_draft();
                match this.runtime.select_activity(key.clone()) {
                    Ok(outcome) => {
                        if outcome.changed {
                            this.load_composer_draft();
                        }
                        this.diagnostic = None;
                    }
                    Err(error) => {
                        this.diagnostic = Some(format!("cannot select activity: {error:?}"));
                    }
                }
                cx.notify();
            }))
            .child(label)
            .into_any_element()
    }

    fn stage(
        &self,
        layout: LayoutMode,
        plot: Option<&DesktopPlot>,
        terminal_focused: bool,
        composer_focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = plot
            .map(|plot| plot.title.clone())
            .unwrap_or_else(|| "No Plot selected".to_owned());
        let subtitle = match (plot, self.runtime.selected_activity()) {
            (Some(plot), Some(DesktopActivityKey::Session(session_id))) => format!(
                "Plot {} · Session {}",
                short_id(&plot.plot_id, "plot-"),
                short_id(session_id, "session-")
            ),
            (Some(plot), Some(DesktopActivityKey::Execution { run_id, .. })) => format!(
                "Plot {} · Run {}",
                short_id(&plot.plot_id, "plot-"),
                short_id(run_id, "run-")
            ),
            (Some(plot), None) => format!("Plot {}", short_id(&plot.plot_id, "plot-")),
            (None, _) => "Start Nopal to load the Field".to_owned(),
        };
        let activities = plot.map(|plot| plot.activities.clone()).unwrap_or_default();
        let selected_activity = self.runtime.selected_activity().cloned();
        let status = runtime_status_label(self.runtime.status());

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_w_0()
            .h_full()
            .bg(rgb(theme::SURFACE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(theme::HEADER_HEIGHT))
                    .px_5()
                    .border_b_1()
                    .border_color(rgb(theme::BORDER))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .text_color(rgb(theme::TEXT_PRIMARY))
                            .text_size(px(theme::TEXT_BASE))
                            .child(title)
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_TINY))
                                    .text_color(rgb(theme::TEXT_TERTIARY))
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(matches!(layout, LayoutMode::Narrow), |header| {
                                header.child(
                                    div()
                                        .id("narrow-plots-button")
                                        .debug_selector(|| "narrow-plots-button".to_owned())
                                        .cursor_pointer()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(theme::ROW_RADIUS))
                                        .bg(rgb(theme::RAIL_BACKGROUND))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.narrow_rail_open = !this.narrow_rail_open;
                                            cx.notify();
                                        }))
                                        .child("Plots"),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_TINY))
                                    .text_color(rgb(theme::TEXT_TERTIARY))
                                    .child(status),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .children(activities.iter().map(|activity| {
                        Self::activity_tab(activity.clone(), selected_activity.as_ref(), cx)
                    })),
            )
            .child(self.presentation_switch(cx))
            .child(match self.runtime.presentation() {
                RuntimePresentation::Output => self.output_stage(cx).into_any_element(),
                RuntimePresentation::Terminal => {
                    self.terminal_stage(terminal_focused, cx).into_any_element()
                }
            })
            .child(self.composer_stage(composer_focused, cx))
    }

    fn presentation_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_none().px_4().pb_2().child(
            div()
                .flex()
                .p(px(2.0))
                .rounded(px(theme::ROW_RADIUS))
                .bg(rgb(theme::RAIL_BACKGROUND))
                .child(self.presentation_button("Output", RuntimePresentation::Output, cx))
                .child(self.presentation_button("Terminal", RuntimePresentation::Terminal, cx)),
        )
    }

    fn presentation_button(
        &self,
        label: &'static str,
        mode: RuntimePresentation,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.runtime.presentation() == mode;
        div()
            .id(label)
            .debug_selector(move || format!("presentation-{}", label.to_lowercase()))
            .cursor_pointer()
            .px_3()
            .py_1()
            .rounded(px(theme::ROW_RADIUS - 2.0))
            .text_size(px(theme::TEXT_TINY))
            .text_color(rgb(if selected {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            }))
            .when(selected, |button| button.bg(rgb(theme::SURFACE)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.runtime.set_presentation(mode);
                if mode == RuntimePresentation::Terminal {
                    this.terminal_focus.focus(window);
                } else {
                    this.composer_focus.focus(window);
                }
                cx.notify();
            }))
            .child(label)
    }

    fn output_stage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let events = self.runtime.current_events().to_vec();
        let empty = output_empty_state(self.runtime.status());
        let feed_notice = self.feed_notice(cx);
        div()
            .id("output-stage")
            .flex()
            .flex_col()
            .flex_grow()
            .min_h_0()
            .mx_4()
            .mb_3()
            .p_4()
            .gap_4()
            .rounded(px(theme::RADIUS))
            .bg(rgb(theme::OUTPUT_SURFACE))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .overflow_y_scroll()
            .when_some(feed_notice, |stage, notice| stage.child(notice))
            .when(events.is_empty(), |stage| {
                stage.child(
                    div()
                        .id("timeline-empty")
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .child(empty),
                )
            })
            .children(events.into_iter().map(timeline_event_row))
    }

    fn feed_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let ReplayState::Failed(failure) = self.runtime.replay_state() {
            return Some(self.feed_notice_card(
                "verified-prefix-error",
                "Verified history paused",
                verified_failure_detail(&failure),
                false,
                cx,
            ));
        }
        match self.runtime.feed_state()? {
            FeedState::Idle | FeedState::Connecting { .. } => Some(self.feed_notice_card(
                "feed-state-restoring",
                "Connecting to Session",
                "Preparing the durable history feed.",
                false,
                cx,
            )),
            FeedState::Restoring {
                received,
                after_cursor,
                ..
            } => Some(self.feed_notice_card(
                "feed-state-restoring",
                "Restoring Session history",
                if after_cursor.is_some() {
                    format!("Received {received} newer events to verify.")
                } else {
                    format!("Received {received} events to verify.")
                },
                false,
                cx,
            )),
            FeedState::Backoff {
                attempt, reason, ..
            } => Some(self.feed_notice_card(
                "feed-state-reconnecting",
                "Reconnecting to Session",
                format!("Attempt {attempt}. {reason}"),
                true,
                cx,
            )),
            FeedState::Fatal { message, .. } => Some(self.feed_notice_card(
                "verified-prefix-error",
                "Verified history paused",
                message.clone(),
                false,
                cx,
            )),
            FeedState::Live | FeedState::Closed => None,
        }
    }

    fn feed_notice_card(
        &self,
        selector: &'static str,
        title: &'static str,
        detail: impl Into<String>,
        retryable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = detail.into();
        div()
            .id(selector)
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .p_3()
            .rounded(px(theme::ROW_RADIUS))
            .bg(rgb(if selector == "verified-prefix-error" {
                0xfff7ed
            } else {
                theme::SURFACE
            }))
            .border_1()
            .border_color(rgb(if selector == "verified-prefix-error" {
                0xd89a6a
            } else {
                theme::BORDER
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(rgb(theme::TEXT_PRIMARY))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_TINY))
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_2()
                    .when(retryable, |actions| {
                        actions.child(
                            div()
                                .id("feed-retry")
                                .debug_selector(|| "feed-retry".to_owned())
                                .cursor_pointer()
                                .px_3()
                                .py_1()
                                .rounded(px(theme::ROW_RADIUS - 2.0))
                                .bg(rgb(theme::SELECTED))
                                .text_size(px(theme::TEXT_TINY))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.runtime.retry_now() {
                                        this.diagnostic = None;
                                    }
                                    cx.notify();
                                }))
                                .child("Retry"),
                        )
                    })
                    .child(
                        div()
                            .id("feed-open-terminal")
                            .debug_selector(|| "feed-open-terminal".to_owned())
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded(px(theme::ROW_RADIUS - 2.0))
                            .border_1()
                            .border_color(rgb(theme::BORDER))
                            .text_size(px(theme::TEXT_TINY))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.runtime.set_presentation(RuntimePresentation::Terminal);
                                this.terminal_focus.focus(window);
                                cx.notify();
                            }))
                            .child("Open Terminal"),
                    ),
            )
            .into_any_element()
    }

    fn composer_stage(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.runtime.can_submit();
        div()
            .id("composer")
            .debug_selector(move || {
                if enabled {
                    "composer-enabled".to_owned()
                } else {
                    "composer-disabled".to_owned()
                }
            })
            .track_focus(&self.composer_focus)
            .key_context("NopalComposer")
            .flex()
            .flex_col()
            .flex_none()
            .mx_4()
            .mb_4()
            .min_h(px(92.0))
            .p_3()
            .rounded(px(theme::RADIUS))
            .bg(rgb(theme::SURFACE))
            .border_1()
            .border_color(rgb(if focused && enabled {
                theme::ACCENT
            } else {
                theme::BORDER
            }))
            .shadow_sm()
            .child(composer_canvas(
                self.composer.text().to_owned(),
                self.composer.selection(),
                self.composer.cursor(),
                self.composer.cursor_line(),
                cx.entity(),
                self.composer_focus.clone(),
            ))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(theme::TEXT_TINY))
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child("Shift-Enter for a new line")
                    .child(if enabled {
                        "Enter to send"
                    } else {
                        "Waiting for live Session"
                    }),
            )
            .on_action(cx.listener(Self::paste_composer))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    if !this.runtime.can_submit() {
                        return;
                    }
                    this.composer_focus.focus(window);
                    if let Some(index) = this.composer_index_at(event.position) {
                        this.composer.move_to(index, event.modifiers.shift);
                        this.composer_selecting = true;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.composer_selecting
                    && event.dragging()
                    && let Some(index) = this.composer_index_at(event.position)
                {
                    this.composer.move_to(index, true);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.composer_selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.composer_selecting = false),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if !this.runtime.can_submit() || this.handle_composer_key(&event.keystroke, cx) {
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
    }

    fn submit_composer(&mut self) {
        let original = self.composer.text().to_owned();
        let Some(submission) = self.composer.take_submission() else {
            return;
        };
        match self.runtime.submit_prompt(&submission) {
            SubmitOutcome::Sent { .. } => self.diagnostic = None,
            SubmitOutcome::RestoreText { reason, .. } => {
                self.composer.replace(None, &original);
                self.diagnostic = Some(reason);
            }
        }
    }

    fn paste_composer(&mut self, _: &PasteComposer, _: &mut Window, cx: &mut Context<Self>) {
        if !self.runtime.can_submit() {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.composer.replace(None, &text);
            cx.notify();
        }
    }

    fn handle_composer_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let modifiers = keystroke.modifiers;
        let command = modifiers.platform || modifiers.control;
        let extend = modifiers.shift;
        match keystroke.key.as_str() {
            "enter" => {
                if extend {
                    self.composer.insert_newline();
                } else {
                    self.submit_composer();
                }
            }
            "backspace" => {
                self.composer.delete_backward();
            }
            "delete" => {
                self.composer.delete_forward();
            }
            "left" if modifiers.platform => self.composer.move_line_start(extend),
            "right" if modifiers.platform => self.composer.move_line_end(extend),
            "left" => self.composer.move_left(extend, modifiers.alt),
            "right" => self.composer.move_right(extend, modifiers.alt),
            "up" => self.composer.move_vertical(-1, extend),
            "down" => self.composer.move_vertical(1, extend),
            "home" => self.composer.move_line_start(extend),
            "end" => self.composer.move_line_end(extend),
            "a" if command => self.composer.select_all(),
            "c" if command => {
                if let Some(text) = self.composer.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                }
            }
            "x" if command => {
                if let Some(text) = self.composer.selected_text().map(ToOwned::to_owned) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.composer.replace(None, "");
                }
            }
            "z" if command && extend => {
                self.composer.redo();
            }
            "z" if command => {
                self.composer.undo();
            }
            "y" if modifiers.control => {
                self.composer.redo();
            }
            _ => return false,
        }
        true
    }

    fn composer_index_at(&self, position: Point<Pixels>) -> Option<usize> {
        let first = self.composer_lines.first()?;
        let last = self.composer_lines.last()?;
        let line = if position.y < first.bounds.top() {
            first
        } else if position.y > last.bounds.bottom() {
            last
        } else {
            self.composer_lines
                .iter()
                .find(|line| position.y <= line.bounds.bottom())?
        };
        let local_x = (position.x - line.bounds.left()).max(px(0.0));
        Some(
            line.text_range.start
                + line
                    .line
                    .closest_index_for_x(local_x)
                    .min(line.text_range.len()),
        )
    }

    fn terminal_stage(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.terminal_controller() {
            Some(controller) => terminal_canvas(
                controller.snapshot(),
                controller.selection(),
                self.marked_text.clone(),
                cx.entity(),
                self.terminal_focus.clone(),
            )
            .into_any_element(),
            None => div()
                .flex()
                .flex_col()
                .gap_2()
                .child("No live Session is attached")
                .child(
                    div()
                        .text_color(rgb(0x999991))
                        .child("Select a Plot with an available tmux Session."),
                )
                .into_any_element(),
        };
        let status = match self
            .terminal_controller()
            .map(TerminalController::connection_state)
        {
            Some(ConnectionState::Connected) if focused => {
                "Focused · typing goes to tmux".to_owned()
            }
            Some(ConnectionState::Connected) => "Click to focus · Shift-drag selects".to_owned(),
            Some(ConnectionState::Degraded(error)) => {
                format!("Interactive · fixed pane size · {error}")
            }
            Some(ConnectionState::Reconnecting(detail)) => format!("Reconnecting · {detail}"),
            Some(ConnectionState::ReadOnly(error)) => format!("Read only · {error}"),
            Some(ConnectionState::Unavailable(error)) => format!("Unavailable · {error}"),
            None => "Unavailable".to_owned(),
        };
        div()
            .id("terminal-stage")
            .track_focus(&self.terminal_focus)
            .key_context("NopalTerminal")
            .cursor_text()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h_0()
            .mx_4()
            .mb_4()
            .rounded(px(theme::RADIUS))
            .bg(rgb(theme::TERMINAL_SURFACE))
            .border_1()
            .border_color(rgb(if focused { 0x6e9ee8 } else { theme::BORDER }))
            .text_color(rgb(theme::TERMINAL_TEXT))
            .font_family("SF Mono")
            .text_size(px(theme::TEXT_SMALL))
            .overflow_hidden()
            .child(
                div()
                    .id("terminal-history-boundary")
                    .debug_selector(|| "terminal-history-boundary".to_owned())
                    .flex_none()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(theme::BORDER))
                    .text_size(px(theme::TEXT_TINY))
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(TERMINAL_HISTORY_LABEL),
            )
            .child(div().flex_grow().min_h_0().p_3().child(content))
            .child(
                div()
                    .flex_none()
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(rgb(theme::BORDER))
                    .text_size(px(theme::TEXT_TINY))
                    .text_color(rgb(if focused {
                        0xaecbff
                    } else {
                        theme::TEXT_TERTIARY
                    }))
                    .child(status),
            )
            .on_action(cx.listener(Self::paste_terminal))
            .on_action(cx.listener(Self::copy_terminal))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_terminal_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_terminal_scroll))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let handled = this
                    .terminal_controller_mut()
                    .is_some_and(|controller| controller.send_keystroke(&event.keystroke));
                if handled {
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
    }

    fn paste_terminal(&mut self, _: &PasteTerminal, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if self
            .terminal_controller_mut()
            .is_some_and(|controller| controller.send_paste(&text))
        {
            cx.notify();
        }
    }

    fn copy_terminal(&mut self, _: &CopyTerminal, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self
            .terminal_controller()
            .and_then(TerminalController::selected_text)
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn on_terminal_mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_focus.focus(window);
        let Some(cell) = self.terminal_cell(event.position) else {
            return;
        };
        let Some(controller) = self.terminal_controller_mut() else {
            return;
        };
        controller.set_focused(true);
        if controller.input_mode().mouse_reporting && !event.modifiers.shift {
            controller.clear_selection();
            controller.send_mouse(0, true, cell);
        } else {
            controller.begin_selection((cell.1, cell.0));
            self.selecting = true;
        }
        cx.notify();
    }

    fn on_terminal_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting || !event.dragging() {
            return;
        }
        let Some(cell) = self.terminal_cell(event.position) else {
            return;
        };
        if let Some(controller) = self.terminal_controller_mut() {
            controller.update_selection((cell.1, cell.0));
            cx.notify();
        }
    }

    fn on_terminal_mouse_up(
        &mut self,
        event: &gpui::MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cell = self.terminal_cell(event.position);
        if let (Some(controller), Some(cell)) = (self.terminal_controller_mut(), cell)
            && controller.input_mode().mouse_reporting
            && !event.modifiers.shift
        {
            controller.send_mouse(0, false, cell);
        }
        self.selecting = false;
        cx.notify();
    }

    fn on_terminal_scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(self.line_height);
        let lines = (f32::from(delta.y) / f32::from(self.line_height)).round() as i32;
        if lines == 0 {
            return;
        }
        let cell = self.terminal_cell(event.position);
        let Some(controller) = self.terminal_controller_mut() else {
            return;
        };
        let handled = if controller.input_mode().mouse_reporting && !event.modifiers.shift {
            cell.is_some_and(|cell| {
                controller.send_mouse(if lines > 0 { 64 } else { 65 }, true, cell)
            })
        } else {
            controller.scroll_lines(lines)
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn terminal_cell(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let bounds = self.terminal_bounds?;
        if !bounds.contains(&position) {
            return None;
        }
        let column = (f32::from(position.x - bounds.left()) / f32::from(self.cell_width)) as usize;
        let row = (f32::from(position.y - bounds.top()) / f32::from(self.line_height)) as usize;
        Some((column, row))
    }

    fn inspector(&self, plot: Option<&DesktopPlot>) -> impl IntoElement {
        let progress = plot
            .map(|plot| plot.progress.clone())
            .unwrap_or_else(|| "Unavailable".to_owned());
        let conditions = plot
            .map(|plot| {
                if plot.conditions.is_empty() {
                    "None".to_owned()
                } else {
                    plot.conditions.join(", ")
                }
            })
            .unwrap_or_else(|| "None".to_owned());
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(theme::INSPECTOR_WIDTH))
            .h_full()
            .p_4()
            .gap_4()
            .bg(rgb(theme::APP_BACKGROUND))
            .border_l_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .child("Plot inspector"),
            )
            .child(fact("Progress", &progress))
            .child(fact("Conditions", &conditions))
            .when_some(self.diagnostic.clone(), |inspector, diagnostic| {
                inspector.child(
                    div()
                        .mt_2()
                        .p_3()
                        .rounded(px(theme::ROW_RADIUS))
                        .bg(rgb(0xfff4db))
                        .text_size(px(theme::TEXT_TINY))
                        .text_color(rgb(0x72510d))
                        .child(diagnostic),
                )
            })
    }
}

struct TerminalPaintState {
    lines: Vec<(gpui::ShapedLine, gpui::Pixels)>,
    cursor: Option<(gpui::Pixels, gpui::Pixels)>,
    marked_text: Option<(gpui::ShapedLine, gpui::Pixels, gpui::Pixels)>,
    selection: Vec<Bounds<Pixels>>,
    cell_width: Pixels,
    line_height: gpui::Pixels,
}

fn terminal_canvas<C>(
    snapshot: TerminalSnapshot,
    selection: Option<((usize, usize), (usize, usize))>,
    marked_text: String,
    input: Entity<DesktopShell<C>>,
    focus: FocusHandle,
) -> impl IntoElement
where
    C: RuntimeConnector<Terminal = LiveTerminalBinding> + 'static,
{
    let paint_snapshot = snapshot.clone();
    let paint_input = input.clone();
    let paint_focus = focus.clone();
    canvas(
        move |bounds, window, _cx: &mut App| {
            let font_size = px(theme::TEXT_SMALL);
            let line_height = px(18.0);
            let base_font = font("SF Mono");
            let metric_run = TextRun {
                len: 1,
                font: base_font.clone(),
                color: rgb(theme::TERMINAL_TEXT).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let metric = window.text_system().shape_line(
                SharedString::from("M"),
                font_size,
                &[metric_run],
                None,
            );
            let cell_width = metric.x_for_index(1);
            let visible_rows = (f32::from(bounds.size.height) / f32::from(line_height)) as usize;
            let mut lines = Vec::new();
            for row in snapshot.rows.iter().take(visible_rows) {
                let mut text = String::new();
                let mut text_runs = Vec::new();
                let mut column = 0usize;
                for run in &row.runs {
                    if run.start_column > column {
                        let gap = " ".repeat(run.start_column - column);
                        text.push_str(&gap);
                        text_runs.push(TextRun {
                            len: gap.len(),
                            font: base_font.clone(),
                            color: rgb(theme::TERMINAL_TEXT).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        });
                    }
                    text.push_str(&run.text);
                    let mut run_font = base_font.clone();
                    if run
                        .style
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::BOLD)
                    {
                        run_font.weight = FontWeight::BOLD;
                    }
                    text_runs.push(TextRun {
                        len: run.text.len(),
                        font: run_font,
                        color: rgb(terminal_foreground(&run.style)).into(),
                        background_color: terminal_background(&run.style)
                            .map(|color| rgb(color).into()),
                        underline: None,
                        strikethrough: None,
                    });
                    column = run.start_column + run.cell_width;
                }
                if text.is_empty() {
                    text.push(' ');
                    text_runs.push(TextRun {
                        len: 1,
                        font: base_font.clone(),
                        color: rgb(theme::TERMINAL_TEXT).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });
                }
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    font_size,
                    &text_runs,
                    None,
                );
                lines.push((shaped, px(row.line as f32 * 18.0)));
            }
            let cursor = (paint_snapshot.cursor.line < visible_rows
                && paint_snapshot.cursor.shape
                    != alacritty_terminal::vte::ansi::CursorShape::Hidden)
                .then_some((
                    cell_width * paint_snapshot.cursor.column,
                    line_height * paint_snapshot.cursor.line,
                ));
            let marked_text = (!marked_text.is_empty()).then(|| {
                let run = TextRun {
                    len: marked_text.len(),
                    font: base_font.clone(),
                    color: rgb(0xaecbff).into(),
                    background_color: Some(rgb(0x203855).into()),
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(
                    SharedString::from(marked_text.clone()),
                    font_size,
                    &[run],
                    None,
                );
                let (x, y) = cursor.unwrap_or((px(0.0), px(0.0)));
                (line, x, y)
            });
            let mut selection_bounds = Vec::new();
            if let Some((start, end)) = selection {
                let (start, end) = if start <= end {
                    (start, end)
                } else {
                    (end, start)
                };
                for row in start.0..=end.0.min(visible_rows.saturating_sub(1)) {
                    let from = if row == start.0 { start.1 } else { 0 };
                    let to = if row == end.0 {
                        end.1.saturating_add(1)
                    } else {
                        snapshot.columns
                    };
                    selection_bounds.push(Bounds::new(
                        point(cell_width * from, line_height * row),
                        size(cell_width * to.saturating_sub(from), line_height),
                    ));
                }
            }
            TerminalPaintState {
                lines,
                cursor,
                marked_text,
                selection: selection_bounds,
                cell_width,
                line_height,
            }
        },
        move |bounds, state, window, cx| {
            window.handle_input(
                &paint_focus,
                ElementInputHandler::new(bounds, paint_input.clone()),
                cx,
            );
            for selection in state.selection {
                window.paint_quad(fill(
                    Bounds::new(bounds.origin + selection.origin, selection.size),
                    rgb(0x29486f),
                ));
            }
            for (line, y) in state.lines {
                let _ = line.paint(
                    point(bounds.left(), bounds.top() + y),
                    state.line_height,
                    window,
                    cx,
                );
            }
            if let Some((line, x, y)) = state.marked_text {
                let _ = line.paint(
                    point(bounds.left() + x, bounds.top() + y),
                    state.line_height,
                    window,
                    cx,
                );
            }
            if let Some((x, y)) = state.cursor {
                window.paint_quad(fill(
                    gpui::Bounds::new(
                        point(bounds.left() + x, bounds.top() + y),
                        size(px(1.5), state.line_height),
                    ),
                    rgb(0xd8d8d3),
                ));
            }
            input.update(cx, |shell, cx| {
                shell.terminal_bounds = Some(bounds);
                shell.cell_width = state.cell_width;
                shell.line_height = state.line_height;
                if let Some(controller) = shell.terminal_controller_mut()
                    && controller.is_focused()
                {
                    let grid =
                        TerminalController::<Box<dyn crate::tmux::PaneTransport>>::grid_for_pixels(
                            f32::from(bounds.size.width),
                            f32::from(bounds.size.height),
                            f32::from(state.cell_width),
                            f32::from(state.line_height),
                        );
                    if controller.resize(grid.0, grid.1) {
                        cx.notify();
                    }
                }
            });
        },
    )
    .size_full()
}

struct ComposerPaintState {
    lines: Vec<ComposerLineGeometry>,
    cursor: Point<Pixels>,
    selections: Vec<Bounds<Pixels>>,
    line_height: Pixels,
}

fn composer_canvas<C>(
    text: String,
    selection: Range<usize>,
    cursor_offset: usize,
    cursor_line: usize,
    input: Entity<DesktopShell<C>>,
    focus: FocusHandle,
) -> impl IntoElement
where
    C: RuntimeConnector<Terminal = LiveTerminalBinding> + 'static,
{
    let paint_input = input.clone();
    let paint_focus = focus.clone();
    canvas(
        move |bounds, window, _cx: &mut App| {
            let font_size = px(theme::TEXT_BASE);
            let line_height = px(20.0);
            let base_font = font(".SystemUIFont");
            let line_starts = std::iter::once(0)
                .chain(text.match_indices('\n').map(|(index, _)| index + 1))
                .collect::<Vec<_>>();
            let first_visible = cursor_line.saturating_sub(2);
            let mut lines = Vec::new();
            let mut cursor = point(px(0.0), px(0.0));
            let mut selections = Vec::new();
            for (visible_index, line_index) in
                (first_visible..line_starts.len()).take(3).enumerate()
            {
                let start = line_starts[line_index];
                let end = text[start..]
                    .find('\n')
                    .map_or(text.len(), |offset| start + offset);
                let line_text = &text[start..end];
                let placeholder = text.is_empty();
                let visible = if placeholder {
                    "Send an instruction to this Session"
                } else if line_text.is_empty() {
                    " "
                } else {
                    line_text
                };
                let run = TextRun {
                    len: visible.len(),
                    font: base_font.clone(),
                    color: rgb(if placeholder {
                        theme::TEXT_TERTIARY
                    } else {
                        theme::TEXT_PRIMARY
                    })
                    .into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(visible.to_owned()),
                    font_size,
                    &[run],
                    None,
                );
                let y = line_height * visible_index;
                if line_index == cursor_line {
                    cursor = point(shaped.x_for_index(cursor_offset - start), y);
                }
                let selected_start = selection.start.max(start).min(end);
                let selected_end = selection.end.max(start).min(end);
                if selected_start < selected_end {
                    let left = shaped.x_for_index(selected_start - start);
                    let right = shaped.x_for_index(selected_end - start);
                    selections.push(Bounds::new(
                        point(bounds.left() + left, bounds.top() + y),
                        size(right - left, line_height),
                    ));
                }
                lines.push(ComposerLineGeometry {
                    text_range: start..end,
                    bounds: Bounds::new(
                        point(bounds.left(), bounds.top() + y),
                        size(bounds.size.width, line_height),
                    ),
                    line: shaped,
                });
            }
            let max_y = (bounds.size.height - line_height).max(px(0.0));
            cursor.y = cursor.y.min(max_y);
            ComposerPaintState {
                lines,
                cursor,
                selections,
                line_height,
            }
        },
        move |bounds, state, window, cx| {
            window.handle_input(
                &paint_focus,
                ElementInputHandler::new(bounds, paint_input.clone()),
                cx,
            );
            for selection in &state.selections {
                window.paint_quad(fill(*selection, rgb(0xcddcf2)));
            }
            for line in &state.lines {
                let _ = line
                    .line
                    .paint(line.bounds.origin, state.line_height, window, cx);
            }
            if paint_focus.is_focused(window) {
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.left() + state.cursor.x,
                            bounds.top() + state.cursor.y,
                        ),
                        size(px(1.5), state.line_height),
                    ),
                    rgb(theme::TEXT_PRIMARY),
                ));
            }
            input.update(cx, |shell, _| {
                shell.composer_lines = state.lines.clone();
                shell.composer_bounds = Some(Bounds::new(
                    point(
                        bounds.left() + state.cursor.x,
                        bounds.top() + state.cursor.y,
                    ),
                    size(px(2.0), state.line_height),
                ));
            });
        },
    )
    .w_full()
    .h(px(56.0))
}

impl<C> Render for DesktopShell<C>
where
    C: RuntimeConnector<Terminal = LiveTerminalBinding> + 'static,
{
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = LayoutMode::for_width(f32::from(window.bounds().size.width));
        let selected_plot = self.runtime.selected_plot().cloned();
        let terminal_focused = self.terminal_focus.is_focused(window);
        let composer_focused = self.composer_focus.is_focused(window);
        if let Some(controller) = self.terminal_controller_mut() {
            controller.set_focused(terminal_focused);
        }
        div()
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(rgb(theme::APP_BACKGROUND))
            .text_size(px(theme::TEXT_BASE))
            .when(layout.rail_width() > 0.0, |root| {
                root.child(self.rail(layout.rail_width(), cx))
            })
            .when(
                matches!(layout, LayoutMode::Narrow) && self.narrow_rail_open,
                |root| {
                    root.child(
                        div()
                            .id("narrow-plot-drawer")
                            .debug_selector(|| "narrow-plot-drawer".to_owned())
                            .flex_none()
                            .child(self.rail(theme::COMPACT_RAIL_WIDTH, cx)),
                    )
                },
            )
            .child(self.stage(
                layout,
                selected_plot.as_ref(),
                terminal_focused,
                composer_focused,
                cx,
            ))
            .when(self.inspector_open && layout.shows_inspector(), |root| {
                root.child(self.inspector(selected_plot.as_ref()))
            })
    }
}

impl<C> EntityInputHandler for DesktopShell<C>
where
    C: RuntimeConnector<Terminal = LiveTerminalBinding> + 'static,
{
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.composer_focus.is_focused(window) {
            let byte_range = self.composer.byte_range_from_utf16(range.clone());
            adjusted_range.replace(range);
            return self.composer.text().get(byte_range).map(ToOwned::to_owned);
        }
        let utf16 = self.marked_text.encode_utf16().collect::<Vec<_>>();
        let start = range.start.min(utf16.len());
        let end = range.end.min(utf16.len()).max(start);
        adjusted_range.replace(start..end);
        String::from_utf16(&utf16[start..end]).ok()
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if self.composer_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: self.composer.utf16_selection(),
                reversed: self.composer.selection_reversed(),
            });
        }
        let cursor = self.marked_text.encode_utf16().count();
        Some(UTF16Selection {
            range: cursor..cursor,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if self.composer_focus.is_focused(window) {
            return self
                .composer
                .marked_range()
                .map(|range| self.composer.utf16_range(range));
        }
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_focus.is_focused(window) {
            self.composer.unmark();
            cx.notify();
            return;
        }
        let text = std::mem::take(&mut self.marked_text);
        if !text.is_empty()
            && self
                .terminal_controller_mut()
                .is_some_and(|controller| controller.send_text(&text))
        {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer_focus.is_focused(window) {
            if !self.runtime.can_submit() {
                return;
            }
            let range = range.map(|range| self.composer.byte_range_from_utf16(range));
            self.composer.replace(range, text);
            cx.notify();
            return;
        }
        self.marked_text.clear();
        if self
            .terminal_controller_mut()
            .is_some_and(|controller| controller.send_text(text))
        {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer_focus.is_focused(window) {
            if !self.runtime.can_submit() {
                return;
            }
            let range = range.map(|range| self.composer.byte_range_from_utf16(range));
            self.composer.replace_and_mark(range, new_text);
            cx.notify();
            return;
        }
        self.marked_text.clear();
        self.marked_text.push_str(new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.composer_focus.is_focused(window) {
            return self.composer_bounds;
        }
        let cursor = self.terminal_controller()?.snapshot().cursor;
        Some(Bounds::new(
            point(
                element_bounds.left() + self.cell_width * cursor.column,
                element_bounds.top() + self.line_height * cursor.line,
            ),
            size(self.cell_width, self.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if self.composer_focus.is_focused(window) {
            return self
                .composer_index_at(point)
                .map(|index| self.composer.utf16_range(0..index).end);
        }
        Some(0)
    }
}

fn activity_id(key: &DesktopActivityKey) -> String {
    match key {
        DesktopActivityKey::Session(session_id) => format!("session-{session_id}"),
        DesktopActivityKey::Execution {
            service_id,
            repo_id,
            run_id,
        } => format!("execution-{service_id}-{repo_id}-{run_id}"),
    }
}

fn runtime_status_label(status: &RuntimeStatus) -> String {
    match status {
        RuntimeStatus::Ready => "Connected".to_owned(),
        RuntimeStatus::StructuredOnly { .. } => "Output only".to_owned(),
        RuntimeStatus::TerminalOnly { .. } => "Terminal only".to_owned(),
        RuntimeStatus::ExecutionSelected => "Execution selected".to_owned(),
        RuntimeStatus::Unavailable { .. } => "Unavailable".to_owned(),
        RuntimeStatus::Degraded { .. } => "Needs attention".to_owned(),
    }
}

fn output_empty_state(status: &RuntimeStatus) -> String {
    match status {
        RuntimeStatus::Ready | RuntimeStatus::StructuredOnly { .. } => {
            "Send an instruction to begin this Session timeline.".to_owned()
        }
        RuntimeStatus::TerminalOnly { structured_error } => {
            format!("Structured output is unavailable: {structured_error}")
        }
        RuntimeStatus::ExecutionSelected => {
            "Select a Session to view its structured output.".to_owned()
        }
        RuntimeStatus::Unavailable { reason } => reason.clone(),
        RuntimeStatus::Degraded { detail } => detail.clone(),
    }
}

fn verified_failure_detail(failure: &TimelineFailure) -> String {
    match failure {
        TimelineFailure::Feed { message, .. } => message.clone(),
        TimelineFailure::Gap {
            expected_sequence,
            actual_sequence,
            ..
        } => format!(
            "Expected event {expected_sequence}, received {actual_sequence}. Verified events remain visible."
        ),
        TimelineFailure::ReplayCompleteMismatch(_) => {
            "Replay completion did not match the verified history. Verified events remain visible."
                .to_owned()
        }
        _ => "Session history could not be verified. Verified events remain visible.".to_owned(),
    }
}

fn timeline_event_row(event: SessionEvent) -> AnyElement {
    let event_id = event.event_id;
    let row = div()
        .id(SharedString::from(format!("timeline-event-{event_id}")))
        .debug_selector(move || format!("timeline-event-{event_id}"))
        .flex()
        .flex_col()
        .gap_1()
        .text_color(rgb(theme::OUTPUT_TEXT));
    match event.event {
        SessionEventPayload::SessionReady { .. } => row
            .text_size(px(theme::TEXT_TINY))
            .text_color(rgb(theme::TEXT_TERTIARY))
            .child("Session connected")
            .into_any_element(),
        SessionEventPayload::UserMessage { text, .. } => row
            .items_end()
            .child(
                div()
                    .max_w(px(620.0))
                    .px_3()
                    .py_2()
                    .rounded(px(theme::RADIUS))
                    .bg(rgb(theme::USER_CARD))
                    .child(text),
            )
            .into_any_element(),
        SessionEventPayload::AssistantMessage { text, .. } => row
            .child(div().text_size(px(theme::TEXT_BASE)).child(text))
            .into_any_element(),
        SessionEventPayload::SessionError { message, .. } => row
            .p_3()
            .rounded(px(theme::RADIUS))
            .border_1()
            .border_color(rgb(0xd89a6a))
            .bg(rgb(0xfff4e8))
            .child(message)
            .into_any_element(),
    }
}

fn fact(label: &str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TEXT_TINY))
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(rgb(theme::TEXT_PRIMARY))
                .child(value.to_owned()),
        )
}

fn short_id(id: &str, prefix: &str) -> String {
    let visible = id.strip_prefix(prefix).unwrap_or(id);
    if visible.chars().count() <= 12 {
        visible.to_owned()
    } else {
        format!("{}…", visible.chars().take(11).collect::<String>())
    }
}

fn terminal_foreground(style: &TerminalStyle) -> u32 {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    match style.foreground {
        Color::Spec(color) => {
            (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
        }
        Color::Indexed(index) => indexed_color(index),
        Color::Named(NamedColor::Black) => 0x1f1f1f,
        Color::Named(NamedColor::Red | NamedColor::BrightRed) => 0xf26d78,
        Color::Named(NamedColor::Green | NamedColor::BrightGreen) => 0x78c98a,
        Color::Named(NamedColor::Yellow | NamedColor::BrightYellow) => 0xe3c36a,
        Color::Named(NamedColor::Blue | NamedColor::BrightBlue) => 0x75a7e8,
        Color::Named(NamedColor::Magenta | NamedColor::BrightMagenta) => 0xc995d9,
        Color::Named(NamedColor::Cyan | NamedColor::BrightCyan) => 0x70c3c8,
        Color::Named(_) => theme::TERMINAL_TEXT,
    }
}

fn terminal_background(style: &TerminalStyle) -> Option<u32> {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    match style.background {
        Color::Named(NamedColor::Background) => None,
        Color::Spec(color) => {
            Some((u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b))
        }
        Color::Indexed(index) => Some(indexed_color(index)),
        Color::Named(NamedColor::Black) => Some(0x1f1f1f),
        Color::Named(_) => Some(theme::TERMINAL_SURFACE),
    }
}

fn indexed_color(index: u8) -> u32 {
    const ANSI: [u32; 16] = [
        0x1f1f1f, 0xd65f68, 0x72b880, 0xd0ad55, 0x6795d0, 0xb886c8, 0x63afb4, 0xd8d8d3, 0x777771,
        0xf26d78, 0x78c98a, 0xe3c36a, 0x75a7e8, 0xc995d9, 0x70c3c8, 0xf4f4ef,
    ];
    ANSI.get(usize::from(index))
        .copied()
        .unwrap_or(theme::TERMINAL_TEXT)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use gpui::{
        AppContext, ClipboardItem, EntityInputHandler, Keystroke, Modifiers, MouseButton,
        MouseDownEvent, TestAppContext, VisualTestContext, point, px, size,
    };

    use crate::activity::VerifiedSessionEvent;
    use crate::interaction::TerminalController;
    use crate::model::{
        DesktopActivity, DesktopField, DesktopPlot, DesktopSessionProtocol, SelectedSessionContext,
    };
    use crate::session_feed::{
        ClientFeedFrame, FeedConnection, FeedError, FeedState, FeedTransport, FeedUpdate,
        SessionFeedContext, SessionFeedServerFrame,
    };
    use crate::session_runtime::{
        LiveTerminalBinding, RuntimeConnector, RuntimePresentation, SessionRuntime,
        TerminalConnection,
    };
    use crate::terminal::TerminalSurface;
    use crate::tmux::PaneTransport;

    use super::{DesktopShell, LayoutMode, TERMINAL_HISTORY_LABEL};

    #[derive(Clone, Default)]
    struct RecordingTransport(Rc<RefCell<Vec<Vec<u8>>>>);

    impl PaneTransport for RecordingTransport {
        fn send_input(&self, _: &str, bytes: &[u8]) -> Result<(), String> {
            self.0.borrow_mut().push(bytes.to_vec());
            Ok(())
        }

        fn resize_pane(&self, _: &str, _: usize, _: usize) -> Result<(), String> {
            Ok(())
        }
    }

    struct TestFeedConnection {
        replay_complete: Option<nopal_feed_client::session::SessionServerFrame>,
    }

    impl FeedConnection for TestFeedConnection {
        fn send(&mut self, frame: ClientFeedFrame) -> Result<(), FeedError> {
            if let ClientFeedFrame::Subscribe(subscribe) = frame
                && let Some(nopal_feed_client::session::SessionServerFrame::ReplayComplete(
                    complete,
                )) = self.replay_complete.as_mut()
            {
                complete.request_id = subscribe.request_id;
            }
            Ok(())
        }

        fn try_receive(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
            Ok(self
                .replay_complete
                .take()
                .map(SessionFeedServerFrame::from))
        }

        fn close(&mut self) {}
    }

    #[derive(Clone)]
    struct TestFeedTransport {
        remaining_connect_failures: Rc<Cell<usize>>,
        connect_count: Rc<Cell<usize>>,
    }

    impl FeedTransport for TestFeedTransport {
        type Connection = TestFeedConnection;

        fn connect(&mut self, context: &SessionFeedContext) -> Result<Self::Connection, FeedError> {
            self.connect_count.set(self.connect_count.get() + 1);
            let remaining = self.remaining_connect_failures.get();
            if remaining > 0 {
                self.remaining_connect_failures.set(remaining - 1);
                return Err(FeedError::endpoint_absent(
                    "test Session endpoint is temporarily absent",
                ));
            }
            Ok(TestFeedConnection {
                replay_complete: Some(
                    nopal_feed_client::session::SessionServerFrame::ReplayComplete(
                        nopal_feed_client::session::SessionReplayComplete {
                            kind: nopal_feed_client::session::SESSION_REPLAY_COMPLETE_KIND
                                .to_owned(),
                            request_id: "shell-test-replay".to_owned(),
                            plot_id: context.plot_id.clone(),
                            session_id: context.session_id.clone(),
                            stream_id: format!("stream-{}", context.session_id),
                            cursor: None,
                            sequence: 0,
                            event_count: 0,
                            extra: BTreeMap::new(),
                        },
                    ),
                ),
            })
        }
    }

    #[derive(Clone, Default)]
    struct TestConnector {
        transport: RecordingTransport,
        structured_available: bool,
        remaining_connect_failures: Rc<Cell<usize>>,
        connect_count: Rc<Cell<usize>>,
    }

    impl RuntimeConnector for TestConnector {
        type FeedTransport = TestFeedTransport;
        type Terminal = LiveTerminalBinding;

        fn feed_transport(
            &self,
            _: &SelectedSessionContext,
        ) -> Result<Self::FeedTransport, String> {
            if !self.structured_available {
                return Err("structured test connection is unavailable".to_owned());
            }
            Ok(TestFeedTransport {
                remaining_connect_failures: self.remaining_connect_failures.clone(),
                connect_count: self.connect_count.clone(),
            })
        }

        fn bind_terminal(
            &self,
            context: &SelectedSessionContext,
        ) -> Result<TerminalConnection<Self::Terminal>, String> {
            let pane_id = context
                .host_pane
                .clone()
                .ok_or_else(|| "test Session has no pane".to_owned())?;
            let controller = TerminalController::new(
                pane_id.clone(),
                TerminalSurface::new(80, 24),
                Box::new(self.transport.clone()) as Box<dyn PaneTransport>,
                (80, 24),
            );
            let (_sender, output) = async_channel::unbounded();
            Ok(TerminalConnection {
                binding: LiveTerminalBinding::from_controller(pane_id, controller),
                output,
            })
        }
    }

    fn test_runtime(
        field: DesktopField,
        transport: RecordingTransport,
    ) -> SessionRuntime<TestConnector> {
        SessionRuntime::new(
            field,
            TestConnector {
                transport,
                structured_available: false,
                remaining_connect_failures: Rc::default(),
                connect_count: Rc::default(),
            },
        )
    }

    fn live_test_runtime(
        field: DesktopField,
        transport: RecordingTransport,
    ) -> SessionRuntime<TestConnector> {
        let mut runtime = SessionRuntime::new(
            field,
            TestConnector {
                transport,
                structured_available: true,
                remaining_connect_failures: Rc::default(),
                connect_count: Rc::default(),
            },
        );
        runtime.drain_at(0);
        runtime
    }

    fn selection_field() -> DesktopField {
        let session = |session_id: &str, pane_id: &str| DesktopActivity::Session {
            session_id: session_id.to_owned(),
            host_pane: Some(pane_id.to_owned()),
            state: "active".to_owned(),
            protocol: Some(DesktopSessionProtocol {
                kind: "nopal.session/v2".to_owned(),
                transport: "unix".to_owned(),
                address: format!("/tmp/{session_id}.sock"),
                state: "ready".to_owned(),
                extra: BTreeMap::new(),
            }),
        };
        DesktopField {
            plots: vec![
                DesktopPlot {
                    plot_id: "plot-a".to_owned(),
                    title: "Plot A".to_owned(),
                    progress: "Active".to_owned(),
                    conditions: Vec::new(),
                    activities: vec![
                        session("session-a", "%71"),
                        DesktopActivity::Execution {
                            service_id: "rondo".to_owned(),
                            repo_id: "repo-a".to_owned(),
                            run_id: "run-a".to_owned(),
                            status: "running".to_owned(),
                        },
                    ],
                    selected_session_id: Some("session-a".to_owned()),
                    extra: BTreeMap::new(),
                },
                DesktopPlot {
                    plot_id: "plot-b".to_owned(),
                    title: "Plot B".to_owned(),
                    progress: "Waiting".to_owned(),
                    conditions: Vec::new(),
                    activities: vec![session("session-b", "%72")],
                    selected_session_id: Some("session-b".to_owned()),
                    extra: BTreeMap::new(),
                },
            ],
            selected_plot_id: Some("plot-a".to_owned()),
            extra: BTreeMap::new(),
        }
    }

    fn open_selection_shell(
        cx: &mut TestAppContext,
    ) -> gpui::WindowHandle<DesktopShell<TestConnector>> {
        open_shell(cx, false)
    }

    fn open_shell(
        cx: &mut TestAppContext,
        structured_available: bool,
    ) -> gpui::WindowHandle<DesktopShell<TestConnector>> {
        cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let terminal_focus = cx.focus_handle();
                let composer_focus = cx.focus_handle();
                composer_focus.focus(window);
                cx.new(|_| {
                    let mut runtime = SessionRuntime::new(
                        selection_field(),
                        TestConnector {
                            transport: RecordingTransport::default(),
                            structured_available,
                            remaining_connect_failures: Rc::default(),
                            connect_count: Rc::default(),
                        },
                    );
                    runtime.drain_at(0);
                    DesktopShell::new(runtime, None, terminal_focus, composer_focus, None)
                })
            })
            .expect("test window")
        })
    }

    fn open_state_shell(
        cx: &mut TestAppContext,
        connect_failures: usize,
        drain: bool,
        draft: &str,
    ) -> (
        gpui::WindowHandle<DesktopShell<TestConnector>>,
        Rc<Cell<usize>>,
    ) {
        let remaining_connect_failures = Rc::new(Cell::new(connect_failures));
        let connect_count = Rc::new(Cell::new(0));
        let test_connect_count = connect_count.clone();
        let draft = draft.to_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let terminal_focus = cx.focus_handle();
                let composer_focus = cx.focus_handle();
                composer_focus.focus(window);
                let remaining_connect_failures = remaining_connect_failures.clone();
                let connect_count = connect_count.clone();
                let draft = draft.clone();
                cx.new(|_| {
                    let mut runtime = SessionRuntime::new(
                        selection_field(),
                        TestConnector {
                            transport: RecordingTransport::default(),
                            structured_available: true,
                            remaining_connect_failures,
                            connect_count,
                        },
                    );
                    runtime.set_composer_draft(draft);
                    if drain {
                        runtime.drain_at(0);
                    }
                    DesktopShell::new(runtime, None, terminal_focus, composer_focus, None)
                })
            })
            .expect("stateful test window")
        });
        (window, test_connect_count)
    }

    #[test]
    fn wide_layout_keeps_the_plot_rail_and_inspector_secondary() {
        let layout = LayoutMode::for_width(1440.0);

        assert_eq!(layout.rail_width(), 260.0);
        assert!(layout.shows_inspector());
        assert!(1440.0 - layout.rail_width() - 300.0 > 1440.0 / 2.0);
    }

    #[test]
    fn laptop_layout_preserves_the_live_stage_by_hiding_the_inspector() {
        let layout = LayoutMode::for_width(1024.0);

        assert_eq!(layout.rail_width(), 220.0);
        assert!(!layout.shows_inspector());
    }

    #[test]
    fn narrow_layout_gives_the_whole_window_to_the_selected_activity() {
        let layout = LayoutMode::for_width(720.0);

        assert_eq!(layout.rail_width(), 0.0);
        assert!(!layout.shows_inspector());
    }

    #[test]
    fn output_is_the_default_presentation() {
        assert_eq!(RuntimePresentation::default(), RuntimePresentation::Output);
    }

    #[test]
    fn terminal_history_boundary_label_is_exact() {
        assert_eq!(
            TERMINAL_HISTORY_LABEL,
            "Live Terminal - not part of Session history"
        );
    }

    #[gpui::test]
    fn restoring_state_disables_composer_retains_text_and_opens_labeled_terminal(
        cx: &mut TestAppContext,
    ) {
        let draft = "  exact restoring draft 🌵  ";
        let (window, _) = open_state_shell(cx, 0, false, draft);
        let mut visual = VisualTestContext::from_window(*window, cx);
        for width in [1440.0, 1024.0, 720.0] {
            visual.simulate_resize(size(px(width), px(800.0)));
            visual.run_until_parked();
            assert!(visual.debug_bounds("feed-state-restoring").is_some());
            assert!(visual.debug_bounds("composer-disabled").is_some());
            assert!(visual.debug_bounds("feed-open-terminal").is_some());
        }
        cx.dispatch_keystroke(*window, Keystroke::parse("enter").expect("Enter key"));
        window
            .update(cx, |shell, window, cx| {
                shell.replace_text_in_range(None, "must not replace", window, cx);
                shell.replace_and_mark_text_in_range(None, "must not mark", None, window, cx);
            })
            .expect("disabled direct text input");
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("retained restoring draft"),
            draft
        );

        let terminal = visual
            .debug_bounds("feed-open-terminal")
            .expect("Open Terminal action");
        visual.simulate_click(terminal.center(), Modifiers::default());
        assert!(visual.debug_bounds("terminal-history-boundary").is_some());
        window
            .update(cx, |shell, window, _| {
                assert_eq!(shell.runtime.presentation(), RuntimePresentation::Terminal);
                assert!(shell.terminal_focus.is_focused(window));
            })
            .expect("Terminal presentation and focus");
    }

    #[gpui::test]
    fn reconnecting_retry_action_reuses_the_feed_and_enables_composer(cx: &mut TestAppContext) {
        let draft = "keep while reconnecting";
        let (window, connect_count) = open_state_shell(cx, 1, true, draft);
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_resize(size(px(1024.0), px(800.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("feed-state-reconnecting").is_some());
        assert!(visual.debug_bounds("feed-retry").is_some());
        assert!(visual.debug_bounds("composer-disabled").is_some());
        assert_eq!(connect_count.get(), 1);

        let retry = visual.debug_bounds("feed-retry").expect("Retry action");
        visual.simulate_click(retry.center(), Modifiers::default());
        window
            .update(cx, |shell, _, cx| {
                shell.runtime.drain_at(0);
                cx.notify();
            })
            .expect("retry drain");
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.run_until_parked();
        assert_eq!(connect_count.get(), 2);
        let (feed_state, replay_state, can_submit) = window
            .update(cx, |shell, _, _| {
                (
                    shell.runtime.feed_state().cloned(),
                    shell.runtime.replay_state(),
                    shell.runtime.can_submit(),
                )
            })
            .expect("live state after retry");
        assert_eq!(feed_state, Some(FeedState::Live));
        assert_eq!(replay_state, crate::timeline::ReplayState::Live);
        assert!(can_submit);
        assert!(visual.debug_bounds("composer-enabled").is_some());
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("retained reconnecting draft"),
            draft
        );
    }

    #[gpui::test]
    fn fatal_feed_keeps_verified_prefix_and_error_card_in_every_layout(cx: &mut TestAppContext) {
        let (window, _) = open_state_shell(cx, 0, true, "fatal draft");
        window
            .update(cx, |shell, _, _| {
                let generation = shell.runtime.generation();
                shell.runtime.apply_feed_update(
                    generation,
                    FeedUpdate::Event {
                        generation,
                        event: Box::new(VerifiedSessionEvent::V2(
                            nopal_feed_client::session::DurableSessionEvent {
                                kind: nopal_feed_client::session::DURABLE_SESSION_EVENT_KIND
                                    .to_owned(),
                                event_id: "verified-ready".to_owned(),
                                plot_id: "plot-a".to_owned(),
                                session_id: "session-a".to_owned(),
                                stream_id: "stream-session-a".to_owned(),
                                sequence: 1,
                                previous_cursor: None,
                                cursor: "cursor-verified-1".to_owned(),
                                command_id: None,
                                event:
                                    nopal_feed_client::session::SessionEventPayload::SessionReady {
                                        extra: BTreeMap::new(),
                                    },
                                extra: BTreeMap::new(),
                            },
                        )),
                    },
                );
                shell.runtime.apply_feed_update(
                    generation,
                    FeedUpdate::State {
                        generation,
                        state: FeedState::Fatal {
                            code: "history_gap".to_owned(),
                            message: "verified history has a gap".to_owned(),
                        },
                    },
                );
            })
            .expect("fatal feed state");
        let mut visual = VisualTestContext::from_window(*window, cx);
        for width in [1440.0, 1024.0, 720.0] {
            visual.simulate_resize(size(px(width), px(800.0)));
            visual.run_until_parked();
            assert!(
                visual
                    .debug_bounds("timeline-event-verified-ready")
                    .is_some()
            );
            assert!(visual.debug_bounds("verified-prefix-error").is_some());
            assert!(visual.debug_bounds("composer-disabled").is_some());
        }
    }

    #[gpui::test]
    fn history_gap_keeps_the_verified_prefix_and_disables_submission(cx: &mut TestAppContext) {
        let (window, _) = open_state_shell(cx, 0, true, "gap draft");
        window
            .update(cx, |shell, _, cx| {
                let generation = shell.runtime.generation();
                for (event_id, sequence, previous_cursor, cursor) in [
                    ("verified-before-gap", 1, None, "cursor-before-gap"),
                    (
                        "rejected-after-gap",
                        3,
                        Some("cursor-before-gap"),
                        "cursor-after-gap",
                    ),
                ] {
                    if sequence == 3 {
                        shell.runtime.apply_feed_update(
                            generation,
                            FeedUpdate::State {
                                generation,
                                state: FeedState::Backoff {
                                    attempt: 1,
                                    retry_at_ms: 1,
                                    reason: "restore interrupted before the next event".to_owned(),
                                },
                            },
                        );
                    }
                    shell.runtime.apply_feed_update(
                        generation,
                        FeedUpdate::Event {
                            generation,
                            event: Box::new(VerifiedSessionEvent::V2(
                                nopal_feed_client::session::DurableSessionEvent {
                                kind: nopal_feed_client::session::DURABLE_SESSION_EVENT_KIND
                                    .to_owned(),
                                event_id: event_id.to_owned(),
                                plot_id: "plot-a".to_owned(),
                                session_id: "session-a".to_owned(),
                                stream_id: "stream-session-a".to_owned(),
                                sequence,
                                previous_cursor: previous_cursor.map(str::to_owned),
                                cursor: cursor.to_owned(),
                                command_id: None,
                                event:
                                    nopal_feed_client::session::SessionEventPayload::SessionReady {
                                        extra: BTreeMap::new(),
                                    },
                                    extra: BTreeMap::new(),
                                },
                            )),
                        },
                    );
                }
                cx.notify();
            })
            .expect("gap feed state");
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_resize(size(px(1024.0), px(800.0)));
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("timeline-event-verified-before-gap")
                .is_some()
        );
        assert!(
            visual
                .debug_bounds("timeline-event-rejected-after-gap")
                .is_none()
        );
        assert!(visual.debug_bounds("verified-prefix-error").is_some());
        assert!(visual.debug_bounds("composer-disabled").is_some());
    }

    #[gpui::test]
    fn composer_clears_only_after_a_successful_structured_send(cx: &mut TestAppContext) {
        let window = open_shell(cx, true);
        window
            .update(cx, |shell, _, _| {
                shell.composer.replace(None, "  ship this  ");
                shell.submit_composer();
                assert_eq!(shell.composer.text(), "");
                assert!(shell.diagnostic.is_none());
            })
            .expect("successful composer submission");
    }

    #[gpui::test]
    fn composer_restores_exact_text_and_diagnostic_after_a_failed_send(cx: &mut TestAppContext) {
        let window = open_shell(cx, false);
        window
            .update(cx, |shell, _, _| {
                shell.composer.replace(None, "  keep this exact  ");
                shell.submit_composer();
                assert_eq!(shell.composer.text(), "  keep this exact  ");
                assert_eq!(
                    shell.diagnostic.as_deref(),
                    Some("structured test connection is unavailable")
                );
            })
            .expect("failed composer submission");
    }

    #[gpui::test]
    fn plot_and_activity_clicks_atomically_retarget_the_runtime(cx: &mut TestAppContext) {
        let window = open_selection_shell(cx);
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_resize(size(px(1280.0), px(800.0)));
        visual.run_until_parked();

        let plot_b = visual.debug_bounds("plot-row-plot-b").expect("Plot B row");
        visual.simulate_click(plot_b.center(), Modifiers::default());
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.runtime.selected_session_context())
                .expect("selected context")
                .expect("selected Session")
                .session_id,
            "session-b"
        );

        let plot_a = visual.debug_bounds("plot-row-plot-a").expect("Plot A row");
        visual.simulate_click(plot_a.center(), Modifiers::default());
        let execution = visual
            .debug_bounds("activity-execution-rondo-repo-a-run-a")
            .expect("execution tab");
        visual.simulate_click(execution.center(), Modifiers::default());
        assert!(
            window
                .update(cx, |shell, _, _| shell
                    .runtime
                    .selected_session_context()
                    .is_none())
                .expect("execution selection")
        );
    }

    #[gpui::test]
    fn plot_retarget_restores_each_sessions_exact_visible_composer_draft(cx: &mut TestAppContext) {
        let window = open_selection_shell(cx);
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_resize(size(px(1280.0), px(800.0)));
        visual.run_until_parked();
        let draft_a = "  Plot A\nkeeps spacing 🌵  ";
        let draft_b = "Plot B\nsecond line";
        window
            .update(cx, |shell, _, _| shell.composer.replace(None, draft_a))
            .expect("type Plot A draft");

        let plot_b = visual.debug_bounds("plot-row-plot-b").expect("Plot B row");
        visual.simulate_click(plot_b.center(), Modifiers::default());
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("Plot B initial draft"),
            ""
        );
        window
            .update(cx, |shell, _, _| shell.composer.replace(None, draft_b))
            .expect("type Plot B draft");

        let plot_a = visual.debug_bounds("plot-row-plot-a").expect("Plot A row");
        visual.simulate_click(plot_a.center(), Modifiers::default());
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("restored Plot A draft"),
            draft_a
        );

        let plot_b = visual.debug_bounds("plot-row-plot-b").expect("Plot B row");
        visual.simulate_click(plot_b.center(), Modifiers::default());
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("restored Plot B draft"),
            draft_b
        );
    }

    #[gpui::test]
    fn presentation_toggle_preserves_runtime_identity(cx: &mut TestAppContext) {
        let window = open_selection_shell(cx);
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_resize(size(px(1280.0), px(800.0)));
        visual.run_until_parked();
        let before = window
            .update(cx, |shell, _, _| {
                (
                    shell.runtime.generation(),
                    shell.runtime.selected_session_context(),
                )
            })
            .expect("runtime identity");

        let terminal = visual
            .debug_bounds("presentation-terminal")
            .expect("Terminal button");
        visual.simulate_click(terminal.center(), Modifiers::default());
        let after = window
            .update(cx, |shell, _, _| {
                (
                    shell.runtime.generation(),
                    shell.runtime.selected_session_context(),
                    shell.runtime.presentation(),
                )
            })
            .expect("runtime identity after toggle");
        assert_eq!((after.0, after.1), before);
        assert_eq!(after.2, RuntimePresentation::Terminal);
    }

    #[gpui::test]
    fn narrow_plot_drawer_recovers_navigation_and_closes_after_selection(cx: &mut TestAppContext) {
        let window = open_selection_shell(cx);
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_resize(size(px(720.0), px(800.0)));
        visual.run_until_parked();
        assert!(
            window
                .update(cx, |shell, _, _| !shell.narrow_rail_open)
                .expect("drawer state")
        );

        let plots = visual
            .debug_bounds("narrow-plots-button")
            .expect("Plots button");
        visual.simulate_click(plots.center(), Modifiers::default());
        assert!(visual.debug_bounds("narrow-plot-drawer").is_some());
        let plot_b = visual
            .debug_bounds("plot-row-plot-b")
            .expect("Plot B drawer row");
        visual.simulate_click(plot_b.center(), Modifiers::default());

        assert!(
            window
                .update(cx, |shell, _, _| !shell.narrow_rail_open)
                .expect("closed drawer state")
        );
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell
                    .runtime
                    .field()
                    .selected_plot_id
                    .clone())
                .expect("selected Plot"),
            Some("plot-b".to_owned())
        );
    }

    #[gpui::test]
    fn multiline_composer_keeps_the_cursor_visible_and_maps_pointer_to_text(
        cx: &mut TestAppContext,
    ) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let terminal_focus = cx.focus_handle();
                let composer_focus = cx.focus_handle();
                composer_focus.focus(window);
                cx.new(|_| {
                    let mut shell = DesktopShell::new(
                        live_test_runtime(selection_field(), RecordingTransport::default()),
                        None,
                        terminal_focus,
                        composer_focus,
                        None,
                    );
                    shell.composer.replace(None, "zero\none\ntwo\nthree\nfour");
                    shell
                })
            })
            .expect("test window")
        });
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.run_until_parked();
        let live_state = window
            .update(cx, |shell, _, _| {
                (
                    shell.runtime.feed_state().cloned(),
                    shell.runtime.replay_state(),
                    shell.runtime.can_submit(),
                )
            })
            .expect("live composer state");
        assert!(live_state.2, "composer state is {live_state:?}");
        let (position, expected_cursor, ranges) = window
            .update(cx, |shell, _, _| {
                let line = shell.composer_lines.first().expect("visible line");
                (
                    point(
                        line.bounds.left() + line.line.x_for_index(1),
                        line.bounds.top() + px(5.0),
                    ),
                    line.text_range.start + 1,
                    shell
                        .composer_lines
                        .iter()
                        .map(|line| line.text_range.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .expect("read composer layout");
        assert_eq!(ranges, [9..12, 13..18, 19..23]);

        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.cursor())
                .expect("read clicked cursor"),
            expected_cursor
        );
    }

    #[gpui::test]
    fn rendered_composer_backspace_edits_locally_without_writing_to_tmux(cx: &mut TestAppContext) {
        let transport = RecordingTransport::default();
        let writes = transport.0.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let terminal_focus = cx.focus_handle();
                let composer_focus = cx.focus_handle();
                composer_focus.focus(window);
                cx.new(|_| {
                    DesktopShell::new(
                        live_test_runtime(selection_field(), transport),
                        None,
                        terminal_focus,
                        composer_focus,
                        None,
                    )
                })
            })
            .expect("test window")
        });

        window
            .update(cx, |shell, window, cx| {
                shell.replace_text_in_range(None, "hello", window, cx);
            })
            .expect("type composer text");
        cx.dispatch_keystroke(*window, Keystroke::parse("backspace").expect("key"));

        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("read composer"),
            "hell"
        );
        cx.dispatch_keystroke(*window, Keystroke::parse("left").expect("key"));
        cx.dispatch_keystroke(*window, Keystroke::parse("shift-left").expect("key"));
        cx.dispatch_keystroke(*window, Keystroke::parse("cmd-x").expect("key"));
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("read cut composer"),
            "hel"
        );
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("l".to_owned())
        );
        cx.dispatch_keystroke(*window, Keystroke::parse("cmd-z").expect("key"));
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("read undo composer"),
            "hell"
        );
        cx.dispatch_keystroke(*window, Keystroke::parse("cmd-shift-z").expect("key"));
        assert_eq!(
            window
                .update(cx, |shell, _, _| shell.composer.text().to_owned())
                .expect("read redo composer"),
            "hel"
        );
        assert!(writes.borrow().is_empty());
    }

    #[gpui::test]
    fn focused_terminal_dispatches_printable_input_through_the_rendered_shell(
        cx: &mut TestAppContext,
    ) {
        let transport = RecordingTransport::default();
        let writes = transport.0.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let focus = cx.focus_handle();
                let composer_focus = cx.focus_handle();
                cx.new(|_| {
                    let mut shell = DesktopShell::new(
                        test_runtime(DesktopField::demo("%77".to_owned()), transport),
                        None,
                        focus,
                        composer_focus,
                        None,
                    );
                    shell
                        .terminal_controller_mut()
                        .expect("terminal")
                        .apply_output(b"copy me\x1b[?2004h");
                    shell
                        .runtime
                        .set_presentation(RuntimePresentation::Terminal);
                    shell
                })
            })
            .expect("test window")
        });
        let mut visual = VisualTestContext::from_window(*window, cx);
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(400.0), px(300.0)),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert!(
            window
                .update(cx, |shell, window, _| shell
                    .terminal_focus
                    .is_focused(window))
                .expect("read terminal focus")
        );

        cx.dispatch_keystroke(*window, Keystroke::parse("a").expect("key"));
        cx.write_to_clipboard(ClipboardItem::new_string("paste\nvalue".to_owned()));
        cx.dispatch_action(*window, super::PasteTerminal);
        window
            .update(cx, |shell, window, cx| {
                shell.replace_and_mark_text_in_range(None, "に", None, window, cx);
                shell.unmark_text(window, cx);
                let controller = shell.terminal_controller_mut().expect("terminal");
                controller.begin_selection((0, 0));
                controller.update_selection((0, 3));
            })
            .expect("commit composed text");
        cx.dispatch_action(*window, super::CopyTerminal);

        assert!(writes.borrow().iter().any(|bytes| bytes == b"a"));
        assert!(
            writes
                .borrow()
                .iter()
                .any(|bytes| bytes == b"\x1b[200~paste\nvalue\x1b[201~")
        );
        assert!(writes.borrow().iter().any(|bytes| bytes == "に".as_bytes()));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("copy".to_owned())
        );
    }
}
