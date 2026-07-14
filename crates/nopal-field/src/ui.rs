//! Sidebar rendering: SEATS above AFK RUNS, pinned ASKS bar, filter, spawn
//! picker, and worktree-name prompts, and the structured run detail view.
//! Plus the embedded seat panel (Feature 3) and the help overlay. Pure
//! over [`App`].

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as VtColor, CursorShape, NamedColor};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::embed::Embed;
use crate::keys::{KeyAction, KeyRegistry};
use crate::seat::CandidateKind;
use crate::state::{
    App, DropZone, Mode, PlotActivityKey, PlotInspectorTab, Row, RowDrag, Section, SourceStatus,
};

const PLOT_RAIL_COLUMNS: u16 = 28;
const INSPECTOR_COLUMNS: u16 = 34;
const MEDIUM_PLOT_RAIL_COLUMNS: u16 = 24;
const NARROW_PLOT_RAIL_COLUMNS: u16 = 18;
const WIDE_STAGE_MIN_WIDTH: u16 = 134;
const MEDIUM_STAGE_MIN_WIDTH: u16 = 90;

const HEADER_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().add_modifier(Modifier::DIM);
const SELECTED: Style = Style::new()
    .bg(Color::Indexed(237))
    .add_modifier(Modifier::BOLD);
const ASK_STYLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
/// Scrollback-search highlight for every visible match
/// except the current one, which gets `REVERSED + BOLD` directly at the
/// call site instead - a visibly stronger treatment so the operator can
/// always tell which hit `n`/`N` will move from. A background distinct
/// from [`SELECTED`]'s own index; the two never appear on the same cell in
/// practice (mouse selection and keyboard search are separate gestures).
const SEARCH_MATCH: Style = Style::new().bg(Color::Indexed(58));

/// Geometry of the last rendered frame, for mouse hit-testing. [`draw`]
/// rebuilds it every render; the input layer resolves click coordinates
/// against it instead of re-deriving layout math.
#[derive(Debug, Clone, Default)]
pub struct HitMap {
    /// Each visible sidebar row's full-width line and identity.
    pub rows: Vec<(Rect, Row)>,
    /// The embedded seat's grid area, when an embed is on screen.
    pub embed_grid: Option<Rect>,
    /// The help popup, when the overlay is up.
    pub help: Option<Rect>,
    /// The embedded panel's full rect (header line plus grid), when an
    /// embed is on screen - the drop surface for a row-drag. Narrower than
    /// [`Self::embed_grid`] deliberately would be wrong:
    /// design decision 2 makes the whole panel the drop surface, not just
    /// its grid sub-area, so a drop on the header line still resolves.
    pub panel: Option<Rect>,
    /// Session and execution tabs in exact visual order and geometry.
    pub activity_tabs: Vec<(Rect, PlotActivityKey)>,
    /// Plot inspector tabs in exact visual order and geometry.
    pub inspector_tabs: Vec<(Rect, PlotInspectorTab)>,
    /// Scrollable dominant activity content region.
    pub main: Option<Rect>,
    /// Scrollable Plot-scoped inspector region.
    pub inspector: Option<Rect>,
}

impl HitMap {
    /// The sidebar row under a cell position, if any.
    pub fn row_at(&self, x: u16, y: u16) -> Option<&Row> {
        let position = Position::new(x, y);
        self.rows
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, row)| row)
    }

    /// Whether a cell position falls inside the embedded seat's grid.
    pub fn in_embed_grid(&self, x: u16, y: u16) -> bool {
        self.embed_grid
            .is_some_and(|rect| rect.contains(Position::new(x, y)))
    }

    pub fn activity_at(&self, x: u16, y: u16) -> Option<&PlotActivityKey> {
        let position = Position::new(x, y);
        self.activity_tabs
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, activity)| activity)
    }

    pub fn inspector_tab_at(&self, x: u16, y: u16) -> Option<PlotInspectorTab> {
        let position = Position::new(x, y);
        self.inspector_tabs
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, tab)| *tab)
    }

    pub fn in_main(&self, x: u16, y: u16) -> bool {
        self.main
            .is_some_and(|rect| rect.contains(Position::new(x, y)))
    }

    pub fn in_inspector(&self, x: u16, y: u16) -> bool {
        self.inspector
            .is_some_and(|rect| rect.contains(Position::new(x, y)))
    }
}

/// Edge-band width divisor for row-drag drop zones: each edge band is
/// `1/EDGE_BAND_DIVISOR` of the panel's matching dimension (~20%, the
/// design doc's number); the remaining middle is the center "open" zone.
const EDGE_BAND_DIVISOR: u16 = 5;

/// Resolve which of a row-drag's five drop zones a screen cell falls into
/// within the embedded panel's rect: each edge's outer
/// `1/EDGE_BAND_DIVISOR` band is that edge's split zone, everything else
/// inside the panel is [`DropZone::Center`] ("open"). A corner cell
/// resolves to whichever edge it sits proportionally closest to - distance
/// to the edge as a fraction of that edge's own band width - so the four
/// bands partition the rect with no ambiguous overlap; an exact tie (a
/// literal corner cell, equally close by that ratio) prefers the
/// horizontal edge (left/right) over the vertical one. `None` when the
/// cell falls outside `panel` entirely - not a drop on this panel at all.
/// Pure - unit tested - the same rect+point-to-zone shape as
/// [`crate::embed::screen_to_local`].
pub fn drop_zone_at(panel: Rect, x: u16, y: u16) -> Option<DropZone> {
    if !panel.contains(Position::new(x, y)) {
        return None;
    }
    let w = panel.width.max(1);
    let h = panel.height.max(1);
    let dx = x - panel.x;
    let dy = y - panel.y;
    let band_x = (w / EDGE_BAND_DIVISOR).max(1);
    let band_y = (h / EDGE_BAND_DIVISOR).max(1);
    let candidates = [
        (dx, band_x, DropZone::Left),
        ((w - 1).saturating_sub(dx), band_x, DropZone::Right),
        (dy, band_y, DropZone::Top),
        ((h - 1).saturating_sub(dy), band_y, DropZone::Bottom),
    ];
    let nearest = candidates
        .into_iter()
        .filter(|(dist, band, _)| dist < band)
        .min_by_key(|(dist, band, _)| (u32::from(*dist) * 1000) / u32::from(*band));
    Some(nearest.map_or(DropZone::Center, |(_, _, zone)| zone))
}

/// The five drop-zone rects within `panel`, in the same band proportions
/// [`drop_zone_at`] hit-tests against - one shared band-width calculation
/// so the overlay a row-drag renders always matches what a drop there
/// actually resolves to.
fn drop_zone_rects(panel: Rect) -> [(DropZone, Rect); 5] {
    let w = panel.width.max(1);
    let h = panel.height.max(1);
    let band_x = (w / EDGE_BAND_DIVISOR).max(1).min(w);
    let band_y = (h / EDGE_BAND_DIVISOR).max(1).min(h);
    let left = Rect {
        width: band_x,
        ..panel
    };
    let right = Rect {
        x: panel.x + w - band_x,
        width: band_x,
        ..panel
    };
    let top = Rect {
        height: band_y,
        ..panel
    };
    let bottom = Rect {
        y: panel.y + h - band_y,
        height: band_y,
        ..panel
    };
    let center = Rect {
        x: panel.x + band_x,
        y: panel.y + band_y,
        width: w.saturating_sub(band_x * 2),
        height: h.saturating_sub(band_y * 2),
    };
    [
        (DropZone::Left, left),
        (DropZone::Right, right),
        (DropZone::Top, top),
        (DropZone::Bottom, bottom),
        (DropZone::Center, center),
    ]
}

/// Row-drag drop-zone overlay: while a sidebar seat row is being dragged
/// over an open embedded panel, paint the four edge bands
/// plus the center "open" zone so the operator can see where a drop
/// resolves, brightening whichever one the pointer currently hovers.
/// Reuses the help/context-menu popup's dim-fill convention rather than
/// inventing new styling.
fn draw_drop_zones(frame: &mut Frame<'_>, panel: Rect, hover: Option<DropZone>) {
    for (zone, rect) in drop_zone_rects(panel) {
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let style = if hover == Some(zone) {
            Style::new()
                .bg(Color::Indexed(24))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(Color::Indexed(235)).patch(DIM)
        };
        frame.render_widget(ratatui::widgets::Block::default().style(style), rect);
        let label_y = rect.y + rect.height / 2;
        if label_y < rect.bottom() {
            let label_rect = Rect {
                y: label_y,
                height: 1,
                ..rect
            };
            frame.render_widget(
                Paragraph::new(Line::styled(zone.label(), style))
                    .alignment(ratatui::layout::Alignment::Center),
                label_rect,
            );
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let mut hit = HitMap::default();
    // Base layer.
    if app.stage_open {
        let (plot_width, inspector_width) = plot_first_widths(area.width, app.inspector_collapsed);
        let plot_rail = Rect {
            width: plot_width,
            ..area
        };
        let main = Rect {
            x: area.x + plot_width,
            width: area
                .width
                .saturating_sub(plot_width)
                .saturating_sub(inspector_width),
            ..area
        };
        let inspector = Rect {
            x: main.x + main.width,
            width: inspector_width,
            ..area
        };
        draw_plot_rail(frame, app, plot_rail, &mut hit);
        let tabs_height = main.height.min(3);
        let tabs = Rect {
            height: tabs_height,
            ..main
        };
        let panel = Rect {
            y: main.y + tabs_height,
            height: main.height.saturating_sub(tabs_height),
            ..main
        };
        draw_activity_tabs(frame, app, tabs, &mut hit);
        hit.main = Some(panel);
        if inspector_width > 0 {
            draw_plot_inspector(frame, app, inspector, &mut hit);
        }
        draw_selected_activity(frame, app, panel, &mut hit);
        // A row-drag's drop-zone overlay only ever has
        // something to paint once it has been promoted past `Armed` (see
        // `RowDrag::advance`) - a plain press-in-flight has no hover to
        // show yet, and showing the overlay before any Drag event fired
        // would flash it under an ordinary click.
        if hit.panel.is_some()
            && let Some(RowDrag::Dragging { hover, .. }) = &app.row_drag
        {
            draw_drop_zones(frame, panel, *hover);
        }
    } else if let Some(embed) = &app.embed {
        // Plot-unbound legacy seats predate the durable stage model. Keep
        // their live terminal visible while bound seats route through the
        // Plot activity stage above.
        draw_embed(frame, embed, area, &mut hit, &app.keys);
        hit.panel = Some(area);
    } else if let Mode::RunDetail(key) = &app.mode {
        draw_run_detail(frame, app, key, area);
    } else {
        draw_sidebar(frame, app, area, &mut hit);
    }
    // Overlay layer.
    if app.mode == Mode::Help {
        draw_help(frame, &app.keys, area, &mut hit);
    }
    if let Mode::ContextMenu { row_key, cursor } = &app.mode {
        draw_context_menu(frame, app, area, row_key, *cursor);
    }
    app.hit = hit;
}

/// Plot rail and inspector widths. The dominant activity receives every
/// remaining column, and narrow layouts hide the inspector first.
fn plot_first_widths(area_width: u16, inspector_collapsed: bool) -> (u16, u16) {
    if area_width == 0 {
        return (0, 0);
    }
    let target = if area_width >= WIDE_STAGE_MIN_WIDTH {
        PLOT_RAIL_COLUMNS
    } else if area_width >= MEDIUM_STAGE_MIN_WIDTH {
        MEDIUM_PLOT_RAIL_COLUMNS
    } else {
        NARROW_PLOT_RAIL_COLUMNS
    };
    let plots = target.min(area_width.saturating_sub(1));
    let inspector = if inspector_collapsed || area_width < WIDE_STAGE_MIN_WIDTH {
        0
    } else {
        INSPECTOR_COLUMNS.min(area_width.saturating_sub(plots))
    };
    (plots, inspector)
}

fn draw_plot_rail(frame: &mut Frame<'_>, app: &App, area: Rect, hit: &mut HitMap) {
    let block = Block::default()
        .title(format!(" Plots {} ", app.plots.len()))
        .borders(Borders::RIGHT);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    for plot in app.plots.values() {
        let selected = app.selected_plot_id.as_deref() == Some(plot.plot_id.as_str());
        let marker = if selected { "●" } else { "○" };
        let condition = if plot.conditions.is_empty() {
            "No Conditions".to_owned()
        } else {
            plot.conditions.join(", ")
        };
        let style = if selected { SELECTED } else { Style::new() };
        let offset = lines.len() as u16;
        lines.push(Line::styled(
            format!(" {marker} {}", short_plot_id(&plot.plot_id)),
            style,
        ));
        lines.push(Line::styled(format!("   {}", plot.title), style));
        lines.push(Line::styled(
            format!("   {} · {condition}", title_case(&plot.progress)),
            style.patch(DIM),
        ));
        lines.push(Line::raw(""));
        if offset < inner.height {
            hit.rows.push((
                Rect {
                    y: inner.y + offset,
                    height: 3.min(inner.height - offset),
                    ..inner
                },
                Row {
                    section: Section::Plots,
                    key: plot.plot_id.clone(),
                },
            ));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled(" No Plots yet", DIM));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_activity_tabs(frame: &mut Frame<'_>, app: &App, area: Rect, hit: &mut HitMap) {
    let Some(plot) = app.selected_plot() else {
        frame.render_widget(Paragraph::new(Line::styled(" No Plot selected", DIM)), area);
        return;
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} ", short_plot_id(&plot.plot_id)), DIM),
        Span::styled(
            plot.title.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", title_case(&plot.progress)), DIM),
    ])];
    let mut tabs = Vec::new();
    let mut x = area.x;
    for session in &plot.sessions {
        let key = PlotActivityKey::Session(session.session_id.clone());
        let label = format!(" Session {} ", short_plot_id(&session.session_id));
        let width = label.chars().count().min(u16::MAX as usize) as u16;
        let visible = width.min(area.right().saturating_sub(x));
        if visible > 0 {
            hit.activity_tabs
                .push((Rect::new(x, area.y + 1, visible, 1), key.clone()));
        }
        tabs.push(Span::styled(
            label,
            if app.selected_plot_activity.as_ref() == Some(&key) {
                SELECTED
            } else {
                DIM
            },
        ));
        x = x.saturating_add(width);
    }
    for execution in &plot.executions {
        let key = PlotActivityKey::Execution {
            service_id: execution.service_id.clone(),
            repo_id: execution.repo_id.clone(),
            run_id: execution.run_id.clone(),
        };
        let label = format!(" Run {} ", short_plot_id(&execution.run_id));
        let width = label.chars().count().min(u16::MAX as usize) as u16;
        let visible = width.min(area.right().saturating_sub(x));
        if visible > 0 {
            hit.activity_tabs
                .push((Rect::new(x, area.y + 1, visible, 1), key.clone()));
        }
        tabs.push(Span::styled(
            label,
            if app.selected_plot_activity.as_ref() == Some(&key) {
                SELECTED
            } else {
                DIM
            },
        ));
        x = x.saturating_add(width);
    }
    if tabs.is_empty() {
        tabs.push(Span::styled(" No activity ", DIM));
    }
    lines.push(Line::from(tabs));
    lines.push(Line::styled("─".repeat(area.width as usize), DIM));
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_selected_activity(frame: &mut Frame<'_>, app: &App, area: Rect, hit: &mut HitMap) {
    let Some(plot) = app.selected_plot() else {
        draw_scrolled_lines(
            frame,
            vec![Line::styled(" No Plot selected", DIM)],
            area,
            app.main_scroll,
        );
        return;
    };
    match app.selected_plot_activity.as_ref() {
        Some(PlotActivityKey::Session(session_id)) => {
            let Some(session) = plot
                .sessions
                .iter()
                .find(|session| &session.session_id == session_id)
            else {
                draw_scrolled_lines(
                    frame,
                    vec![
                        Line::styled(" Session unavailable", HEADER_STYLE),
                        Line::raw(""),
                        Line::raw(" The selected Session is no longer present in this Plot."),
                    ],
                    area,
                    app.main_scroll,
                );
                return;
            };
            let live_embed = app.embed.as_ref().filter(|embed| {
                session.host_pane.as_deref() == Some(embed.pane_id.as_str())
                    || app.seats.values().any(|seat| {
                        seat.pane_id == embed.pane_id
                            && seat.plot_session_id.as_deref() == Some(session.session_id.as_str())
                    })
            });
            if let Some(embed) = live_embed {
                draw_embed(frame, embed, area, hit, &app.keys);
                hit.panel = Some(area);
            } else {
                let mut lines = vec![
                    Line::styled(" Session unavailable", HEADER_STYLE),
                    Line::raw(""),
                    Line::raw(" No live tmux pane is attached to this Session."),
                    Line::raw(""),
                    Line::styled(" SESSION", DIM),
                    Line::raw(format!(" {}", session.session_id)),
                    Line::styled(" STATE", DIM),
                    Line::raw(format!(" {}", session.state)),
                    Line::styled(" HOST", DIM),
                    Line::raw(format!(" {} / {}", session.host, session.host_session)),
                    Line::styled(" PANE", DIM),
                    Line::raw(format!(
                        " {}",
                        session.host_pane.as_deref().unwrap_or("Not assigned")
                    )),
                ];
                if let Some(workspace) = &session.workspace {
                    lines.extend([
                        Line::styled(" WORKSPACE", DIM),
                        Line::raw(format!(" {workspace}")),
                    ]);
                }
                draw_scrolled_lines(frame, lines, area, app.main_scroll);
            }
        }
        Some(PlotActivityKey::Execution {
            service_id,
            repo_id,
            run_id,
        }) => {
            let Some(execution) = plot.executions.iter().find(|execution| {
                &execution.service_id == service_id
                    && &execution.repo_id == repo_id
                    && &execution.run_id == run_id
            }) else {
                draw_scrolled_lines(
                    frame,
                    vec![Line::styled(" Execution unavailable", HEADER_STYLE)],
                    area,
                    app.main_scroll,
                );
                return;
            };
            let outcome = execution.outcome.as_deref().unwrap_or("Not terminal");
            let digest = if execution.manifest_sha256.is_empty() {
                "Not reported".to_owned()
            } else {
                execution.manifest_sha256.clone()
            };
            let lines = vec![
                Line::styled(format!(" Run {}", execution.run_id), HEADER_STYLE),
                Line::styled(" Durable Plot execution · read-only", DIM),
                Line::raw(""),
                Line::styled(" PROVENANCE", DIM),
                Line::raw(format!(
                    " {} / {} / {}",
                    execution.service_id, execution.repo_id, execution.run_id
                )),
                Line::raw(""),
                Line::styled(" STATUS", DIM),
                Line::raw(format!(" {}", execution.status)),
                Line::styled(" OUTCOME", DIM),
                Line::raw(format!(" {outcome}")),
                Line::styled(" EVENT CURSOR", DIM),
                Line::raw(format!(" {}", execution.event_cursor)),
                Line::styled(" MANIFEST SHA-256", DIM),
                Line::raw(format!(" {digest}")),
                Line::styled(" CREATED", DIM),
                Line::raw(format!(" {}", value_or_unknown(&execution.created_at))),
                Line::styled(" UPDATED", DIM),
                Line::raw(format!(" {}", value_or_unknown(&execution.updated_at))),
            ];
            draw_scrolled_lines(frame, lines, area, app.main_scroll);
        }
        None => draw_scrolled_lines(
            frame,
            vec![
                Line::styled(" No Plot activity", HEADER_STYLE),
                Line::raw(""),
                Line::raw(" This Plot has no Sessions or executions yet."),
            ],
            area,
            app.main_scroll,
        ),
    }
}

fn draw_scrolled_lines(
    frame: &mut Frame<'_>,
    lines: Vec<Line<'static>>,
    area: Rect,
    scroll: usize,
) {
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn value_or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "Not reported"
    } else {
        value
    }
}

fn draw_plot_inspector(frame: &mut Frame<'_>, app: &App, area: Rect, hit: &mut HitMap) {
    let block = Block::default()
        .title(" Plot inspector ")
        .borders(Borders::LEFT);
    hit.inspector = Some(area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(plot) = app.selected_plot() else {
        frame.render_widget(
            Paragraph::new(Line::styled(" No Plot selected", DIM)),
            inner,
        );
        return;
    };
    let tab_area = Rect { height: 1, ..inner };
    let content = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let mut x = tab_area.x;
    let mut spans = Vec::new();
    for (tab, name) in [
        (PlotInspectorTab::Overview, "Overview"),
        (PlotInspectorTab::Roots, "Roots"),
        (PlotInspectorTab::Evidence, "Evidence"),
        (PlotInspectorTab::Fruit, "Fruit"),
    ] {
        let label = format!(" {name} ");
        let width = label.len() as u16;
        let visible = width.min(tab_area.right().saturating_sub(x));
        if visible > 0 {
            hit.inspector_tabs
                .push((Rect::new(x, tab_area.y, visible, 1), tab));
        }
        spans.push(Span::styled(
            label,
            if app.inspector_tab == tab {
                SELECTED
            } else {
                DIM
            },
        ));
        x = x.saturating_add(width);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), tab_area);
    frame.render_widget(
        Paragraph::new(inspector_lines(app.inspector_tab, plot))
            .wrap(Wrap { trim: false })
            .scroll((app.inspector_scroll.min(u16::MAX as usize) as u16, 0)),
        content,
    );
}

fn inspector_lines(tab: PlotInspectorTab, plot: &crate::state::Plot) -> Vec<Line<'static>> {
    match tab {
        PlotInspectorTab::Overview => {
            let conditions = if plot.conditions.is_empty() {
                "None".to_owned()
            } else {
                plot.conditions.join(", ")
            };
            let seed = if plot.seed_text.is_empty() {
                format!("No text yet ({})", plot.seed_source)
            } else {
                plot.seed_text.clone()
            };
            let mut lines = vec![
                Line::styled(format!(" {}", short_plot_id(&plot.plot_id)), HEADER_STYLE),
                Line::styled(" PROGRESS", DIM),
                Line::raw(format!(" {}", title_case(&plot.progress))),
                Line::styled(" CONDITIONS", DIM),
                Line::raw(format!(" {conditions}")),
                Line::styled(" SEED", DIM),
                Line::raw(format!(" {seed}")),
                Line::styled(" INTENT", DIM),
                Line::raw(format!(" {}", value_or_unknown(&plot.intent))),
            ];
            if let Some(establishment) = &plot.establishment {
                lines.extend([
                    Line::styled(" ESTABLISHMENT", DIM),
                    Line::raw(format!(" {}", establishment.event)),
                    Line::raw(format!(
                        " workflow {} / {}",
                        short_plot_id(&establishment.workflow_source_repository_id),
                        &establishment.workflow_source_hash
                            [..establishment.workflow_source_hash.len().min(12)]
                    )),
                ]);
            }
            if !plot.workspaces.is_empty() {
                lines.push(Line::styled(" WORKSPACES", DIM));
                for workspace in &plot.workspaces {
                    lines.push(Line::raw(format!(
                        " {} / {} / {}",
                        workspace.kind, workspace.repository_id, workspace.root
                    )));
                }
            }
            lines
        }
        PlotInspectorTab::Roots => {
            let mut lines = vec![Line::styled(" Roots and Proof Requirements", HEADER_STYLE)];
            for repository in &plot.repositories {
                lines.extend([
                    Line::raw(""),
                    Line::styled(format!(" {}", repository.repository_id), DIM),
                    Line::raw(format!(" root {}", repository.root)),
                    Line::raw(format!(" configuration {}", repository.configuration_root)),
                ]);
                for root in &repository.roots {
                    lines.extend([
                        Line::styled(format!(" ROOT {}", root.id), HEADER_STYLE),
                        Line::raw(format!(" {}", root.statement)),
                    ]);
                    for proof in &root.proof_requirements {
                        lines.extend([
                            Line::styled(format!(" PROOF REQUIREMENT {}", proof.id), DIM),
                            Line::raw(format!(" stage {}", proof.stage)),
                            Line::raw(format!(" required {}", proof.required)),
                            Line::raw(format!(" gates {}", proof.gates.join(", "))),
                            Line::raw(format!(" on missing {}", proof.on_missing)),
                            Line::raw(format!(" on failure {}", proof.on_failure)),
                        ]);
                    }
                }
            }
            if plot
                .repositories
                .iter()
                .all(|repository| repository.roots.is_empty())
            {
                lines.push(Line::styled(" No Roots declared", DIM));
            }
            lines
        }
        PlotInspectorTab::Evidence => {
            let mut lines = vec![Line::styled(" Plot Evidence", HEADER_STYLE)];
            let mut count = 0;
            for execution in &plot.executions {
                for evidence in &execution.evidence {
                    count += 1;
                    lines.extend([
                        Line::raw(""),
                        Line::styled(format!(" {}", evidence.artifact_kind), DIM),
                        Line::raw(format!(
                            " {} / {} / {}",
                            execution.service_id, execution.repo_id, execution.run_id
                        )),
                        Line::raw(format!(" {}", evidence.uri)),
                    ]);
                }
            }
            if count == 0 {
                lines.push(Line::styled(" No Evidence reported", DIM));
            }
            lines
        }
        PlotInspectorTab::Fruit => vec![
            Line::styled(" Fruit", HEADER_STYLE),
            Line::raw(""),
            if plot.fruit_state == "absent" {
                Line::raw(" Fruit is absent.")
            } else {
                Line::raw(format!(" State: {}", plot.fruit_state))
            },
            Line::raw(""),
            Line::styled(" Completion does not infer or mutate Fruit.", DIM),
        ],
    }
}

fn short_plot_id(plot_id: &str) -> String {
    if plot_id.chars().count() <= 16 {
        plot_id.to_owned()
    } else {
        format!("{}…", plot_id.chars().take(15).collect::<String>())
    }
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn draw_sidebar(frame: &mut Frame<'_>, app: &App, area: Rect, hit: &mut HitMap) {
    let asks: Vec<_> = app.pending_asks().collect();
    let ask_rows = asks.len().min(3) as u16 + u16::from(!asks.is_empty());
    let body_height = area.height.saturating_sub(1 + ask_rows + 1); // title, asks bar, status line

    let title = Rect { height: 1, ..area };
    let body = Rect {
        y: area.y + 1,
        height: body_height,
        ..area
    };
    let asks_bar = Rect {
        y: body.y + body.height,
        height: ask_rows,
        ..area
    };
    let status = Rect {
        y: asks_bar.y + asks_bar.height,
        height: 1,
        ..area
    };

    frame.render_widget(title_line(app), title);
    let (body_lines, body_hits) = body_paragraph(app, body.height as usize);
    frame.render_widget(body_lines, body);
    for (offset, row) in body_hits {
        if offset < body.height {
            hit.rows.push((
                Rect {
                    y: body.y + offset,
                    height: 1,
                    ..body
                },
                row,
            ));
        }
    }
    if !asks.is_empty() {
        frame.render_widget(asks_paragraph(app), asks_bar);
        // The bar's first line is its header; asks follow one per line.
        for (index, ask) in asks.iter().take(3).enumerate() {
            hit.rows.push((
                Rect {
                    y: asks_bar.y + 1 + index as u16,
                    height: 1,
                    ..asks_bar
                },
                Row {
                    section: Section::Asks,
                    key: ask.ask_id.clone(),
                },
            ));
        }
    }
    frame.render_widget(status_line(app), status);
}

fn title_line(app: &App) -> Paragraph<'static> {
    let mut spans = vec![Span::styled(
        format!(" nopal field [{}] ", app.session_name),
        HEADER_STYLE,
    )];
    for (name, status) in &app.sources {
        let (glyph, style) = match status {
            SourceStatus::Ok => ("+", Style::new().fg(Color::Green)),
            SourceStatus::Unavailable(_) => ("-", Style::new().fg(Color::Red)),
        };
        spans.push(Span::styled(format!("{name}{glyph} "), style.patch(DIM)));
    }
    Paragraph::new(Line::from(spans))
}

/// Body lines plus, for hit-testing, each selectable row's line offset.
fn body_paragraph(app: &App, height: usize) -> (Paragraph<'static>, Vec<(u16, Row)>) {
    if let Mode::SpawnPicker { query, cursor } = &app.mode {
        return spawn_picker_lines(app, query, *cursor, height);
    }
    if let Mode::GotoPicker { query, cursor } = &app.mode {
        return goto_picker_lines(app, query, *cursor, height);
    }
    if let Mode::WorktreeName { project, buffer } = &app.mode {
        return worktree_name_lines(project, buffer, height);
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<(u16, Row)> = Vec::new();
    let selected = app.selected.clone();
    let rows = app.rows();

    let focused_pane = app.focused_seat().map(|seat| seat.pane_id.clone());
    lines.push(Line::styled(" SEATS", HEADER_STYLE));
    let mut seat_count = 0;
    for row in rows.iter().filter(|row| row.section == Section::Seats) {
        let Some(seat) = app.seats.get(&row.key) else {
            continue;
        };
        seat_count += 1;
        let field_session = app.field_session_id.as_deref();
        let marker = if focused_pane.as_deref() == Some(seat.pane_id.as_str()) {
            "*"
        } else if seat.dead {
            "x"
        } else {
            " "
        };
        // An untagged pane sitting in the field window is the slot's
        // placeholder shell; window names would mislead here.
        let label = if seat.name.is_empty()
            && app.field_window_id.as_deref() == Some(seat.window_id.as_str())
        {
            "(shell)".to_owned()
        } else {
            seat.display_name(field_session).to_owned()
        };
        // Agent state at a glance: a filled dot when the agent is running
        // in this pane, a hollow one otherwise (a bare shell, or an exited
        // agent). Primary detection is process-tree based
        // (`app.agent_panes`, from `feeds::agents`): `pane_current_command`
        // alone is fooled by shell-integration wrappers (e.g.
        // kiro-cli-term) that keep the pane reporting its login shell
        // forever while the agent runs as a descendant of it. The direct
        // `command == "nopal" | "pi"` check remains as a fallback for
        // panes running the agent with no such wrapper in the way (`pi`
        // because `nopal cli` execs into pi; `node`, pi's actual kernel
        // image name, is deliberately absent - any node process would
        // light the dot).
        let agent_glyph = if app.agent_panes.contains(&seat.pane_id)
            || matches!(seat.command.as_str(), "nopal" | "pi")
        {
            Span::styled("\u{25cf} ", Style::new().fg(Color::Green))
        } else {
            Span::styled("\u{25cb} ", DIM)
        };
        let mut line = Line::from(vec![
            Span::raw(format!(" {marker} ")),
            agent_glyph,
            Span::raw(label),
            Span::styled(format!(" [{}]", tag_or(&seat.repo_tag(), "-")), DIM),
            Span::styled(format!(" {}", seat.command), DIM),
        ]);
        if selected.as_deref() == Some(row.key.as_str()) {
            line = line.style(SELECTED);
        }
        hits.push((lines.len() as u16, row.clone()));
        lines.push(line);
    }
    if seat_count == 0 {
        lines.push(Line::styled("   (no seats; n to spawn)", DIM));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(" AFK RUNS", HEADER_STYLE));
    let mut run_count = 0;
    for row in rows.iter().filter(|row| row.section == Section::AfkRuns) {
        let Some(run) = app.runs.get(&row.key) else {
            continue;
        };
        run_count += 1;
        let glyph = match run.status.as_str() {
            "running" => Span::styled(" > ", Style::new().fg(Color::Green)),
            "completed" => Span::styled(" = ", Style::new().fg(Color::Blue)),
            "failed" | "terminated" => Span::styled(" ! ", Style::new().fg(Color::Red)),
            "paused" | "interrupted" => Span::styled(" ~ ", Style::new().fg(Color::Yellow)),
            _ => Span::raw(" ? "),
        };
        let label = if run.ticket.is_empty() {
            short_run_id(&run.run_id)
        } else {
            run.ticket.clone()
        };
        let mut line = Line::from(vec![
            glyph,
            Span::raw(label),
            Span::styled(format!(" [{}]", tag_or(&run.repo, "-")), DIM),
            Span::styled(format!(" {}", run.status), DIM),
        ]);
        if selected.as_deref() == Some(row.key.as_str()) {
            line = line.style(SELECTED);
        }
        hits.push((lines.len() as u16, row.clone()));
        lines.push(line);
    }
    if run_count == 0 {
        lines.push(Line::styled("   (no runs reported)", DIM));
    }

    lines.truncate(height);
    (Paragraph::new(lines), hits)
}

/// The spawn picker's body: title, the live query line, then
/// [`App::filtered_candidates`] in [`crate::seat::merge`]'s order (recents
/// first, then each project's root/worktrees/new-worktree row), with the
/// cursor row highlighted. An empty filtered list shows the free-entry
/// path hint instead.
fn spawn_picker_lines(
    app: &App,
    query: &str,
    cursor: usize,
    height: usize,
) -> (Paragraph<'static>, Vec<(u16, Row)>) {
    let candidates = app.filtered_candidates(query);
    let mut lines: Vec<Line<'static>> = vec![
        Line::styled(" new seat", HEADER_STYLE),
        Line::from(vec![Span::raw(format!(" > {query}_"))]),
        Line::raw(""),
    ];
    if candidates.is_empty() {
        lines.push(Line::styled("   <enter path>", DIM));
    } else {
        for (index, candidate) in candidates.iter().enumerate() {
            let mut spans = vec![Span::raw(format!("   {}", candidate.label))];
            if candidate.kind != CandidateKind::NewWorktree {
                spans.push(Span::styled(format!(" [{}]", candidate.project), DIM));
            }
            let mut line = Line::from(spans);
            if index == cursor {
                line = line.style(SELECTED);
            }
            lines.push(line);
        }
    }
    lines.truncate(height);
    (Paragraph::new(lines), Vec::new())
}

/// The goto picker's body: title, the live query line, then
/// [`App::goto_candidates`]'s fuzzy-filtered existing seats, cursor row
/// highlighted - the same shape as [`spawn_picker_lines`], but jumping to
/// a live seat instead of spawning one. No row hits: like the spawn
/// picker, the mouse has nothing well-defined to hit while this overlay
/// owns the sidebar body (see the mode guard in `app.rs::handle_mouse`).
fn goto_picker_lines(
    app: &App,
    query: &str,
    cursor: usize,
    height: usize,
) -> (Paragraph<'static>, Vec<(u16, Row)>) {
    let field_session = app.field_session_id.as_deref();
    let seats = app.goto_candidates(query);
    let mut lines: Vec<Line<'static>> = vec![
        Line::styled(" goto seat", HEADER_STYLE),
        Line::from(vec![Span::raw(format!(" > {query}_"))]),
        Line::raw(""),
    ];
    if seats.is_empty() {
        lines.push(Line::styled("   (no matching seats)", DIM));
    } else {
        for (index, seat) in seats.iter().enumerate() {
            let spans = vec![
                Span::raw(format!("   {}", seat.display_name(field_session))),
                Span::styled(format!(" [{}]", tag_or(&seat.repo_tag(), "-")), DIM),
            ];
            let mut line = Line::from(spans);
            if index == cursor {
                line = line.style(SELECTED);
            }
            lines.push(line);
        }
    }
    lines.truncate(height);
    (Paragraph::new(lines), Vec::new())
}

/// The new-worktree name prompt: shown after picking a `+ new worktree in
/// <project>` row, before `git worktree add` runs.
fn worktree_name_lines(
    project: &str,
    buffer: &str,
    height: usize,
) -> (Paragraph<'static>, Vec<(u16, Row)>) {
    let project_name = crate::state::worktree_repo_tag(project);
    let mut lines: Vec<Line<'static>> = vec![
        Line::styled(" new worktree", HEADER_STYLE),
        Line::raw(""),
        Line::from(vec![Span::raw(format!(
            " new worktree in {project_name}: {buffer}_"
        ))]),
    ];
    lines.truncate(height);
    (Paragraph::new(lines), Vec::new())
}

fn asks_paragraph(app: &App) -> Paragraph<'static> {
    let asks: Vec<_> = app.pending_asks().collect();
    let selected = app.selected.clone();
    let mut lines = vec![Line::styled(
        format!(" ASKS ({} pending)", asks.len()),
        ASK_STYLE,
    )];
    for ask in asks.iter().take(3) {
        let mut line = Line::from(vec![
            Span::styled(" ! ", ASK_STYLE),
            Span::raw(ask.action.clone()),
            Span::styled(format!(" [{}]", tag_or(&ask.repo, "-")), DIM),
            Span::styled(format!(" {}", ask.session_id), DIM),
        ]);
        if selected.as_deref() == Some(ask.ask_id.as_str()) {
            line = line.style(SELECTED);
        }
        lines.push(line);
    }
    if asks.len() > 3 {
        lines.push(Line::styled(format!("   (+{} more)", asks.len() - 3), DIM));
    }
    Paragraph::new(lines)
}

fn status_line(app: &App) -> Paragraph<'static> {
    let text = match &app.mode {
        Mode::Filter(text) => format!(" /{text}_"),
        _ if !app.status_line.is_empty() => format!(" {}", app.status_line),
        _ => format!(" {}", default_hint(&app.keys)),
    };
    Paragraph::new(Line::styled(text, DIM))
}

/// The idle status-line hint, rebuilt from the effective key registry so a
/// remap is self-documenting the same way [`draw_help`] is. Digits, the
/// mouse gestures, and search's `n`/`N` cycle are not part of the
/// registry (see `crate::keys`'s module doc) and stay literal.
fn default_hint(keys: &KeyRegistry) -> String {
    let l = |action| keys.label(action);
    [
        format!("{}/{} move", l(KeyAction::MoveDown), l(KeyAction::MoveUp)),
        format!("{} cycle", l(KeyAction::SectionNext)),
        "1-9 jump".to_owned(),
        format!("click/{} view", l(KeyAction::OpenView)),
        "drag row->panel split".to_owned(),
        format!("{} focus", l(KeyAction::Focus)),
        format!(
            "{} / {} split",
            l(KeyAction::SplitRight),
            l(KeyAction::SplitBelow)
        ),
        format!("{} break", l(KeyAction::BreakToWindow)),
        format!("{} swap", l(KeyAction::SwapIntoSlot)),
        format!("{} collapse", l(KeyAction::Collapse)),
        format!("{} search (embed)", l(KeyAction::EmbedSearch)),
        "n/N next/prev match".to_owned(),
        format!("{} goto", l(KeyAction::GotoPicker)),
        format!("{} new", l(KeyAction::SpawnPicker)),
        format!("{} kill", l(KeyAction::Kill)),
        format!("{} start", l(KeyAction::Relaunch)),
        format!("{} adopt", l(KeyAction::Adopt)),
        format!("{}/{} ask", l(KeyAction::AskApprove), l(KeyAction::AskDeny)),
        format!("{} filter (sidebar)", l(KeyAction::Filter)),
        format!("{} help", l(KeyAction::Help)),
    ]
    .join("  ")
}

fn draw_run_detail(frame: &mut Frame<'_>, app: &App, key: &str, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    match app.runs.get(key) {
        None => lines.push(Line::raw("run vanished; esc to go back")),
        Some(run) => {
            lines.push(Line::styled(
                format!(
                    " {} [{}] {}",
                    run.run_id,
                    tag_or(&run.repo, "-"),
                    run.status
                ),
                HEADER_STYLE,
            ));
            if !run.ticket.is_empty() || !run.branch.is_empty() {
                lines.push(Line::styled(format!(" {} {}", run.ticket, run.branch), DIM));
            }
            if !run.gates.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled(" GATES", HEADER_STYLE));
                for gate in run.gates.iter().take(6) {
                    let style = if gate.ends_with("pass") {
                        Style::new().fg(Color::Green)
                    } else {
                        Style::new().fg(Color::Red)
                    };
                    lines.push(Line::styled(format!("   {gate}"), style));
                }
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(" EVENTS", HEADER_STYLE));
            if run.events.is_empty() {
                lines.push(Line::styled(
                    "   (no structured events; ledger runs gain them via rondo)",
                    DIM,
                ));
            }
            let budget =
                (area.height as usize).saturating_sub(lines.len() + run.evidence.len().min(6) + 3);
            let skip = run.events.len().saturating_sub(budget);
            for event in run.events.iter().skip(skip) {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {:>4} ", event.sequence), DIM),
                    Span::raw(short_kind(&event.kind).to_owned()),
                    Span::styled(format!(" {}", event.detail), DIM),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(" EVIDENCE", HEADER_STYLE));
            for (kind, uri) in run.evidence.iter().take(6) {
                lines.push(Line::from(vec![
                    Span::raw(format!(" {kind} ")),
                    Span::styled(uri.clone(), DIM),
                ]));
            }
        }
    }
    lines.push(Line::styled(" esc back", DIM));
    frame.render_widget(Paragraph::new(lines), area);
}

fn embed_header_line(
    label: &str,
    pane_id: &str,
    input_focus: bool,
    cols: u16,
    rows: u16,
    keys: &KeyRegistry,
) -> Line<'static> {
    let mode = if input_focus { "typing" } else { "preview" };
    let hint = if input_focus {
        format!("{} sidebar", keys.label(KeyAction::ReleaseInput))
    } else {
        let open = keys.label(KeyAction::OpenView);
        let input = keys.label(KeyAction::InputFocus);
        let type_keys = if open == input {
            open
        } else {
            format!("{open}/{input}")
        };
        format!(
            "{type_keys} type, {} full focus, esc close",
            keys.label(KeyAction::Focus)
        )
    };
    Line::from(vec![
        Span::styled(format!(" {label} "), HEADER_STYLE),
        Span::styled(format!("[{pane_id}] "), DIM),
        Span::styled(
            format!(" {mode} "),
            if input_focus {
                Style::new().bg(Color::Green).fg(Color::Black)
            } else {
                Style::new().bg(Color::Indexed(237))
            },
        ),
        Span::styled(format!("  {cols}x{rows}  ({hint})"), DIM),
    ])
}

/// Render the embedded seat's VT grid into `area`. The grid is sized to the
/// seat's real dimensions (snapshotted at attach); we center it and clip to
/// the panel, so the seat is never resized. ratatui's own cell-diff between
/// frames means only changed cells reach the outer terminal. Scrolled-back
/// rows and the active mouse selection, if any, render from
/// the same `renderable_content()` snapshot.
fn draw_embed(
    frame: &mut Frame<'_>,
    embed: &Embed,
    area: Rect,
    hit: &mut HitMap,
    keys: &KeyRegistry,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let header = embed_header_line(
        &embed.label,
        &embed.pane_id,
        embed.input_focus,
        embed.cols,
        embed.rows,
        keys,
    );
    let header_area = Rect { height: 1, ..area };
    frame.render_widget(Paragraph::new(header), header_area);

    let grid = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    if grid.width == 0 || grid.height == 0 {
        return;
    }
    hit.embed_grid = Some(grid);

    let content = embed.term().renderable_content();
    let cols = embed.cols;
    let rows = embed.rows;
    // Center within the panel when it is larger; clip from the top-left when
    // it is smaller.
    let x_off = grid.width.saturating_sub(cols) / 2;
    let y_off = grid.height.saturating_sub(rows) / 2;
    // `display_iter` numbers rows relative to the *live* screen (line 0 is
    // its top; scrolled-back rows are negative - see `Grid::display_iter`),
    // shifted by however far back the view has scrolled. Adding the offset
    // back converts to an on-screen row, so history renders exactly like a
    // live row once it is the one under the viewport (v0 had
    // no scrollback at all, so every row was already screen-relative and
    // this shift was always zero).
    let display_offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor_point = content.cursor.point;
    let cursor_shape = content.cursor.shape;
    // Scrollback-search highlights: a snapshot computed at
    // the last Enter/n/N jump, not recomputed per frame - see the doc on
    // `EmbedSearch::Active` for why that is correct even as new output
    // arrives mid-search.
    let search_current = embed.search_current_match();
    let search_visible = embed.search_visible_matches();

    let buf = frame.buffer_mut();
    for indexed in content.display_iter {
        let row = indexed.point.line.0 + display_offset;
        if row < 0 || row as u16 >= rows {
            continue;
        }
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue; // the wide char to its left already occupies this slot
        }
        let col = indexed.point.column.0 as u16;
        let cx = grid.x.saturating_add(x_off).saturating_add(col);
        let cy = grid.y.saturating_add(y_off).saturating_add(row as u16);
        if cx >= grid.right() || cy >= grid.bottom() {
            continue;
        }
        if let Some(target) = buf.cell_mut(Position::new(cx, cy)) {
            let symbol = if cell.c == '\0' { ' ' } else { cell.c };
            target.set_symbol(symbol.encode_utf8(&mut [0u8; 4]));
            let mut style = vt_style(cell.fg, cell.bg, cell.flags);
            if search_current.is_some_and(|m| m.contains(&indexed.point)) {
                style = style
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::BOLD);
            } else if search_visible.iter().any(|m| m.contains(&indexed.point)) {
                style = style.patch(SEARCH_MATCH);
            }
            // `contains_cell` also special-cases the block cursor so a
            // selection edge does not paint over it (mirrors alacritty's
            // own renderer).
            if selection.is_some_and(|s| s.contains_cell(&indexed, cursor_point, cursor_shape)) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            target.set_style(style);
        }
    }

    // Render the cursor from grid state (not by toggling DECTCEM every
    // frame - that is herdr's flicker class). We surface the hardware cursor
    // only when the seat holds input focus. The cursor's line is in the
    // same live-screen-relative space as `display_iter`'s points, so it
    // naturally scrolls off-screen (row falls outside 0..rows) once the
    // view is scrolled back far enough - matching a real terminal, where
    // the cursor is invisible while you are browsing scrollback.
    if embed.input_focus && cursor_shape != CursorShape::Hidden {
        let row = cursor_point.line.0 + display_offset;
        if row >= 0 && (row as u16) < rows {
            let cx = grid
                .x
                .saturating_add(x_off)
                .saturating_add(cursor_point.column.0 as u16);
            let cy = grid.y.saturating_add(y_off).saturating_add(row as u16);
            if cx < grid.right() && cy < grid.bottom() {
                frame.set_cursor_position(Position::new(cx, cy));
            }
        }
    }
}

/// Row entries whose key column is a fixed literal - overlay-internal
/// bindings, mouse gestures, and the non-remappable digit jump table - are
/// `Fixed`; ones that render an action's live registry label are
/// `Bound`, so a remap is self-documenting the same way the status hint
/// (see `default_hint`) is.
enum HelpKey {
    Fixed(&'static str),
    Bound(KeyAction),
    /// Two actions sharing one row, joined with " / " (e.g. split right /
    /// split below).
    BoundPair(KeyAction, KeyAction),
    /// A literal prefix (always "esc" in practice - the one key this
    /// table never remaps) joined with " / " to a bound action, for the
    /// `esc / close_embed` row: esc is the permanent fallback,
    /// `close_embed` the remappable alias (see `crate::keys::KeyAction::CloseEmbed`).
    FixedThenBound(&'static str, KeyAction),
}

/// Full keybinding reference, centered over the current view.
fn draw_help(frame: &mut Frame<'_>, keys: &KeyRegistry, area: Rect, hit: &mut HitMap) {
    use HelpKey::{Bound, BoundPair, Fixed, FixedThenBound};
    let rows: &[(HelpKey, &str)] = &[
        (
            BoundPair(KeyAction::MoveDown, KeyAction::MoveUp),
            "sidebar: move the row selection; embed: move the Plot selection and retarget",
        ),
        (
            BoundPair(KeyAction::SectionNext, KeyAction::SectionPrev),
            "cycle sidebar sections: seats -> afk runs -> asks -> wrap",
        ),
        (
            BoundPair(KeyAction::ActivityNext, KeyAction::ActivityPrev),
            "stage: cycle Plot activities: Sessions -> Rondo executions -> wrap",
        ),
        (
            Fixed("1-9"),
            "sidebar: jump to the nth seat; embed: jump to the nth Plot and retarget",
        ),
        (
            Bound(KeyAction::OpenView),
            "open the selected seat live with input focus",
        ),
        (
            Bound(KeyAction::Focus),
            "full-focus the seat's real pane (zero-overhead flagship)",
        ),
        (
            BoundPair(KeyAction::SplitRight, KeyAction::SplitBelow),
            "split the selected seat into the slot, right / below (join-pane)",
        ),
        (
            Bound(KeyAction::BreakToWindow),
            "break the selected seat to its own window (mouse equivalent: right-click, \
             break to window); split-in seats break out first",
        ),
        (
            Bound(KeyAction::SwapIntoSlot),
            "swap the selected seat into the slot; the sidebar keeps focus",
        ),
        (
            Bound(KeyAction::InputFocus),
            "embedded view: give the seat input focus back",
        ),
        (
            Bound(KeyAction::ReleaseInput),
            "embedded view: release input focus back to the sidebar",
        ),
        (
            FixedThenBound("esc", KeyAction::CloseEmbed),
            "embedded view: close and return to the sidebar",
        ),
        (
            Bound(KeyAction::Collapse),
            "hide or show the Plot inspector while a Session is open",
        ),
        (
            Bound(KeyAction::EmbedSearch),
            "embedded view: open scrollback search over the mirror (case-insensitive by default)",
        ),
        (
            Fixed("n / N"),
            "embedded view, search active: next match toward history / previous match toward the live tail, wrapping",
        ),
        (
            Fixed("click (seat row)"),
            "select and open the seat's embedded view",
        ),
        (
            Fixed("click (run / ask row)"),
            "select; click the selection again to open detail / show context",
        ),
        (
            Fixed("right-click (seat row)"),
            "context menu: open / kill / relaunch / spawn-here / swap into slot, plus \
             split right / split below / break to window for a windowed seat, or break \
             to window / return to its window for one already split in",
        ),
        (
            Fixed("drag (seat row) -> panel"),
            "with a seat open: drop on the panel's center to open/retarget it (same as \
             a click), or on an edge band to real-split it there (join-pane, \
             left/right/top/bottom); with no seat open, or dropped elsewhere, cancels - \
             esc cancels mid-drag too",
        ),
        (
            Fixed("click (embed)"),
            "the grid takes seat input; a sidebar row releases it",
        ),
        (
            Fixed("drag (embed)"),
            "select text; release to copy it to the clipboard",
        ),
        (
            Fixed("double-click (embed)"),
            "copy the word/token under the pointer",
        ),
        (
            Fixed("wheel"),
            "sidebar: move the selection; embed: scroll back, or reach the seat when it has mouse reporting on",
        ),
        (Bound(KeyAction::AskJump), "jump to the pending ask queue"),
        (
            BoundPair(KeyAction::AskApprove, KeyAction::AskDeny),
            "approve / deny the selected ask",
        ),
        (
            Bound(KeyAction::Filter),
            "sidebar-only: filter the sidebar (subsequence match)",
        ),
        (
            Bound(KeyAction::SpawnPicker),
            "spawn picker: type to filter, enter to spawn or create a worktree",
        ),
        (
            Bound(KeyAction::GotoPicker),
            "goto picker: fuzzy-jump to an existing seat by name/repo",
        ),
        (
            Bound(KeyAction::Kill),
            "kill the selected seat (y confirms, anything else cancels)",
        ),
        (
            Bound(KeyAction::Relaunch),
            "(re)launch the agent in the selected seat's pane",
        ),
        (
            Bound(KeyAction::ShowAll),
            "toggle showing all sessions vs nopal-managed only",
        ),
        (
            Bound(KeyAction::Adopt),
            "adopt the selected unmanaged session into nopal",
        ),
        (
            Bound(KeyAction::Reconcile),
            "reconcile the server-wide seat snapshot",
        ),
        (
            Bound(KeyAction::Profiling),
            "show render profiling counters",
        ),
        (Bound(KeyAction::Help), "toggle this help"),
        (
            Bound(KeyAction::Quit),
            "quit the UI (tmux session and seats survive)",
        ),
    ];
    let mut lines: Vec<Line<'static>> = vec![
        Line::styled(" nopal field - keys", HEADER_STYLE),
        Line::raw(""),
    ];
    for (key, desc) in rows {
        let label = match key {
            Fixed(text) => (*text).to_owned(),
            Bound(action) => keys.label(*action),
            BoundPair(a, b) => format!("{} / {}", keys.label(*a), keys.label(*b)),
            FixedThenBound(text, action) => format!("{text} / {}", keys.label(*action)),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {label:>16}  "), Style::new().fg(Color::Cyan)),
            Span::raw((*desc).to_owned()),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  getting back to the field from a full-focused seat: sesh last, or prefix L",
        DIM,
    ));
    // The help overlay's own dismiss chord is overlay-internal (like the
    // context menu's j/k or an active search's n/N) and stays on its
    // literal keys regardless of any `help`/`close_embed` remap - see
    // `handle_key`'s `Mode::Help` branch in `app.rs`.
    lines.push(Line::styled("  ? / esc / q to dismiss", DIM));

    let width = area.width.clamp(20, 78).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    hit.help = Some(popup);
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = ratatui::widgets::Block::bordered()
        .border_style(HEADER_STYLE)
        .style(Style::new().bg(Color::Indexed(235)));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// Right-click context menu for a seat row: a small popup centered over
/// the current view, listing [`App::context_menu_actions`]'s currently
/// visible list for the seat with the cursor row highlighted - a split-in
/// seat and an ordinary windowed seat see different action sets, so this is
/// no longer a fixed row count. Reuses [`draw_help`]'s
/// centered-popup shape at a much smaller footprint. Does not extend the
/// hitmap: dismissal is unconditional on the active mode in
/// `app.rs::handle_mouse` (any click closes it), never hit-tested against a
/// stored rect the way sidebar rows are.
fn draw_context_menu(frame: &mut Frame<'_>, app: &App, area: Rect, row_key: &str, cursor: usize) {
    let label = app
        .seats
        .get(row_key)
        .map(|seat| {
            seat.display_name(app.field_session_id.as_deref())
                .to_owned()
        })
        .unwrap_or_else(|| row_key.to_owned());
    let mut lines: Vec<Line<'static>> = vec![Line::styled(format!(" {label}"), HEADER_STYLE)];
    for (index, action) in app.context_menu_actions(row_key).iter().enumerate() {
        let mut line = Line::raw(format!(" {}", action.label()));
        if index == cursor {
            line = line.style(SELECTED);
        }
        lines.push(line);
    }
    lines.push(Line::styled(" j/k move  enter act  esc cancel", DIM));

    let width = 30.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = ratatui::widgets::Block::bordered()
        .border_style(HEADER_STYLE)
        .style(Style::new().bg(Color::Indexed(235)));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// Map an alacritty VT cell's colors and flags to a ratatui style.
fn vt_style(fg: VtColor, bg: VtColor, flags: Flags) -> Style {
    let mut style = Style::new().fg(map_color(fg)).bg(map_color(bg));
    let mut modifier = Modifier::empty();
    if flags.contains(Flags::BOLD) {
        modifier |= Modifier::BOLD;
    }
    if flags.contains(Flags::DIM) {
        modifier |= Modifier::DIM;
    }
    if flags.contains(Flags::ITALIC) {
        modifier |= Modifier::ITALIC;
    }
    if flags.intersects(Flags::ALL_UNDERLINES) {
        modifier |= Modifier::UNDERLINED;
    }
    if flags.contains(Flags::INVERSE) {
        modifier |= Modifier::REVERSED;
    }
    if flags.contains(Flags::HIDDEN) {
        modifier |= Modifier::HIDDEN;
    }
    if flags.contains(Flags::STRIKEOUT) {
        modifier |= Modifier::CROSSED_OUT;
    }
    style.add_modifier = modifier;
    style
}

/// Map an alacritty VT color to a ratatui color. Named default fg/bg become
/// Reset so the outer terminal's own defaults show through; 256/RGB pass as
/// indexed/true-color (the documented v1 color scope).
fn map_color(color: VtColor) -> Color {
    match color {
        VtColor::Named(named) => match named {
            NamedColor::Black => Color::Black,
            NamedColor::Red => Color::Red,
            NamedColor::Green => Color::Green,
            NamedColor::Yellow => Color::Yellow,
            NamedColor::Blue => Color::Blue,
            NamedColor::Magenta => Color::Magenta,
            NamedColor::Cyan => Color::Cyan,
            NamedColor::White => Color::Gray,
            NamedColor::BrightBlack => Color::DarkGray,
            NamedColor::BrightRed => Color::LightRed,
            NamedColor::BrightGreen => Color::LightGreen,
            NamedColor::BrightYellow => Color::LightYellow,
            NamedColor::BrightBlue => Color::LightBlue,
            NamedColor::BrightMagenta => Color::LightMagenta,
            NamedColor::BrightCyan => Color::LightCyan,
            NamedColor::BrightWhite => Color::White,
            _ => Color::Reset,
        },
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Indexed(i),
    }
}

fn tag_or(tag: &str, fallback: &str) -> String {
    if tag.is_empty() {
        fallback.to_owned()
    } else {
        tag.to_owned()
    }
}

fn short_run_id(run_id: &str) -> String {
    if run_id.len() > 18 {
        format!("..{}", &run_id[run_id.len() - 16..])
    } else {
        run_id.to_owned()
    }
}

fn short_kind(kind: &str) -> &str {
    kind.strip_prefix("rondo.").unwrap_or(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Notification;
    use crate::state::{
        Plot, PlotActivityKey, PlotExecution, PlotExecutionEvidence, PlotInspectorTab, PlotSession,
    };
    use alacritty_terminal::vte::ansi::Rgb;
    use std::collections::BTreeMap;

    #[test]
    fn hitmap_resolves_rows_and_grid() {
        let map = HitMap {
            rows: vec![
                (
                    Rect::new(0, 2, 44, 1),
                    Row {
                        section: Section::Seats,
                        key: "%2".to_owned(),
                    },
                ),
                (
                    Rect::new(0, 3, 44, 1),
                    Row {
                        section: Section::AfkRuns,
                        key: "ledger:x".to_owned(),
                    },
                ),
            ],
            embed_grid: Some(Rect::new(44, 1, 80, 40)),
            help: None,
            panel: None,
            ..HitMap::default()
        };
        assert_eq!(map.row_at(0, 2).map(|r| r.key.as_str()), Some("%2"));
        assert_eq!(map.row_at(43, 3).map(|r| r.key.as_str()), Some("ledger:x"));
        assert_eq!(map.row_at(44, 2), None, "clicks right of the row miss");
        assert_eq!(map.row_at(0, 4), None, "clicks below the rows miss");
        assert!(map.in_embed_grid(44, 1));
        assert!(map.in_embed_grid(123, 40));
        assert!(!map.in_embed_grid(43, 5), "sidebar is not the grid");
        assert!(!map.in_embed_grid(124, 1), "right of the grid misses");
    }

    #[test]
    fn embedded_header_says_typing_not_input_mode() {
        let keys = KeyRegistry::defaults();
        let header = embed_header_line("alpha", "%3", true, 120, 40, &keys);
        let text = header
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("alpha"));
        assert!(text.contains("typing"));
        assert!(text.contains("Ctrl-o sidebar"));
        assert!(!text.contains("INPUT"));
        assert!(!text.contains("esc close"));
    }

    #[test]
    fn help_documents_stage_activity_navigation() {
        let mut app = App::new("%1".to_owned(), "nopal".to_owned());
        app.mode = Mode::Help;
        let backend = ratatui::backend::TestBackend::new(100, 60);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("cycle Plot activities"));
    }

    #[test]
    fn embedded_header_says_preview_and_how_to_type() {
        let keys = KeyRegistry::defaults();
        let header = embed_header_line("alpha", "%3", false, 120, 40, &keys);
        let text = header
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("preview"));
        assert!(text.contains("enter/i type"));
        assert!(text.contains("f full focus"));
        assert!(text.contains("esc close"));
        assert!(!text.contains("VIEW"));
    }

    #[test]
    fn embedded_header_uses_remapped_key_labels() {
        let raw = BTreeMap::from([
            ("release_input".to_owned(), "ctrl-x".to_owned()),
            ("input_focus".to_owned(), "t".to_owned()),
            ("focus".to_owned(), "F".to_owned()),
        ]);
        let (keys, problems) = KeyRegistry::build(&raw);
        assert!(problems.is_empty());
        let typing = embed_header_line("alpha", "%3", true, 120, 40, &keys)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(typing.contains("Ctrl-x sidebar"));

        let preview = embed_header_line("alpha", "%3", false, 120, 40, &keys)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(preview.contains("enter/t type"));
        assert!(preview.contains("F full focus"));
    }

    #[test]
    fn draw_records_sidebar_row_hits() {
        let mut app = App::new("%1".to_owned(), "nopal".to_owned());
        app.reduce_tmux(&Notification::SubscriptionChanged {
            name: crate::state::SEAT_SUBSCRIPTION_NAME.to_owned(),
            pane_id: None,
            window_id: None,
            value: "%3|@2|seat:a|zsh|alpha|nopal||1|0|$1|nopal|1|1|/home/nopal/x".to_owned(),
        });
        let backend = ratatui::backend::TestBackend::new(44, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        // Title at y=0, SEATS header at y=1, the seat row at y=2.
        let row = app.hit.row_at(3, 2).cloned().expect("seat row recorded");
        assert_eq!(row.key, "%3");
        assert_eq!(row.section, Section::Seats);
        assert!(app.hit.embed_grid.is_none(), "no embed drawn");
        assert!(app.hit.help.is_none(), "no help drawn");
    }

    #[test]
    fn draw_records_help_popup() {
        let mut app = App::new("%1".to_owned(), "nopal".to_owned());
        app.enter_help();
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(app.hit.help.is_some(), "help popup recorded");
    }

    // --- Plot-first three-region layout arithmetic ---

    #[test]
    fn plot_first_layout_keeps_the_activity_stage_dominant() {
        assert_eq!(plot_first_widths(160, false), (28, 34));
        assert_eq!(160 - 28 - 34, 98);
        assert_eq!(plot_first_widths(134, false), (28, 34));
        assert_eq!(134 - 28 - 34, 72);
    }

    #[test]
    fn plot_first_layout_hides_the_inspector_before_the_plot_rail() {
        assert_eq!(plot_first_widths(100, false), (24, 0));
        assert_eq!(plot_first_widths(90, false), (24, 0));
        assert_eq!(plot_first_widths(89, false), (18, 0));
        assert_eq!(plot_first_widths(160, true), (28, 0));
        assert_eq!(plot_first_widths(20, false), (18, 0));
        assert_eq!(plot_first_widths(1, false), (0, 0));
    }

    #[test]
    fn execution_activity_is_read_only_and_records_exact_hit_regions() {
        let mut app = plot_stage_fixture();
        app.stage_open = true;
        app.embed = Some(Embed::test_for_app("%4", "fixture", true));
        app.selected_plot_activity = Some(PlotActivityKey::Execution {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: "run-42".to_owned(),
        });
        app.inspector_tab = PlotInspectorTab::Evidence;
        let backend = ratatui::backend::TestBackend::new(160, 45);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("run-42"));
        assert!(rendered.contains("rondo-core / repo-a / run-42"));
        assert!(rendered.contains("rondo-run://repo-a/run-42/log"));
        assert!(app.hit.embed_grid.is_none());
        assert!(app.hit.panel.is_none());
        assert_eq!(app.hit.main, Some(Rect::new(28, 3, 98, 42)));
        assert_eq!(app.hit.inspector, Some(Rect::new(126, 0, 34, 45)));
        let execution_hit = app
            .hit
            .activity_tabs
            .iter()
            .find(|(_, key)| matches!(key, PlotActivityKey::Execution { run_id, .. } if run_id == "run-42"))
            .expect("execution tab hit");
        assert_eq!(
            app.hit.activity_at(execution_hit.0.x, execution_hit.0.y),
            Some(&execution_hit.1)
        );
        let evidence_hit = app
            .hit
            .inspector_tabs
            .iter()
            .find(|(_, tab)| *tab == PlotInspectorTab::Evidence)
            .expect("evidence tab hit");
        assert_eq!(
            app.hit.inspector_tab_at(evidence_hit.0.x, evidence_hit.0.y),
            Some(PlotInspectorTab::Evidence)
        );
    }

    #[test]
    fn unavailable_session_and_absent_fruit_are_honest() {
        let mut app = plot_stage_fixture();
        app.stage_open = true;
        app.selected_plot_activity = Some(PlotActivityKey::Session("session-fixture".to_owned()));
        app.inspector_tab = PlotInspectorTab::Fruit;
        let backend = ratatui::backend::TestBackend::new(160, 45);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Session unavailable"));
        assert!(rendered.contains("No live tmux pane is attached"));
        assert!(rendered.contains("Fruit is absent"));
        assert!(app.hit.embed_grid.is_none());
        assert!(app.hit.panel.is_none());
    }

    #[test]
    fn selected_session_renders_only_its_matching_live_embed() {
        let mut app = plot_stage_fixture();
        app.stage_open = true;
        app.selected_plot_activity = Some(PlotActivityKey::Session("session-fixture".to_owned()));
        app.embed = Some(Embed::test_for_app("%4", "fixture", false));
        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert!(app.hit.embed_grid.is_some());
        assert_eq!(app.hit.panel, app.hit.main);
    }

    #[test]
    fn selected_session_uses_its_tagged_seat_when_the_host_pane_hint_is_absent() {
        let mut app = plot_stage_fixture();
        app.stage_open = true;
        app.selected_plot_activity = Some(PlotActivityKey::Session("session-fixture".to_owned()));
        app.plots.get_mut("plot-fixture").unwrap().sessions[0].host_pane = None;
        app.seats.insert(
            "%fallback".to_owned(),
            crate::state::Seat {
                pane_id: "%fallback".to_owned(),
                plot_session_id: Some("session-fixture".to_owned()),
                ..crate::state::Seat::default()
            },
        );
        app.embed = Some(Embed::test_for_app("%fallback", "fixture", false));
        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert!(app.hit.embed_grid.is_some());
        assert_eq!(app.hit.panel, app.hit.main);
    }

    fn plot_stage_fixture() -> App {
        let mut app = App::new("%1".to_owned(), "nopal".to_owned());
        let plot = Plot {
            plot_id: "plot-fixture".to_owned(),
            title: "Dogfood Plot".to_owned(),
            provisional: false,
            progress: "active".to_owned(),
            conditions: vec!["Core green".to_owned()],
            seed_source: "field_open".to_owned(),
            seed_text: "Exercise the walking skeleton".to_owned(),
            intent: "Prove the Plot-first flow".to_owned(),
            fruit_state: "absent".to_owned(),
            executions: vec![PlotExecution {
                service_id: "rondo-core".to_owned(),
                repo_id: "repo-a".to_owned(),
                run_id: "run-42".to_owned(),
                manifest_sha256: "b".repeat(64),
                status: "completed".to_owned(),
                outcome: Some("succeeded".to_owned()),
                event_cursor: "rondo.core/v1:9".to_owned(),
                evidence: vec![PlotExecutionEvidence {
                    artifact_kind: "log".to_owned(),
                    uri: "rondo-run://repo-a/run-42/log".to_owned(),
                }],
                created_at: "2026-07-12T10:00:00Z".to_owned(),
                updated_at: "2026-07-12T10:01:00Z".to_owned(),
            }],
            sessions: vec![PlotSession {
                session_id: "session-fixture".to_owned(),
                mode: "interactive".to_owned(),
                host: "pi".to_owned(),
                host_session: "nopal-work".to_owned(),
                host_pane: Some("%4".to_owned()),
                state: "active".to_owned(),
                workspace: Some("workspace-primary".to_owned()),
            }],
            selected_session_id: Some("session-fixture".to_owned()),
            establishment: None,
            repositories: vec![crate::state::PlotRepository {
                repository_id: "repo-a".to_owned(),
                root: "/repo".to_owned(),
                configuration_root: "/repo".to_owned(),
                revision: Some("abc123".to_owned()),
                roots: vec![crate::state::PlotRoot {
                    id: "dogfood-quality".to_owned(),
                    statement: "The complete flow is usable".to_owned(),
                    proof_requirements: vec![crate::state::PlotProofRequirement {
                        id: "full-gates".to_owned(),
                        stage: "pre_pr".to_owned(),
                        required: true,
                        gates: vec!["test".to_owned()],
                        on_missing: "block".to_owned(),
                        on_failure: "block".to_owned(),
                    }],
                }],
                gate_ids: vec!["test".to_owned()],
            }],
            workspaces: Vec::new(),
        };
        app.plots.insert(plot.plot_id.clone(), plot);
        app.selected_plot_id = Some("plot-fixture".to_owned());
        app.inspector_tab = PlotInspectorTab::Roots;
        app
    }

    #[test]
    fn plot_rail_and_inspector_render_core_owned_facts() {
        let mut app = App::new("%1".to_owned(), "nopal".to_owned());
        let plot = Plot {
            plot_id: "plot-fixture".to_owned(),
            title: "Dogfood Plot".to_owned(),
            provisional: true,
            progress: "active".to_owned(),
            conditions: vec!["Core green".to_owned()],
            seed_source: "field_open".to_owned(),
            seed_text: "Exercise the walking skeleton".to_owned(),
            intent: "Prove the Plot-first flow".to_owned(),
            fruit_state: "absent".to_owned(),
            executions: Vec::new(),
            sessions: vec![PlotSession {
                session_id: "session-fixture".to_owned(),
                mode: "interactive".to_owned(),
                host: "pi".to_owned(),
                host_session: "nopal-work".to_owned(),
                host_pane: Some("%4".to_owned()),
                state: "active".to_owned(),
                workspace: None,
            }],
            selected_session_id: Some("session-fixture".to_owned()),
            establishment: Some(crate::state::PlotEstablishment {
                event: "kickoff_context_ready".to_owned(),
                primary_repository_id: "repository-primary".to_owned(),
                workflow_source_repository_id: "repository-primary".to_owned(),
                workflow_source_hash: "0123456789abcdef".to_owned(),
            }),
            repositories: vec![crate::state::PlotRepository {
                repository_id: "repository-primary".to_owned(),
                root: "/repo".to_owned(),
                configuration_root: "/repo".to_owned(),
                revision: Some("abc123".to_owned()),
                roots: vec![crate::state::PlotRoot {
                    id: "dogfood-quality".to_owned(),
                    statement: "The complete flow is usable".to_owned(),
                    proof_requirements: vec![crate::state::PlotProofRequirement {
                        id: "full-gates".to_owned(),
                        stage: "pre_pr".to_owned(),
                        required: true,
                        gates: vec!["test".to_owned()],
                        on_missing: "block".to_owned(),
                        on_failure: "block".to_owned(),
                    }],
                }],
                gate_ids: vec!["test".to_owned()],
            }],
            workspaces: vec![crate::state::PlotWorkspace {
                workspace_id: "workspace-primary".to_owned(),
                repository_id: "repository-primary".to_owned(),
                root: "/repo".to_owned(),
                revision: Some("abc123".to_owned()),
                kind: "primary".to_owned(),
            }],
        };
        app.plots.insert(plot.plot_id.clone(), plot);
        app.selected_plot_id = Some("plot-fixture".to_owned());
        app.inspector_tab = PlotInspectorTab::Roots;
        let backend = ratatui::backend::TestBackend::new(80, 36);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut hit = HitMap::default();

        terminal
            .draw(|frame| {
                draw_plot_rail(frame, &app, Rect::new(0, 0, 32, 36), &mut hit);
                draw_plot_inspector(frame, &app, Rect::new(32, 0, 48, 36), &mut hit);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Dogfood Plot"));
        assert!(rendered.contains("dogfood-quality"));
        assert!(rendered.contains("The complete flow is usable"));
        assert!(rendered.contains("full-gates"));
        assert!(rendered.contains("stage pre_pr"));
        assert!(rendered.contains("on missing block"));
        assert_eq!(
            hit.row_at(2, 1).map(|row| row.key.as_str()),
            Some("plot-fixture")
        );
    }

    #[test]
    fn draw_records_context_menu_popup_without_a_sidebar_row_hit() {
        let mut app = App::new("%1".to_owned(), "nopal".to_owned());
        app.reduce_tmux(&Notification::SubscriptionChanged {
            name: crate::state::SEAT_SUBSCRIPTION_NAME.to_owned(),
            pane_id: None,
            window_id: None,
            value: "%3|@2|seat:a|zsh|alpha|nopal||1|0|$1|nopal|1|1|/home/nopal/x".to_owned(),
        });
        app.mode = Mode::ContextMenu {
            row_key: "%3".to_owned(),
            cursor: 1,
        };
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        // Must not panic; the sidebar underneath still records its own
        // row hits (the menu is a non-hit-tested overlay on top of it).
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(
            app.hit.row_at(3, 2).is_some(),
            "sidebar hits still recorded"
        );
    }

    #[test]
    fn maps_named_rgb_and_indexed_colors() {
        assert_eq!(map_color(VtColor::Named(NamedColor::Red)), Color::Red);
        assert_eq!(
            map_color(VtColor::Named(NamedColor::BrightBlue)),
            Color::LightBlue
        );
        // Default fg/bg fall through to the outer terminal's own defaults.
        assert_eq!(
            map_color(VtColor::Named(NamedColor::Foreground)),
            Color::Reset
        );
        assert_eq!(
            map_color(VtColor::Named(NamedColor::Background)),
            Color::Reset
        );
        assert_eq!(
            map_color(VtColor::Spec(Rgb {
                r: 10,
                g: 20,
                b: 30
            })),
            Color::Rgb(10, 20, 30)
        );
        assert_eq!(map_color(VtColor::Indexed(200)), Color::Indexed(200));
    }

    #[test]
    fn vt_style_carries_attributes() {
        let flags = Flags::BOLD | Flags::UNDERLINE | Flags::INVERSE;
        let style = vt_style(
            VtColor::Named(NamedColor::Green),
            VtColor::Named(NamedColor::Black),
            flags,
        );
        assert_eq!(style.fg, Some(Color::Green));
        assert_eq!(style.bg, Some(Color::Black));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(style.add_modifier.contains(Modifier::REVERSED));
        assert!(!style.add_modifier.contains(Modifier::ITALIC));
    }

    // --- drop_zone_at: row-drag drop-zone hit test ---

    #[test]
    fn drop_zone_at_resolves_center_and_each_edge_band() {
        // width 20 -> band_x = 4 (cols 10..14 left, 26..30 right); height
        // 10 -> band_y = 2 (rows 5..7 top, 13..15 bottom).
        let panel = Rect::new(10, 5, 20, 10);
        assert_eq!(drop_zone_at(panel, 20, 10), Some(DropZone::Center));
        assert_eq!(drop_zone_at(panel, 11, 10), Some(DropZone::Left));
        assert_eq!(drop_zone_at(panel, 28, 10), Some(DropZone::Right));
        assert_eq!(drop_zone_at(panel, 20, 6), Some(DropZone::Top));
        assert_eq!(drop_zone_at(panel, 20, 14), Some(DropZone::Bottom));
    }

    #[test]
    fn drop_zone_at_is_none_outside_the_panel() {
        let panel = Rect::new(10, 5, 20, 10);
        assert_eq!(drop_zone_at(panel, 9, 10), None, "left of the panel");
        assert_eq!(drop_zone_at(panel, 30, 10), None, "right of the panel");
        assert_eq!(drop_zone_at(panel, 20, 4), None, "above the panel");
        assert_eq!(drop_zone_at(panel, 20, 15), None, "below the panel");
    }

    #[test]
    fn drop_zone_at_breaks_corner_ties_toward_the_horizontal_edge() {
        let panel = Rect::new(10, 5, 20, 10);
        // The top-left cell is equally close, by band ratio, to the left
        // and top bands; left wins. Same for the top-right cell and right.
        assert_eq!(drop_zone_at(panel, 10, 5), Some(DropZone::Left));
        assert_eq!(drop_zone_at(panel, 29, 5), Some(DropZone::Right));
    }
}
