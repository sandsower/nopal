//! Field sidebar state: the pure model every feed reduces into.
//!
//! The field renders and routes, but never decides: this
//! module holds facts reported by tmux, nopal ask, the run ledger, and the
//! rondo.core/v1 feed, and derives sidebar rows from them. Nothing here
//! spawns processes or talks to tmux.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::notify::Notification;
use crate::seat::Candidate;

/// Format installed by the sidecar for the pane subscription; [`reduce`]
/// parses values produced from it, so they must stay in lockstep.
/// `pane_current_path` is last so `splitn` tolerates `|` in paths.
pub const SEAT_SUBSCRIPTION_NAME: &str = "nopal-seats";
pub const SEAT_SUBSCRIPTION_FORMAT: &str = "#{pane_id}|#{window_id}|#{window_name}|#{pane_current_command}|#{@nopal_seat}|#{@nopal_repo}|#{@nopal_role}|#{@nopal_managed}|#{pane_dead}|#{session_id}|#{session_name}|#{pane_active}|#{window_active}|#{@nopal_plot}|#{@nopal_plot_session}|#{pane_current_path}";
const SEAT_FIELDS: usize = 16;
const LEGACY_SEAT_FIELDS: usize = 14;

/// One tmux pane observed on the server; a subset become sidebar seats.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seat {
    pub pane_id: String,
    pub window_id: String,
    pub window_name: String,
    pub command: String,
    /// `@nopal_seat` pane option; display falls back per [`Self::display_name`].
    pub name: String,
    /// `@nopal_repo` pane option; falls back to a path-derived tag in rows.
    pub repo: String,
    /// `@nopal_role` pane option: `"split"` for a seat `join_seat_split`
    /// moved into the field window, empty for an ordinary windowed seat.
    /// The field's own pane also carries `"field"`, but that pane never
    /// makes it into the seats map (see [`App::apply_seat_line`]), so a
    /// live `Seat`'s role is only ever `"split"` or `""`. The durable
    /// marker [`crate::tmux::Backend::join_seat_split`] stamps, so it
    /// survives across reconciles the way `name`/`repo` already do.
    pub role: String,
    /// True when the pane's session carries the `@nopal_managed` marker
    /// (nopal opened or adopted it). Session-scoped options resolve for
    /// every pane in the session, so this is uniform across a session's
    /// panes. The default sidebar shows only marker-bearing sessions.
    pub managed: bool,
    pub dead: bool,
    pub session_id: String,
    pub session_name: String,
    /// This pane is its session's active pane in the active window.
    pub active: bool,
    /// Core-owned Plot identity stamped on the tmux session.
    pub plot_id: Option<String>,
    /// Core-owned Session identity stamped alongside the Plot identity.
    pub plot_session_id: Option<String>,
    pub path: String,
}

impl Seat {
    /// Tagged name, else the session name for foreign sessions (sesh names
    /// sessions after the project dir), else the window name.
    pub fn display_name(&self, field_session: Option<&str>) -> &str {
        if !self.name.is_empty() {
            return &self.name;
        }
        if field_session.is_some_and(|session| session == self.session_id) {
            &self.window_name
        } else {
            &self.session_name
        }
    }

    /// Sidebar repo tag: the explicit tag, else derived from the pane's
    /// path with `nopal-*` worktree dirs grouped under their parent repo.
    pub fn repo_tag(&self) -> String {
        if !self.repo.is_empty() {
            return self.repo.clone();
        }
        worktree_repo_tag(&self.path)
    }

    /// True when this seat is currently split into the field window
    /// (`join_seat_split` moved it there, `break_seat_out` undoes it). The
    /// slot-coherence property the whole split/break/return feature rests
    /// on: [`App::focused_seat`] and the context menu's visible-action list
    /// both key off this bit rather than window membership, since a split
    /// pane shares the field's `window_id` with the true slot pane.
    pub fn is_split(&self) -> bool {
        self.role == "split"
    }
}

/// `/a/teotl/nopal-task-15-x` -> `teotl`; `/a/teotl` -> `teotl`.
pub fn worktree_repo_tag(path: &str) -> String {
    let mut parts = path.trim_end_matches('/').rsplit('/');
    let base = parts.next().unwrap_or_default();
    if let Some(worktree) = base.strip_prefix("nopal-") {
        let _ = worktree;
        if let Some(parent) = parts.next()
            && !parent.is_empty()
        {
            return parent.to_owned();
        }
    }
    base.to_owned()
}

/// Which feed reported an AFK run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSource {
    Ledger,
    Rondo,
}

/// One structured event row for a run's detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEventRow {
    pub sequence: u64,
    pub timestamp: String,
    pub kind: String,
    pub detail: String,
}

/// One AFK run: rendered from feeds (rondo.core/v1 events, run ledger),
/// never from log tails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfkRun {
    /// Stable sidebar key, e.g. `ledger:<run_id>` or `rondo:<repo>/<run>`.
    pub key: String,
    pub source: RunSource,
    pub run_id: String,
    pub repo: String,
    pub status: String,
    pub ticket: String,
    pub branch: String,
    pub updated_at: String,
    pub events: Vec<RunEventRow>,
    /// `(artifact_kind, uri)` evidence pointers.
    pub evidence: Vec<(String, String)>,
    /// Latest gate attempts as preformatted `name(scope): status` rows.
    pub gates: Vec<String>,
}

/// One pending policy ask (nopal.ask/v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub ask_id: String,
    pub action: String,
    pub reason: String,
    pub session_id: String,
    pub repo: String,
    pub state: String,
    pub created_at: String,
    pub expires_at: String,
}

/// One Core-owned Plot projected through `nopal.field/v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plot {
    pub plot_id: String,
    pub title: String,
    pub provisional: bool,
    pub progress: String,
    pub conditions: Vec<String>,
    pub seed_source: String,
    pub seed_text: String,
    pub intent: String,
    pub fruit_state: String,
    pub executions: Vec<PlotExecution>,
    pub sessions: Vec<PlotSession>,
    pub selected_session_id: Option<String>,
    pub establishment: Option<PlotEstablishment>,
    pub repositories: Vec<PlotRepository>,
    pub workspaces: Vec<PlotWorkspace>,
}

/// Stable identity of one item in a Plot's dominant activity stage.
/// Sessions and executions are siblings, but execution identity needs all
/// three contract coordinates to remain unambiguous across repositories.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlotActivityKey {
    Session(String),
    Execution {
        service_id: String,
        repo_id: String,
        run_id: String,
    },
}

impl Plot {
    /// Activities in stage order: interactive Sessions first, then durable
    /// executions, preserving Core's order within each group.
    pub fn activity_keys(&self) -> Vec<PlotActivityKey> {
        self.sessions
            .iter()
            .map(|session| PlotActivityKey::Session(session.session_id.clone()))
            .chain(
                self.executions
                    .iter()
                    .map(|execution| PlotActivityKey::Execution {
                        service_id: execution.service_id.clone(),
                        repo_id: execution.repo_id.clone(),
                        run_id: execution.run_id.clone(),
                    }),
            )
            .collect()
    }
}

/// Plot-scoped facts shown by the inspector beside the activity stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlotInspectorTab {
    Overview,
    Roots,
    Evidence,
    Fruit,
}

impl PlotInspectorTab {
    const ALL: [Self; 4] = [Self::Overview, Self::Roots, Self::Evidence, Self::Fruit];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotExecution {
    pub service_id: String,
    pub repo_id: String,
    pub run_id: String,
    pub manifest_sha256: String,
    pub status: String,
    pub outcome: Option<String>,
    pub event_cursor: String,
    pub evidence: Vec<PlotExecutionEvidence>,
    pub created_at: String,
    pub updated_at: String,
}

/// One opaque Evidence pointer, scoped by its owning Plot execution's
/// service, repository, and run identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotExecutionEvidence {
    pub artifact_kind: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotEstablishment {
    pub event: String,
    pub primary_repository_id: String,
    pub workflow_source_repository_id: String,
    pub workflow_source_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotRepository {
    pub repository_id: String,
    pub root: String,
    pub configuration_root: String,
    pub revision: Option<String>,
    pub roots: Vec<PlotRoot>,
    pub gate_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotRoot {
    pub id: String,
    pub statement: String,
    pub proof_requirements: Vec<PlotProofRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotProofRequirement {
    pub id: String,
    pub stage: String,
    pub required: bool,
    pub gates: Vec<String>,
    pub on_missing: String,
    pub on_failure: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotWorkspace {
    pub workspace_id: String,
    pub repository_id: String,
    pub root: String,
    pub revision: Option<String>,
    pub kind: String,
}

/// One interactive Session belonging to a Plot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotSession {
    pub session_id: String,
    pub mode: String,
    pub host: String,
    pub host_session: String,
    pub host_pane: Option<String>,
    pub state: String,
    pub workspace: Option<String>,
}

/// Availability of one feed source; absent sources degrade, never crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    Ok,
    Unavailable(String),
}

/// Events the app loop reduces into [`App`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEvent {
    /// Full snapshot of Core-owned Plots.
    Plots(Vec<Plot>),
    /// Full snapshot from `nopal ask list --json` (pending asks only).
    Asks(Vec<Ask>),
    /// Full snapshot from `nopal ledger dashboard --json`.
    LedgerRuns(Vec<AfkRun>),
    /// Incremental rondo.core/v1 events for one tracked run.
    RondoRun {
        key: String,
        repo_id: String,
        run_id: String,
        status: Option<String>,
        events: Vec<RunEventRow>,
        evidence: Vec<(String, String)>,
    },
    /// A feed reported (un)availability.
    Source { name: String, status: SourceStatus },
    /// Full snapshot of pane ids whose process tree currently runs the
    /// agent binary, from [`crate::feeds::agents`]. Replaces the prior set
    /// wholesale; a pane's absence means its tree stopped running the
    /// agent, not that the feed forgot it (the poller reports a fresh
    /// snapshot every tick).
    AgentPanes(BTreeSet<String>),
}

/// Sidebar sections, in fixed display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Plots,
    Seats,
    AfkRuns,
    Asks,
}

/// One selectable sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub section: Section,
    /// Seat pane id, run key, or ask id.
    pub key: String,
}

/// Right-click context-menu actions for a seat row. The menu is
/// context-dependent: which subset is offered depends on
/// whether the target seat is currently split into the field window (see
/// [`ContextAction::visible_for`]), so there is no single fixed cursor
/// range any more - [`move_context_cursor`] takes the visible list's length
/// explicitly, and [`ContextAction::at`] resolves a cursor against that
/// same list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextAction {
    Open,
    Kill,
    Relaunch,
    /// `join-pane -h`: move the seat into the field window to the right
    /// of the slot pane. Only offered for a seat not already split in.
    SplitRight,
    /// `join-pane -v`: move the seat into the field window below the
    /// slot pane. Only offered for a seat not already split in.
    SplitBelow,
    /// Mouse equivalent of `f`: for a split-in seat, break it back out to
    /// a real window first (a split has no window of its own to focus);
    /// for an ordinary windowed seat, exactly the existing `f` action.
    /// Offered either way.
    BreakToWindow,
    /// Break a split-in seat back out to a fresh background window without
    /// also focusing it (unlike [`Self::BreakToWindow`]). Only offered for
    /// a seat currently split into the field window - there is nothing
    /// to "return" for a seat that never left its own window.
    Return,
    /// `swap-pane -d`: trade the seat's pane with the current focused-seat
    /// slot occupant, without also taking tmux's active-pane focus there
    /// (unlike [`Self::BreakToWindow`]/`f`) - the sidebar keeps focus, the
    /// seat becomes what is visible next to it. Offered either way: for a
    /// windowed seat it is the ordinary slot swap; for a split-in seat
    /// `swap-pane` still works pane-to-pane (see
    /// [`crate::tmux::Backend::swap_seat_into_slot`]).
    SwapIntoSlot,
    SpawnHere,
}

impl ContextAction {
    /// Every action that exists, in a stable order; not itself a valid
    /// cursor range for any one seat (see [`Self::visible_for`]) - kept for
    /// exhaustiveness in tests and as the canonical declaration order the
    /// two visible-action subsets below are drawn from.
    pub const ALL: [ContextAction; 9] = [
        ContextAction::Open,
        ContextAction::Kill,
        ContextAction::Relaunch,
        ContextAction::SplitRight,
        ContextAction::SplitBelow,
        ContextAction::BreakToWindow,
        ContextAction::Return,
        ContextAction::SwapIntoSlot,
        ContextAction::SpawnHere,
    ];

    /// Actions offered for a seat currently split into the field window:
    /// it cannot be split again, but it can return to its own window.
    const SPLIT_SEAT: [ContextAction; 7] = [
        ContextAction::Open,
        ContextAction::Kill,
        ContextAction::Relaunch,
        ContextAction::BreakToWindow,
        ContextAction::Return,
        ContextAction::SwapIntoSlot,
        ContextAction::SpawnHere,
    ];

    /// Actions offered for an ordinary windowed seat: it can be split in,
    /// but "return to its window" is meaningless since it never left one.
    const WINDOWED_SEAT: [ContextAction; 8] = [
        ContextAction::Open,
        ContextAction::Kill,
        ContextAction::Relaunch,
        ContextAction::SplitRight,
        ContextAction::SplitBelow,
        ContextAction::BreakToWindow,
        ContextAction::SwapIntoSlot,
        ContextAction::SpawnHere,
    ];

    /// The visible action list for a seat, in cursor/display order:
    /// [`Self::SPLIT_SEAT`] when it is currently split into the field
    /// window, [`Self::WINDOWED_SEAT`] otherwise. Pure over the one bit of
    /// state the menu cares about ([`Seat::is_split`]), so it is
    /// unit-testable without a live seat and reusable by both the dispatch
    /// (`app.rs`) and render (`ui.rs`) sides of the menu.
    pub fn visible_for(is_split: bool) -> &'static [ContextAction] {
        if is_split {
            &Self::SPLIT_SEAT
        } else {
            &Self::WINDOWED_SEAT
        }
    }

    /// The action at a menu cursor position within `visible`, clamped to
    /// its last entry. [`move_context_cursor`] is the only thing that moves
    /// the cursor and it already clamps to the same list, so this defends
    /// against nothing today - but it means a future caller can never
    /// panic here, only ever see the last visible action, which is the
    /// safer failure mode for a UI dispatch table. `visible` empty is
    /// unreachable (both [`Self::SPLIT_SEAT`] and [`Self::WINDOWED_SEAT`]
    /// are non-empty), so this simply cannot be called with nothing to
    /// clamp into in practice.
    pub fn at(cursor: usize, visible: &[ContextAction]) -> ContextAction {
        visible[cursor.min(visible.len().saturating_sub(1))]
    }

    pub fn label(self) -> &'static str {
        match self {
            ContextAction::Open => "open",
            ContextAction::Kill => "kill",
            ContextAction::Relaunch => "relaunch",
            ContextAction::SplitRight => "split right",
            ContextAction::SplitBelow => "split below",
            ContextAction::BreakToWindow => "break to window",
            ContextAction::Return => "return to its window",
            ContextAction::SwapIntoSlot => "swap into slot",
            ContextAction::SpawnHere => "spawn here",
        }
    }
}

/// Move the context-menu cursor by `delta`, clamped to `0..len` (the
/// currently visible action list's length - see
/// [`ContextAction::visible_for`], which varies per seat now that the menu
/// is context-dependent). A dropdown menu, not the sidebar's wrapping row
/// list: running past the last action or before the first just stops there
/// instead of wrapping. `len == 0` clamps to `0` rather than underflowing;
/// unreachable in practice since every visible-action list is non-empty.
pub fn move_context_cursor(cursor: usize, delta: i64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let last = (len - 1) as i64;
    (cursor as i64 + delta).clamp(0, last) as usize
}

/// The five regions a row-drag drop resolves against once it crosses onto
/// the embedded panel: the outer band on each edge is a
/// real-split zone, the inner region is "open" - dropping there is the same
/// as clicking the row. [`DropZone::join_flags`] carries the mapping onto
/// `join-pane`'s own `-h`/`-v`/`-b` flags (see
/// [`crate::tmux::Backend::join_seat_split`]); geometry - which zone a
/// screen cell falls in - is a rendering concern, not a state one: see
/// [`crate::ui::drop_zone_at`], the ratatui-`Rect`-aware sibling of this
/// pane-agnostic enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DropZone {
    /// `join-pane`'s flags for a split on this edge, as
    /// `(horizontal, before)`: right = `-h`, left = `-h -b`, below = `-v`,
    /// top = `-v -b` (the design doc's exact mapping). `None` for
    /// [`Self::Center`] - it has no split meaning, it opens/retargets
    /// instead.
    pub fn join_flags(self) -> Option<(bool, bool)> {
        match self {
            DropZone::Center => None,
            DropZone::Left => Some((true, true)),
            DropZone::Right => Some((true, false)),
            DropZone::Top => Some((false, true)),
            DropZone::Bottom => Some((false, false)),
        }
    }

    /// Overlay/status label shown while a row-drag hovers this zone.
    pub fn label(self) -> &'static str {
        match self {
            DropZone::Center => "open",
            DropZone::Left => "split left",
            DropZone::Right => "split right",
            DropZone::Top => "split top",
            DropZone::Bottom => "split below",
        }
    }
}

/// State machine for a sidebar seat-row press-drag-drop gesture. `Armed` is
/// entered on `Down` over a seat row - the click-to-open
/// action moves off `Down` so it can be disambiguated from the start of a
/// drag (see [`resolve_row_drag`]); the first `Drag` event promotes it to
/// `Dragging`, which [`Self::advance`] also refreshes on every later `Drag`
/// with the current hover zone (`None` outside the embedded panel, or
/// whenever no embed is open to drop against at all - the panel is the
/// only drop surface, per the design doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowDrag {
    Armed {
        pane_id: String,
    },
    Dragging {
        pane_id: String,
        hover: Option<DropZone>,
    },
}

impl RowDrag {
    /// The seat pane id carried by either state.
    pub fn pane_id(&self) -> &str {
        match self {
            RowDrag::Armed { pane_id } | RowDrag::Dragging { pane_id, .. } => pane_id,
        }
    }

    /// Advance on a `Drag` event: `Armed` promotes to `Dragging` (entering
    /// row-drag mode proper), an already-`Dragging` state just refreshes its
    /// hover zone. Either way the pane id travels unchanged.
    pub fn advance(self, hover: Option<DropZone>) -> RowDrag {
        RowDrag::Dragging {
            pane_id: self.pane_id().to_owned(),
            hover,
        }
    }
}

/// What a button release resolves a row-drag to, given its state at that
/// moment - the pure decision [`RowDrag::advance`]'s transitions feed into.
/// `Armed` (no `Drag` event ever fired) is a plain click: always
/// [`Self::Open`], matching the pre-pass-2 click-to-open behavior it
/// replaces (now resolved on release instead of press). `Dragging` resolves
/// by its last hover zone: no zone (outside the panel, or no embed open to
/// drop against) cancels; the center zone opens/retargets, same as a click;
/// an edge zone splits, via [`DropZone::join_flags`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDropOutcome {
    Open,
    Split { horizontal: bool, before: bool },
    Cancel,
}

/// Resolve a row-drag's release outcome; pure over [`RowDrag`] - see
/// [`RowDropOutcome`] for what each arm means.
pub fn resolve_row_drag(state: &RowDrag) -> RowDropOutcome {
    match state {
        RowDrag::Armed { .. } => RowDropOutcome::Open,
        RowDrag::Dragging { hover: None, .. } => RowDropOutcome::Cancel,
        RowDrag::Dragging {
            hover: Some(zone), ..
        } => match zone.join_flags() {
            None => RowDropOutcome::Open,
            Some((horizontal, before)) => RowDropOutcome::Split { horizontal, before },
        },
    }
}

/// UI interaction mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Filter input; fuzzy-matches name/repo/status/ticket.
    Filter(String),
    /// Structured event/evidence detail for one run key.
    RunDetail(String),
    /// Spawn-picker: narrows [`App::spawn_candidates`] by substring over
    /// label/path/project; `cursor` indexes the filtered list for Enter to
    /// activate. A query matching nothing that starts with `/` or `~`
    /// spawns at that literal path instead.
    SpawnPicker {
        query: String,
        cursor: usize,
    },
    /// Goto-picker: fuzzy-jumps to an existing seat by name/repo/window
    /// (see [`App::goto_candidates`]) instead of spawning one; `cursor`
    /// indexes the filtered list the same way [`Mode::SpawnPicker`] does.
    /// Only entered from the sidebar-only view (no open embed) - matching
    /// every other overlay-entering key in app.rs - so Enter always opens
    /// a fresh embed rather than ever needing to retarget one.
    GotoPicker {
        query: String,
        cursor: usize,
    },
    /// Collecting a name for a new worktree off `project` (the owning
    /// repo's path, not its candidate label). Enter creates it via
    /// [`crate::seat::worktree::create`] and spawns a seat there.
    WorktreeName {
        project: String,
        buffer: String,
    },
    /// Confirming a kill for the seat at this row key; `y` confirms, any
    /// other key cancels back to [`Mode::Normal`].
    ConfirmKill(String),
    /// Right-click context menu for the seat at this row key: a
    /// context-dependent subset of [`ContextAction`] (see
    /// [`App::context_menu_actions`]) - open / kill / relaunch / spawn-here
    /// always, plus split right / split below / break to window for an
    /// ordinary windowed seat, or break to window / return to its window
    /// for one already split into the field window - navigated with j/k
    /// and dispatched with enter. Only entered from the sidebar-only view,
    /// the same scoping as [`Mode::GotoPicker`] and for the same reason.
    ContextMenu {
        row_key: String,
        cursor: usize,
    },
    /// Full-screen keybinding reference overlay (`?`).
    Help,
}

/// The whole field state.
#[derive(Debug)]
pub struct App {
    /// Core-owned Plots by stable Plot id.
    pub plots: BTreeMap<String, Plot>,
    /// Stable Plot id selected in the Field.
    pub selected_plot_id: Option<String>,
    /// Selected Session or execution in the currently selected Plot.
    pub selected_plot_activity: Option<PlotActivityKey>,
    /// The durable Plot stage can remain open without a live tmux mirror.
    pub stage_open: bool,
    pub inspector_tab: PlotInspectorTab,
    pub main_scroll: usize,
    pub inspector_scroll: usize,
    /// Seats by pane id (BTreeMap for deterministic order).
    pub seats: BTreeMap<String, Seat>,
    /// AFK runs by sidebar key.
    pub runs: BTreeMap<String, AfkRun>,
    pub asks: Vec<Ask>,
    pub sources: BTreeMap<String, SourceStatus>,
    pub mode: Mode,
    /// Key of the selected row, if it still exists.
    pub selected: Option<String>,
    /// The field's own pane id (`$TMUX_PANE`); excluded from seats.
    pub field_pane_id: String,
    /// The field's window id, learned from its own subscription row;
    /// the seat sharing this window is the focused seat.
    pub field_window_id: Option<String>,
    /// The field's session id, learned the same way; panes in other
    /// sessions surface as one seat per session.
    pub field_session_id: Option<String>,
    pub session_name: String,
    /// Render profiling counters (survey benchmark 10).
    pub events_reduced: u64,
    pub frames_rendered: u64,
    pub should_quit: bool,
    /// Set when tmux state may have drifted (window add/close); the app
    /// loop answers by reconciling through the sidecar command channel.
    pub wants_reconcile: bool,
    pub status_line: String,
    /// When false (default), the sidebar shows only nopal-managed sessions
    /// (marker-bearing). `A` toggles the escape hatch that reveals every
    /// session so an unmanaged one can be adopted.
    pub show_all: bool,
    /// The live embedded-seat view, when a seat is opened in the main
    /// panel (Feature 3). Absent means the field is sidebar-only.
    pub embed: Option<crate::embed::Embed>,
    /// True while the help overlay holds a zoom it took itself (opened
    /// from the sidebar-only layout). Never set when an embed owns the
    /// zoom, so dismissing help cannot steal it.
    pub help_zoomed: bool,
    /// Frame geometry recorded by the last render, for mouse hit-testing.
    pub hit: crate::ui::HitMap,
    /// The spawn picker's candidate list, refreshed on entry to
    /// [`Mode::SpawnPicker`] from recents, project roots, and worktrees.
    /// Empty outside the picker.
    pub spawn_candidates: Vec<Candidate>,
    /// Pane ids whose process tree currently runs the agent binary, per
    /// the last [`FeedEvent::AgentPanes`] snapshot. `pane_current_command`
    /// alone is unreliable under shell-integration wrappers (e.g.
    /// kiro-cli-term) that keep the pane's foreground command reporting
    /// the login shell forever even while the agent runs as its
    /// descendant; this set is the process-tree-derived complement the
    /// seat glyph and `s` key fall back to.
    pub agent_panes: BTreeSet<String>,
    /// Session name of a just-spawned/attached seat waiting to be surfaced
    /// in the embedded panel. Spawn never `switch-client`s the operator
    /// away from the field; instead the app
    /// selects the seat and opens its embed as soon as reconcile delivers
    /// it. Cleared on fulfillment; a fresh spawn simply overwrites it.
    pub pending_embed_session: Option<String>,
    /// The embedded grid's last left-press: when it happened and where, in
    /// screen coordinates. crossterm reports individual `Down` events, not
    /// double-clicks, so the next `Down` compares itself against this to
    /// decide (via [`crate::embed::is_double_click`]) whether it starts a
    /// semantic (word) selection instead of a fresh simple one. `None`
    /// outside the embedded view or before any click has happened yet.
    pub embed_last_click: Option<(Instant, u16, u16)>,
    /// True while the Plot inspector is hidden (`z`). The compact Plot rail
    /// remains visible and the live Session receives the reclaimed width.
    pub inspector_collapsed: bool,
    /// The in-flight sidebar seat-row drag gesture, if any:
    /// `Some(Armed)` from `Down` on a seat row until either a `Drag` event
    /// promotes it to `Dragging` or `Up` resolves it (see
    /// [`resolve_row_drag`]); `None` the rest of the time, including
    /// immediately after resolution - a drag never survives its own
    /// release. Esc also clears it (cancel), same as dropping outside the
    /// panel.
    pub row_drag: Option<RowDrag>,
    /// The remappable-keybindings table, parsed once at
    /// startup from the field config's `keys` section. Defaults to every
    /// action at its hardcoded key until `app.rs::run_ui` overwrites it -
    /// [`crate::keys::KeyRegistry::defaults`] is what every test in this
    /// crate that builds an `App` directly gets, which is exactly the
    /// "no config" behavior the dispatch tests assert stays unchanged.
    pub keys: crate::keys::KeyRegistry,
}

impl App {
    pub fn new(field_pane_id: String, session_name: String) -> Self {
        Self {
            plots: BTreeMap::new(),
            selected_plot_id: None,
            selected_plot_activity: None,
            stage_open: false,
            inspector_tab: PlotInspectorTab::Overview,
            main_scroll: 0,
            inspector_scroll: 0,
            seats: BTreeMap::new(),
            runs: BTreeMap::new(),
            asks: Vec::new(),
            sources: BTreeMap::new(),
            mode: Mode::Normal,
            selected: None,
            field_pane_id,
            field_window_id: None,
            field_session_id: None,
            session_name,
            events_reduced: 0,
            frames_rendered: 0,
            should_quit: false,
            wants_reconcile: false,
            status_line: String::new(),
            show_all: false,
            embed: None,
            help_zoomed: false,
            hit: crate::ui::HitMap::default(),
            spawn_candidates: Vec::new(),
            agent_panes: BTreeSet::new(),
            pending_embed_session: None,
            embed_last_click: None,
            inspector_collapsed: false,
            row_drag: None,
            keys: crate::keys::KeyRegistry::defaults(),
        }
    }

    /// Toggle the inspector only while the Plot stage is open. Durable
    /// execution activity remains inspectable without a live tmux embed.
    pub fn toggle_inspector(&mut self) {
        if !self.stage_open {
            return;
        }
        self.inspector_collapsed = !self.inspector_collapsed;
    }

    pub fn selected_plot(&self) -> Option<&Plot> {
        self.selected_plot_id
            .as_ref()
            .and_then(|plot_id| self.plots.get(plot_id))
    }

    /// Move the Plot selection in deterministic id order, wrapping at both
    /// ends. Plot selection is independent from the operational row cursor.
    pub fn move_plot_selection(&mut self, delta: i64) {
        let ids: Vec<String> = self.plots.keys().cloned().collect();
        if ids.is_empty() {
            self.selected_plot_id = None;
            self.set_plot_activity(None, true);
            return;
        }
        let current = self
            .selected_plot_id
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected))
            .unwrap_or(0) as i64;
        let index = (current + delta).rem_euclid(ids.len() as i64) as usize;
        self.selected_plot_id = Some(ids[index].clone());
        self.reconcile_plot_activity(true);
    }

    pub fn select_nth_plot(&mut self, n: usize) -> bool {
        let Some(index) = n.checked_sub(1) else {
            return false;
        };
        let Some(plot_id) = self.plots.keys().nth(index).cloned() else {
            return false;
        };
        self.selected_plot_id = Some(plot_id);
        self.reconcile_plot_activity(true);
        true
    }

    /// Select an activity belonging to the current Plot. Invalid keys are
    /// ignored so mouse hit maps and delayed input cannot cross Plot scope.
    pub fn select_plot_activity(&mut self, key: PlotActivityKey) -> bool {
        let valid = self
            .selected_plot()
            .is_some_and(|plot| plot.activity_keys().contains(&key));
        if !valid {
            return false;
        }
        self.set_plot_activity(Some(key), false);
        true
    }

    /// Move among Session and execution siblings, wrapping at both ends.
    pub fn cycle_plot_activity(&mut self, delta: i64) {
        let Some(keys) = self.selected_plot().map(Plot::activity_keys) else {
            self.set_plot_activity(None, false);
            return;
        };
        if keys.is_empty() {
            self.set_plot_activity(None, false);
            return;
        }
        let current = self
            .selected_plot_activity
            .as_ref()
            .and_then(|selected| keys.iter().position(|key| key == selected))
            .unwrap_or(0) as i64;
        let index = (current + delta).rem_euclid(keys.len() as i64) as usize;
        self.set_plot_activity(Some(keys[index].clone()), false);
    }

    pub fn select_inspector_tab(&mut self, tab: PlotInspectorTab) {
        if self.inspector_tab != tab {
            self.inspector_tab = tab;
            self.inspector_scroll = 0;
        }
    }

    pub fn cycle_inspector_tab(&mut self, delta: i64) {
        let current = PlotInspectorTab::ALL
            .iter()
            .position(|tab| *tab == self.inspector_tab)
            .unwrap_or(0) as i64;
        let index = (current + delta).rem_euclid(PlotInspectorTab::ALL.len() as i64) as usize;
        self.select_inspector_tab(PlotInspectorTab::ALL[index]);
    }

    fn reconcile_plot_activity(&mut self, plot_changed: bool) {
        let current = (!plot_changed)
            .then(|| self.selected_plot_activity.clone())
            .flatten();
        let target = self.selected_plot().and_then(|plot| {
            let keys = plot.activity_keys();
            current
                .filter(|selected| keys.contains(selected))
                .or_else(|| {
                    plot.selected_session_id.as_ref().and_then(|session_id| {
                        let selected = PlotActivityKey::Session(session_id.clone());
                        keys.contains(&selected).then_some(selected)
                    })
                })
                .or_else(|| keys.first().cloned())
        });
        self.set_plot_activity(target, plot_changed);
    }

    fn set_plot_activity(&mut self, activity: Option<PlotActivityKey>, plot_changed: bool) {
        if plot_changed || self.selected_plot_activity != activity {
            self.main_scroll = 0;
            self.inspector_scroll = 0;
        }
        self.selected_plot_activity = activity;
    }

    /// Enter the help overlay. True means the caller should zoom the
    /// field pane so the popup gets the full window instead of the
    /// sidebar strip; an active embed already holds the zoom.
    pub fn enter_help(&mut self) -> bool {
        self.mode = Mode::Help;
        self.help_zoomed = self.embed.is_none() && !self.stage_open;
        self.help_zoomed
    }

    /// Leave the help overlay. True means the caller should release the
    /// zoom help itself took - never one an active embed owns.
    pub fn leave_help(&mut self) -> bool {
        self.mode = Mode::Normal;
        let release = self.help_zoomed && self.embed.is_none() && !self.stage_open;
        self.help_zoomed = false;
        release
    }

    /// Reduce one tmux notification.
    pub fn reduce_tmux(&mut self, notification: &Notification) {
        self.events_reduced += 1;
        match notification {
            Notification::SubscriptionChanged { name, value, .. }
                if name == SEAT_SUBSCRIPTION_NAME =>
            {
                self.apply_seat_line(value);
            }
            Notification::WindowAdd { .. }
            | Notification::WindowPaneChanged { .. }
            | Notification::SessionsChanged => {
                // Option tags set after creation do not re-fire the
                // subscription, and foreign-session pane state never
                // pushes (both verified on 3.6a); ask for a reconcile.
                self.wants_reconcile = true;
            }
            Notification::WindowClose { window_id } => {
                self.seats.retain(|_, seat| &seat.window_id != window_id);
                self.prune_selection();
                self.wants_reconcile = true;
            }
            Notification::WindowRenamed { window_id, name } => {
                for seat in self.seats.values_mut() {
                    if &seat.window_id == window_id {
                        seat.window_name = name.clone();
                    }
                }
            }
            Notification::Exit { reason } => {
                self.status_line = if reason.is_empty() {
                    "tmux control client exited".to_owned()
                } else {
                    format!("tmux control client exited: {reason}")
                };
                self.sources.insert(
                    "tmux".to_owned(),
                    SourceStatus::Unavailable(self.status_line.clone()),
                );
            }
            // A reconcile reply (`list-panes -a`) is a full server
            // snapshot: replace the inventory so vanished panes and
            // closed sessions drop out. Non-seat replies pass through.
            Notification::CommandReply {
                success: true,
                output,
                ..
            } if output.iter().any(|line| parse_seat_line(line).is_some()) => {
                self.seats.clear();
                for line in output {
                    self.apply_seat_line(line);
                }
                self.prune_selection();
            }
            _ => {}
        }
    }

    /// Apply one `SEAT_SUBSCRIPTION_FORMAT` value line (from a subscription
    /// push or a reconcile `list-panes -a` reply).
    fn apply_seat_line(&mut self, value: &str) {
        let Some(seat) = parse_seat_line(value) else {
            return;
        };
        if seat.pane_id == self.field_pane_id
            || (seat.role == "field" && seat.session_name == self.session_name)
        {
            // Our own pane: remember which window and session we live in.
            self.field_window_id = Some(seat.window_id);
            self.field_session_id = Some(seat.session_id);
            self.seats.remove(&seat.pane_id);
            return;
        }
        self.seats.insert(seat.pane_id.clone(), seat);
    }

    /// Reduce one feed event.
    pub fn reduce_feed(&mut self, event: FeedEvent) {
        self.events_reduced += 1;
        match event {
            FeedEvent::Plots(plots) => {
                let previous_plot_id = self.selected_plot_id.clone();
                self.plots = plots
                    .into_iter()
                    .map(|plot| (plot.plot_id.clone(), plot))
                    .collect();
                if self
                    .selected_plot_id
                    .as_ref()
                    .is_none_or(|plot_id| !self.plots.contains_key(plot_id))
                {
                    self.selected_plot_id = self.plots.keys().next().cloned();
                }
                self.reconcile_plot_activity(previous_plot_id != self.selected_plot_id);
            }
            FeedEvent::Asks(asks) => {
                self.asks = asks;
                self.prune_selection();
            }
            FeedEvent::LedgerRuns(runs) => {
                self.runs.retain(|_, run| run.source != RunSource::Ledger);
                for run in runs {
                    self.runs.insert(run.key.clone(), run);
                }
                self.prune_selection();
            }
            FeedEvent::RondoRun {
                key,
                repo_id,
                run_id,
                status,
                events,
                evidence,
            } => {
                let run = self.runs.entry(key.clone()).or_insert_with(|| AfkRun {
                    key,
                    source: RunSource::Rondo,
                    run_id,
                    repo: repo_id,
                    status: "unknown".to_owned(),
                    ticket: String::new(),
                    branch: String::new(),
                    updated_at: String::new(),
                    events: Vec::new(),
                    evidence: Vec::new(),
                    gates: Vec::new(),
                });
                if let Some(status) = status {
                    run.status = status;
                }
                if let Some(last) = events.last() {
                    run.updated_at = last.timestamp.clone();
                }
                run.events.extend(events);
                for pointer in evidence {
                    if !run.evidence.contains(&pointer) {
                        run.evidence.push(pointer);
                    }
                }
            }
            FeedEvent::Source { name, status } => {
                self.sources.insert(name, status);
            }
            FeedEvent::AgentPanes(panes) => {
                self.agent_panes = panes;
            }
        }
    }

    /// The focused-seat slot pane: the one seat sharing the field's
    /// window that is *not* a split-in seat. A split (`join_seat_split`)
    /// shares the field's `window_id` too, so this must exclude it or
    /// `focus_seat`'s swap could target a joined split instead of the true
    /// slot, preserving slot coherence. Exactly one non-split seat lives in
    /// the field window by
    /// construction (`create_session` seeds it, joins only ever add
    /// splits, breaks only ever remove them), so this is unambiguous.
    pub fn focused_seat(&self) -> Option<&Seat> {
        let window = self.field_window_id.as_ref()?;
        self.seats
            .values()
            .find(|seat| &seat.window_id == window && !seat.is_split())
    }

    /// The context menu's visible action list for the seat at `row_key`:
    /// [`ContextAction::visible_for`] resolved against live seat state.
    /// Falls back to the windowed-seat set if the seat vanished out from
    /// under an open menu (e.g. killed externally) - the menu's
    /// any-click/Esc dismissal handles the surprise; this only keeps the
    /// cursor math from ever seeing an empty list.
    pub fn context_menu_actions(&self, row_key: &str) -> &'static [ContextAction] {
        let is_split = self.seats.get(row_key).is_some_and(|seat| seat.is_split());
        ContextAction::visible_for(is_split)
    }

    /// Sidebar seats: every pane in the field's own session (adopted
    /// windows and the slot), plus one seat per foreign session - its
    /// active pane - honoring single-seat-per-session. Sorted
    /// by repo tag then name so sessions group by repo at a glance.
    ///
    /// Default display scope is nopal-managed sessions only: a session is
    /// managed when it carries the `@nopal_managed` marker (nopal spawned
    /// or adopted it) or is the field's own session. `show_all` lifts the
    /// scope so unmanaged sessions can be seen and adopted. The sidecar
    /// still observes every session server-wide (events and the filter
    /// both need it); only the display is scoped.
    pub fn sidebar_seats(&self) -> Vec<&Seat> {
        let field_session = self.field_session_id.as_deref();
        let mut seats: Vec<&Seat> = self
            .seats
            .values()
            .filter(|seat| match field_session {
                Some(session) => {
                    seat.session_id == session || (seat.active && !seat.session_name.is_empty())
                }
                // Session unknown yet (first frames): show active panes.
                None => seat.active,
            })
            .filter(|seat| self.show_all || self.is_managed(seat))
            .collect();
        seats.sort_by(|a, b| {
            (a.repo_tag(), a.display_name(field_session))
                .cmp(&(b.repo_tag(), b.display_name(field_session)))
        });
        seats
    }

    /// Fuzzy-filter sidebar seats for the goto picker (`g`): a
    /// case-insensitive subsequence match (reusing [`matches`], the same
    /// algorithm the `/` sidebar filter uses - no new dependency) over the
    /// seat's display name, repo tag, window name, and session name, the
    /// same haystack [`Self::rows`] already searches. An empty query
    /// returns every seat in sidebar order.
    pub fn goto_candidates(&self, query: &str) -> Vec<&Seat> {
        let field_session = self.field_session_id.as_deref();
        let filter = (!query.is_empty()).then_some(query);
        self.sidebar_seats()
            .into_iter()
            .filter(|seat| {
                let haystack = format!(
                    "{} {} {} {}",
                    seat.display_name(field_session),
                    seat.repo_tag(),
                    seat.window_name,
                    seat.session_name
                );
                matches(filter, &haystack)
            })
            .collect()
    }

    /// A seat counts as nopal-managed when its session carries the marker
    /// or it is the field's own session (always managed by construction).
    pub fn is_managed(&self, seat: &Seat) -> bool {
        seat.managed || self.field_session_id.as_deref() == Some(seat.session_id.as_str())
    }

    /// Optimistically mark every seat in the same session as the given pane
    /// managed, so an adopted session stays visible before the next
    /// reconcile confirms the stamped `@nopal_managed` marker.
    pub fn mark_seat_managed(&mut self, pane_id: &str) {
        let Some(session) = self.seats.get(pane_id).map(|s| s.session_id.clone()) else {
            return;
        };
        for seat in self.seats.values_mut() {
            if seat.session_id == session {
                seat.managed = true;
            }
        }
    }

    /// Pending asks only; resolved/expired asks leave the bar.
    pub fn pending_asks(&self) -> impl Iterator<Item = &Ask> {
        self.asks.iter().filter(|ask| ask.state == "pending")
    }

    /// Narrow [`Self::spawn_candidates`] by a case-insensitive substring
    /// match over label, path, and project; an empty query returns every
    /// candidate in its merged order.
    pub fn filtered_candidates(&self, query: &str) -> Vec<&Candidate> {
        if query.is_empty() {
            return self.spawn_candidates.iter().collect();
        }
        let query = query.to_lowercase();
        self.spawn_candidates
            .iter()
            .filter(|candidate| {
                candidate.label.to_lowercase().contains(&query)
                    || candidate.path.to_lowercase().contains(&query)
                    || candidate.project.to_lowercase().contains(&query)
                    || candidate.project_root.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn filter_text(&self) -> Option<&str> {
        match &self.mode {
            Mode::Filter(text) if !text.is_empty() => Some(text),
            _ => None,
        }
    }

    /// Sidebar rows in display order: SEATS, AFK RUNS, then pending ASKS,
    /// with the filter applied to every section.
    pub fn rows(&self) -> Vec<Row> {
        let filter = self.filter_text();
        let field_session = self.field_session_id.as_deref();
        let mut rows = Vec::new();
        for seat in self.sidebar_seats() {
            let haystack = format!(
                "{} {} {} {} {}",
                seat.display_name(field_session),
                seat.repo_tag(),
                seat.window_name,
                seat.session_name,
                seat.command
            );
            if matches(filter, &haystack) {
                rows.push(Row {
                    section: Section::Seats,
                    key: seat.pane_id.clone(),
                });
            }
        }
        let mut runs: Vec<&AfkRun> = self.runs.values().collect();
        runs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.key.cmp(&b.key)));
        for run in runs {
            let haystack = format!(
                "{} {} {} {} {}",
                run.run_id, run.repo, run.status, run.ticket, run.branch
            );
            if matches(filter, &haystack) {
                rows.push(Row {
                    section: Section::AfkRuns,
                    key: run.key.clone(),
                });
            }
        }
        for ask in self.pending_asks() {
            let haystack = format!("{} {} {}", ask.action, ask.repo, ask.session_id);
            if matches(filter, &haystack) {
                rows.push(Row {
                    section: Section::Asks,
                    key: ask.ask_id.clone(),
                });
            }
        }
        rows
    }

    /// Move the selection by `delta` over the visible rows.
    pub fn move_selection(&mut self, delta: i64) {
        let rows = self.rows();
        if rows.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.key == key));
        let next = match current {
            None => {
                if delta >= 0 {
                    0
                } else {
                    rows.len() - 1
                }
            }
            Some(index) => {
                let len = rows.len() as i64;
                ((index as i64 + delta).rem_euclid(len)) as usize
            }
        };
        self.selected = Some(rows[next].key.clone());
    }

    /// Cycle the selection to the first row of the next (`direction = 1`,
    /// `Tab`) or previous (`direction = -1`, `Shift+Tab`) sidebar section -
    /// Seats, AFK runs, Asks, wrapping - skipping any section with no
    /// visible rows. A no-op when [`next_section`] finds fewer than two
    /// populated sections: with only one section on screen there is
    /// nowhere else to cycle to, and resetting to the top of the section
    /// already being browsed would be a surprising side effect of the
    /// same key.
    pub fn cycle_section(&mut self, direction: i64) {
        let rows = self.rows();
        let current = self.selected_row().map(|row| row.section);
        if let Some(section) = next_section(&rows, current, direction)
            && let Some(row) = rows.iter().find(|row| row.section == section)
        {
            self.selected = Some(row.key.clone());
        }
    }

    /// Jump the selection to the `n`th seat (1-indexed) in the sidebar's
    /// current seat ordering (see [`nth_seat_row`]). Returns whether the
    /// jump landed on a real seat; `n == 0` or past the last visible seat
    /// is a no-op. The caller decides whether to retarget an open embed on
    /// success (the existing j/k retarget path).
    pub fn select_nth_seat(&mut self, n: usize) -> bool {
        let rows = self.rows();
        match nth_seat_row(&rows, n) {
            Some(row) => {
                self.selected = Some(row.key.clone());
                true
            }
            None => false,
        }
    }

    /// The selected row, if it still exists.
    pub fn selected_row(&self) -> Option<Row> {
        let key = self.selected.as_ref()?;
        self.rows().into_iter().find(|row| &row.key == key)
    }

    pub fn prune_selection(&mut self) {
        if self.selected_row().is_none() {
            self.selected = None;
        }
    }
}

/// Parse one `SEAT_SUBSCRIPTION_FORMAT` line into a seat, `@nopal_role`
/// included as [`Seat::role`]. Path is the final field, so `splitn` keeps
/// any `|` inside it intact.
fn parse_seat_line(value: &str) -> Option<Seat> {
    let current: Vec<&str> = value.splitn(SEAT_FIELDS, '|').collect();
    let has_current_identity_shape = current.len() == SEAT_FIELDS
        && (current[13].is_empty() || current[13].starts_with("plot-"))
        && (current[14].is_empty() || current[14].starts_with("session-"));
    let (fields, plot_id, plot_session_id, path_index) = if has_current_identity_shape {
        (current, Some(13usize), Some(14usize), 15usize)
    } else {
        let legacy: Vec<&str> = value.splitn(LEGACY_SEAT_FIELDS, '|').collect();
        if legacy.len() != LEGACY_SEAT_FIELDS {
            return None;
        }
        (legacy, None, None, 13usize)
    };
    if !fields[0].starts_with('%') || !fields[9].starts_with('$') {
        return None;
    }
    Some(Seat {
        pane_id: fields[0].to_owned(),
        window_id: fields[1].to_owned(),
        window_name: fields[2].to_owned(),
        command: fields[3].to_owned(),
        name: fields[4].to_owned(),
        repo: fields[5].to_owned(),
        role: fields[6].to_owned(),
        managed: fields[7] == "1",
        dead: fields[8] == "1",
        session_id: fields[9].to_owned(),
        session_name: fields[10].to_owned(),
        active: fields[11] == "1" && fields[12] == "1",
        plot_id: plot_id
            .and_then(|index| (!fields[index].is_empty()).then(|| fields[index].to_owned())),
        plot_session_id: plot_session_id
            .and_then(|index| (!fields[index].is_empty()).then(|| fields[index].to_owned())),
        path: fields[path_index].to_owned(),
    })
}

/// Sidebar sections in cyclic display order, for [`next_section`].
const SECTION_ORDER: [Section; 3] = [Section::Seats, Section::AfkRuns, Section::Asks];

/// Resolve the section a `Tab` (`direction = 1`) or `Shift+Tab`
/// (`direction = -1`) section-cycle should land on: the next/prev section
/// in [`SECTION_ORDER`] that has any visible rows, skipping empty sections
/// and wrapping around. `current` is the presently selected row's section
/// (`None` when nothing is selected, treated as starting just before the
/// first section so a forward cycle lands on the first populated one).
/// `None` when fewer than two sections have rows - [`App::cycle_section`]
/// defines that as a no-op rather than reselecting the section already in
/// view.
fn next_section(rows: &[Row], current: Option<Section>, direction: i64) -> Option<Section> {
    let populated: Vec<Section> = SECTION_ORDER
        .iter()
        .copied()
        .filter(|section| rows.iter().any(|row| row.section == *section))
        .collect();
    if populated.len() < 2 {
        return None;
    }
    let start = current
        .and_then(|section| SECTION_ORDER.iter().position(|s| *s == section))
        .map(|index| index as i64)
        .unwrap_or(if direction >= 0 { -1 } else { 0 });
    let len = SECTION_ORDER.len() as i64;
    for step in 1..=len {
        let index = (start + step * direction).rem_euclid(len) as usize;
        let section = SECTION_ORDER[index];
        if populated.contains(&section) {
            return Some(section);
        }
    }
    None
}

/// Resolve the 1-indexed seat jump (`1`-`9`): the `n`th row, in display
/// order, among only the Seats section. `None` for `n == 0` or an index
/// past the last visible seat.
fn nth_seat_row(rows: &[Row], n: usize) -> Option<&Row> {
    let index = n.checked_sub(1)?;
    rows.iter()
        .filter(|row| row.section == Section::Seats)
        .nth(index)
}

/// Case-insensitive subsequence match ("fuzzy"): every filter char must
/// appear in order in the haystack.
fn matches(filter: Option<&str>, haystack: &str) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let haystack = haystack.to_lowercase();
    let mut chars = haystack.chars();
    for wanted in filter.to_lowercase().chars() {
        if wanted.is_whitespace() {
            continue;
        }
        if !chars.any(|c| c == wanted) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane line in the field's own session ($1); managed by construction.
    fn seat_line(pane: &str, window: &str, name: &str, seat: &str, repo: &str) -> String {
        format!("{pane}|{window}|{name}|zsh|{seat}|{repo}||1|0|$1|nopal|1|1|/home/nopal/x")
    }

    /// A pane line in a foreign session carrying the `@nopal_managed` marker.
    fn foreign_line(pane: &str, session: &str, sname: &str, active: &str, path: &str) -> String {
        format!("{pane}|@50|editor|nvim||||1|0|{session}|{sname}|{active}|{active}|{path}")
    }

    /// A pane line in an unmanaged foreign session (no marker).
    fn unmanaged_line(pane: &str, session: &str, sname: &str, path: &str) -> String {
        format!("{pane}|@60|shell|zsh||||0|0|{session}|{sname}|1|1|{path}")
    }

    /// A seat pane carrying `@nopal_role=split` (as `join_seat_split` stamps
    /// it), sharing `window` with whatever else is passed the same window -
    /// the field window, in every test that uses this.
    fn split_seat_line(pane: &str, window: &str, name: &str, seat: &str, repo: &str) -> String {
        format!("{pane}|{window}|{name}|zsh|{seat}|{repo}|split|1|0|$1|nopal|1|1|/home/nopal/x")
    }

    fn app() -> App {
        App::new("%1".to_owned(), "nopal".to_owned())
    }

    fn activity_plot(id: &str, selected_session: Option<&str>) -> Plot {
        Plot {
            plot_id: id.to_owned(),
            title: format!("Plot {id}"),
            provisional: false,
            progress: "active".to_owned(),
            conditions: Vec::new(),
            seed_source: "field_open".to_owned(),
            seed_text: String::new(),
            intent: String::new(),
            fruit_state: "absent".to_owned(),
            executions: vec![PlotExecution {
                service_id: "rondo-core".to_owned(),
                repo_id: "repository-1".to_owned(),
                run_id: format!("run-{id}"),
                manifest_sha256: "a".repeat(64),
                status: "running".to_owned(),
                outcome: None,
                event_cursor: "rondo.core/v1:1".to_owned(),
                evidence: Vec::new(),
                created_at: "created".to_owned(),
                updated_at: "updated".to_owned(),
            }],
            sessions: vec![
                PlotSession {
                    session_id: format!("session-{id}-1"),
                    mode: "interactive".to_owned(),
                    host: "pi".to_owned(),
                    host_session: "work".to_owned(),
                    host_pane: Some("%4".to_owned()),
                    state: "active".to_owned(),
                    workspace: None,
                },
                PlotSession {
                    session_id: format!("session-{id}-2"),
                    mode: "interactive".to_owned(),
                    host: "pi".to_owned(),
                    host_session: "work-2".to_owned(),
                    host_pane: Some("%5".to_owned()),
                    state: "active".to_owned(),
                    workspace: None,
                },
            ],
            selected_session_id: selected_session.map(str::to_owned),
            establishment: None,
            repositories: Vec::new(),
            workspaces: Vec::new(),
        }
    }

    fn subscription(value: &str) -> Notification {
        Notification::SubscriptionChanged {
            name: SEAT_SUBSCRIPTION_NAME.to_owned(),
            pane_id: None,
            window_id: None,
            value: value.to_owned(),
        }
    }

    #[test]
    fn seat_subscription_upserts_and_excludes_field_pane() {
        let mut app = app();
        app.reduce_tmux(&subscription(&format!(
            "%1|@1|field|nopal|||{}|1|0|$1|nopal|1|1|/home/nopal/x",
            "field"
        )));
        app.reduce_tmux(&subscription(&seat_line("%2", "@1", "field", "", "")));
        app.reduce_tmux(&subscription(&seat_line(
            "%3",
            "@2",
            "seat:alpha",
            "alpha",
            "rondo",
        )));
        assert_eq!(app.seats.len(), 2);
        assert_eq!(app.field_window_id.as_deref(), Some("@1"));
        assert_eq!(app.field_session_id.as_deref(), Some("$1"));
        assert_eq!(app.seats["%3"].display_name(Some("$1")), "alpha");
        assert_eq!(app.seats["%3"].repo_tag(), "rondo");
        // The seat sharing the field window is focused.
        assert_eq!(app.focused_seat().map(|s| s.pane_id.as_str()), Some("%2"));
    }

    #[test]
    fn a_foreign_field_role_cannot_hijack_this_fields_identity() {
        let mut app = app();
        app.reduce_tmux(&subscription(
            "%1|@1|field|nopal|||field|1|0|$1|nopal|1|1|/home/nopal/x",
        ));
        app.reduce_tmux(&subscription(
            "%9|@9|field|nopal|||field|1|0|$9|other-field|1|1|/home/nopal/y",
        ));

        assert_eq!(app.field_window_id.as_deref(), Some("@1"));
        assert_eq!(app.field_session_id.as_deref(), Some("$1"));
        assert!(app.seats.contains_key("%9"));
    }

    #[test]
    fn seat_subscription_reads_explicit_plot_and_session_identity() {
        let mut app = app();
        app.reduce_tmux(&subscription(
            "%7|@4|seat:work|pi|work|nopal||1|0|$9|nopal-work|1|1|plot-1|session-1|/home/nopal/x",
        ));

        assert_eq!(app.seats["%7"].plot_id.as_deref(), Some("plot-1"));
        assert_eq!(
            app.seats["%7"].plot_session_id.as_deref(),
            Some("session-1")
        );
    }

    #[test]
    fn current_and_legacy_subscriptions_preserve_pipes_in_paths() {
        let current = parse_seat_line(
            "%7|@4|seat:work|pi|work|nopal||1|0|$9|nopal-work|1|1|plot-1|session-1|/home/a|b|c",
        )
        .unwrap();
        assert_eq!(current.path, "/home/a|b|c");
        assert_eq!(current.plot_id.as_deref(), Some("plot-1"));

        let legacy =
            parse_seat_line("%7|@4|seat:work|pi|work|nopal||1|0|$9|nopal-work|1|1|/home/a|b|c")
                .unwrap();
        assert_eq!(legacy.path, "/home/a|b|c");
        assert_eq!(legacy.plot_id, None);
        assert_eq!(legacy.plot_session_id, None);
    }

    #[test]
    fn focused_seat_skips_split_panes_in_the_field_window() {
        let mut app = app();
        app.reduce_tmux(&subscription(&format!(
            "%1|@1|field|nopal|||{}|1|0|$1|nopal|1|1|/home/nopal/x",
            "field"
        )));
        // The true slot: an ordinary (non-split) pane in the field window.
        app.reduce_tmux(&subscription(&seat_line("%2", "@1", "field", "", "")));
        // A seat joined into the SAME window as a split - shares
        // window_id with the slot, which is exactly the ambiguity
        // `focused_seat` must resolve correctly.
        app.reduce_tmux(&subscription(&split_seat_line(
            "%9",
            "@1",
            "seat:alpha",
            "alpha",
            "rondo",
        )));
        assert!(app.seats["%9"].is_split());
        assert!(!app.seats["%2"].is_split());
        assert_eq!(
            app.focused_seat().map(|s| s.pane_id.as_str()),
            Some("%2"),
            "the slot, never the joined split, is the focused seat"
        );
    }

    #[test]
    fn malformed_subscription_values_are_ignored() {
        let mut app = app();
        app.reduce_tmux(&subscription("not-a-pane-line"));
        app.reduce_tmux(&subscription("%9|too|few"));
        assert!(app.seats.is_empty());
    }

    #[test]
    fn window_close_drops_seats_and_selection() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line("%3", "@2", "seat:a", "a", "")));
        app.selected = Some("%3".to_owned());
        app.reduce_tmux(&Notification::WindowClose {
            window_id: "@2".to_owned(),
        });
        assert!(app.seats.is_empty());
        assert_eq!(app.selected, None);
    }

    #[test]
    fn window_add_requests_reconcile() {
        let mut app = app();
        assert!(!app.wants_reconcile);
        app.reduce_tmux(&Notification::WindowAdd {
            window_id: "@9".to_owned(),
        });
        assert!(app.wants_reconcile);
    }

    #[test]
    fn command_reply_is_a_full_snapshot() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line("%9", "@9", "seat:old", "old", "")));
        app.selected = Some("%9".to_owned());
        app.reduce_tmux(&Notification::CommandReply {
            num: 1,
            success: true,
            output: vec![
                seat_line("%4", "@3", "seat:beta", "beta", "memento"),
                "garbage".to_owned(),
            ],
        });
        // The reply replaces the inventory: %9 (gone from the server) drops.
        assert_eq!(app.seats.len(), 1);
        assert_eq!(app.seats["%4"].repo_tag(), "memento");
        assert_eq!(app.selected, None);
    }

    #[test]
    fn command_reply_without_seat_lines_keeps_inventory() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line("%9", "@9", "seat:a", "a", "")));
        app.reduce_tmux(&Notification::CommandReply {
            num: 2,
            success: true,
            output: vec!["some other command output".to_owned()],
        });
        assert_eq!(
            app.seats.len(),
            1,
            "non-reconcile replies must not wipe seats"
        );
    }

    #[test]
    fn foreign_sessions_surface_one_seat_each() {
        let mut app = app();
        // Learn our own session first.
        app.reduce_tmux(&subscription(
            "%1|@1|field|nopal|||field|1|0|$1|nopal|1|1|/home/nopal/x",
        ));
        app.reduce_tmux(&subscription(&seat_line("%2", "@1", "field", "", "")));
        // A foreign session with two panes; only the active one is a seat.
        app.reduce_tmux(&subscription(&foreign_line(
            "%20",
            "$7",
            "teotl",
            "1",
            "/work/teotl",
        )));
        app.reduce_tmux(&subscription(&foreign_line(
            "%21",
            "$7",
            "teotl",
            "0",
            "/work/teotl",
        )));
        // A worktree session groups under the parent repo.
        app.reduce_tmux(&subscription(&foreign_line(
            "%30",
            "$8",
            "nopal-task-15-field-b-prototype",
            "1",
            "/work/teotl/nopal-task-15-field-b-prototype",
        )));
        let seats = app.sidebar_seats();
        let keys: Vec<&str> = seats.iter().map(|s| s.pane_id.as_str()).collect();
        assert!(keys.contains(&"%2"), "own-session pane is a seat");
        assert!(keys.contains(&"%20"), "foreign active pane is a seat");
        assert!(!keys.contains(&"%21"), "foreign inactive pane is not");
        assert!(keys.contains(&"%30"));
        let worktree = seats.iter().find(|s| s.pane_id == "%30").unwrap();
        assert_eq!(worktree.repo_tag(), "teotl", "nopal-* groups under parent");
        assert_eq!(
            worktree.display_name(Some("$1")),
            "nopal-task-15-field-b-prototype",
            "foreign seats display their session name"
        );
    }

    #[test]
    fn unmanaged_sessions_are_hidden_until_show_all() {
        let mut app = app();
        app.reduce_tmux(&subscription(
            "%1|@1|field|nopal|||field|1|0|$1|nopal|1|1|/home/nopal/x",
        ));
        // A managed foreign session and an unmanaged one (a stray sesh
        // session, Vic's `teotl`, etc.).
        app.reduce_tmux(&subscription(&foreign_line(
            "%20",
            "$7",
            "rondo",
            "1",
            "/u/v/rondo",
        )));
        app.reduce_tmux(&subscription(&unmanaged_line(
            "%40",
            "$9",
            "teotl",
            "/u/v/teotl",
        )));

        // Default scope: only the managed foreign session shows.
        let keys: Vec<&str> = app
            .sidebar_seats()
            .iter()
            .map(|s| s.pane_id.as_str())
            .collect();
        assert!(keys.contains(&"%20"), "managed session shows");
        assert!(
            !keys.contains(&"%40"),
            "unmanaged session hidden by default"
        );

        // Escape hatch reveals the unmanaged session.
        app.show_all = true;
        let keys: Vec<&str> = app
            .sidebar_seats()
            .iter()
            .map(|s| s.pane_id.as_str())
            .collect();
        assert!(keys.contains(&"%40"), "show_all reveals unmanaged session");

        // Adoption stamps the marker locally so it stays visible when the
        // escape hatch closes again.
        app.mark_seat_managed("%40");
        app.show_all = false;
        let keys: Vec<&str> = app
            .sidebar_seats()
            .iter()
            .map(|s| s.pane_id.as_str())
            .collect();
        assert!(keys.contains(&"%40"), "adopted session stays visible");
    }

    #[test]
    fn sessions_changed_requests_reconcile() {
        let mut app = app();
        app.reduce_tmux(&Notification::SessionsChanged);
        assert!(app.wants_reconcile);
    }

    #[test]
    fn worktree_repo_tag_collapses_vic_dirs() {
        assert_eq!(worktree_repo_tag("/a/teotl/nopal-task-15-x"), "teotl");
        assert_eq!(worktree_repo_tag("/a/teotl"), "teotl");
        assert_eq!(worktree_repo_tag("/a/teotl/"), "teotl");
        assert_eq!(worktree_repo_tag("nopal-solo"), "nopal-solo");
        assert_eq!(worktree_repo_tag(""), "");
    }

    #[test]
    fn ledger_snapshot_replaces_only_ledger_runs() {
        let mut app = app();
        app.reduce_feed(FeedEvent::RondoRun {
            key: "rondo:r/1".to_owned(),
            repo_id: "r".to_owned(),
            run_id: "1".to_owned(),
            status: Some("running".to_owned()),
            events: vec![],
            evidence: vec![],
        });
        let ledger_run = AfkRun {
            key: "ledger:x".to_owned(),
            source: RunSource::Ledger,
            run_id: "x".to_owned(),
            repo: "nopal".to_owned(),
            status: "running".to_owned(),
            ticket: "TASK-15".to_owned(),
            branch: "main".to_owned(),
            updated_at: "2026-07-06T00:00:00Z".to_owned(),
            events: vec![],
            evidence: vec![],
            gates: vec![],
        };
        app.reduce_feed(FeedEvent::LedgerRuns(vec![ledger_run.clone()]));
        assert_eq!(app.runs.len(), 2);
        app.reduce_feed(FeedEvent::LedgerRuns(vec![]));
        assert_eq!(app.runs.len(), 1, "rondo run must survive ledger refresh");
    }

    #[test]
    fn plot_snapshot_selects_first_and_preserves_a_live_selection() {
        let plot = |id: &str| Plot {
            plot_id: id.to_owned(),
            title: format!("Plot {id}"),
            provisional: true,
            progress: "planned".to_owned(),
            conditions: Vec::new(),
            seed_source: "field_open".to_owned(),
            seed_text: String::new(),
            intent: String::new(),
            fruit_state: "absent".to_owned(),
            executions: Vec::new(),
            sessions: Vec::new(),
            selected_session_id: None,
            establishment: None,
            repositories: Vec::new(),
            workspaces: Vec::new(),
        };
        let mut app = app();

        app.reduce_feed(FeedEvent::Plots(vec![plot("plot-b"), plot("plot-a")]));
        assert_eq!(app.selected_plot_id.as_deref(), Some("plot-a"));

        app.selected_plot_id = Some("plot-b".to_owned());
        app.reduce_feed(FeedEvent::Plots(vec![plot("plot-a"), plot("plot-b")]));
        assert_eq!(app.selected_plot_id.as_deref(), Some("plot-b"));

        app.reduce_feed(FeedEvent::Plots(vec![plot("plot-a")]));
        assert_eq!(app.selected_plot_id.as_deref(), Some("plot-a"));
    }

    #[test]
    fn plot_activities_order_sessions_before_executions() {
        let plot = activity_plot("a", None);
        assert_eq!(
            plot.activity_keys(),
            vec![
                PlotActivityKey::Session("session-a-1".to_owned()),
                PlotActivityKey::Session("session-a-2".to_owned()),
                PlotActivityKey::Execution {
                    service_id: "rondo-core".to_owned(),
                    repo_id: "repository-1".to_owned(),
                    run_id: "run-a".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn activity_selection_uses_core_session_then_ordered_fallbacks() {
        let mut app = app();
        let plot = activity_plot("a", Some("session-a-2"));
        app.reduce_feed(FeedEvent::Plots(vec![plot]));
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a-2".to_owned()))
        );

        let mut plot = activity_plot("a", Some("missing"));
        plot.sessions.remove(0);
        app.selected_plot_activity = Some(PlotActivityKey::Session("vanished".to_owned()));
        app.reduce_feed(FeedEvent::Plots(vec![plot]));
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a-2".to_owned()))
        );

        let mut plot = activity_plot("a", None);
        plot.sessions.clear();
        app.reduce_feed(FeedEvent::Plots(vec![plot]));
        assert!(matches!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Execution { ref run_id, .. }) if run_id == "run-a"
        ));

        let mut plot = activity_plot("a", None);
        plot.sessions.clear();
        plot.executions.clear();
        app.reduce_feed(FeedEvent::Plots(vec![plot]));
        assert_eq!(app.selected_plot_activity, None);
    }

    #[test]
    fn refresh_preserves_valid_activity_and_resets_scroll_only_on_change() {
        let mut app = app();
        app.reduce_feed(FeedEvent::Plots(vec![activity_plot("a", None)]));
        assert!(app.select_plot_activity(PlotActivityKey::Session("session-a-2".to_owned())));
        app.main_scroll = 9;
        app.inspector_scroll = 4;

        app.reduce_feed(FeedEvent::Plots(vec![activity_plot("a", None)]));
        assert_eq!(app.main_scroll, 9);
        assert_eq!(app.inspector_scroll, 4);

        let mut refreshed = activity_plot("a", None);
        refreshed.sessions.pop();
        app.reduce_feed(FeedEvent::Plots(vec![refreshed]));
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a-1".to_owned()))
        );
        assert_eq!(app.main_scroll, 0);
        assert_eq!(app.inspector_scroll, 0);
    }

    #[test]
    fn plot_switch_reconciles_activity_without_cross_plot_bleed() {
        let mut app = app();
        app.reduce_feed(FeedEvent::Plots(vec![
            activity_plot("a", Some("session-a-2")),
            activity_plot("b", Some("session-b-2")),
        ]));
        app.main_scroll = 7;
        app.inspector_scroll = 3;

        app.move_plot_selection(1);
        assert_eq!(app.selected_plot_id.as_deref(), Some("b"));
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-b-2".to_owned()))
        );
        assert_eq!((app.main_scroll, app.inspector_scroll), (0, 0));
    }

    #[test]
    fn cycles_activity_and_inspector_tabs_with_wraparound() {
        let mut app = app();
        app.reduce_feed(FeedEvent::Plots(vec![activity_plot("a", None)]));
        app.cycle_plot_activity(-1);
        assert!(matches!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Execution { .. })
        ));
        app.cycle_plot_activity(1);
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a-1".to_owned()))
        );

        app.cycle_inspector_tab(-1);
        assert_eq!(app.inspector_tab, PlotInspectorTab::Fruit);
        app.select_inspector_tab(PlotInspectorTab::Evidence);
        assert_eq!(app.inspector_tab, PlotInspectorTab::Evidence);
    }

    #[test]
    fn inspector_toggle_depends_on_stage_not_live_embed() {
        let mut app = app();
        app.stage_open = true;
        assert!(app.embed.is_none());
        app.toggle_inspector();
        assert!(app.inspector_collapsed);
        app.stage_open = false;
        app.toggle_inspector();
        assert!(app.inspector_collapsed, "closed stage makes toggle a no-op");
    }

    #[test]
    fn rondo_events_append_and_dedupe_evidence() {
        let mut app = app();
        let event = |sequence, ts: &str| RunEventRow {
            sequence,
            timestamp: ts.to_owned(),
            kind: "rondo.run.status_changed".to_owned(),
            detail: "running".to_owned(),
        };
        app.reduce_feed(FeedEvent::RondoRun {
            key: "rondo:r/1".to_owned(),
            repo_id: "r".to_owned(),
            run_id: "1".to_owned(),
            status: Some("running".to_owned()),
            events: vec![event(1, "t1")],
            evidence: vec![("agent_events".to_owned(), "rondo-run://1/a".to_owned())],
        });
        app.reduce_feed(FeedEvent::RondoRun {
            key: "rondo:r/1".to_owned(),
            repo_id: "r".to_owned(),
            run_id: "1".to_owned(),
            status: Some("completed".to_owned()),
            events: vec![event(2, "t2")],
            evidence: vec![("agent_events".to_owned(), "rondo-run://1/a".to_owned())],
        });
        let run = &app.runs["rondo:r/1"];
        assert_eq!(run.status, "completed");
        assert_eq!(run.events.len(), 2);
        assert_eq!(run.evidence.len(), 1);
        assert_eq!(run.updated_at, "t2");
    }

    #[test]
    fn rows_order_sections_and_pin_pending_asks() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line(
            "%3", "@2", "seat:a", "a", "nopal",
        )));
        app.reduce_feed(FeedEvent::LedgerRuns(vec![AfkRun {
            key: "ledger:x".to_owned(),
            source: RunSource::Ledger,
            run_id: "x".to_owned(),
            repo: "nopal".to_owned(),
            status: "running".to_owned(),
            ticket: String::new(),
            branch: String::new(),
            updated_at: String::new(),
            events: vec![],
            evidence: vec![],
            gates: vec![],
        }]));
        app.reduce_feed(FeedEvent::Asks(vec![
            Ask {
                ask_id: "a1".to_owned(),
                action: "git.push".to_owned(),
                reason: "r".to_owned(),
                session_id: "s".to_owned(),
                repo: "nopal".to_owned(),
                state: "pending".to_owned(),
                created_at: String::new(),
                expires_at: String::new(),
            },
            Ask {
                ask_id: "a2".to_owned(),
                action: "net.fetch".to_owned(),
                reason: "r".to_owned(),
                session_id: "s".to_owned(),
                repo: "nopal".to_owned(),
                state: "approved".to_owned(),
                created_at: String::new(),
                expires_at: String::new(),
            },
        ]));
        let rows = app.rows();
        let sections: Vec<Section> = rows.iter().map(|r| r.section).collect();
        assert_eq!(
            sections,
            vec![Section::Seats, Section::AfkRuns, Section::Asks]
        );
        assert_eq!(rows[2].key, "a1", "resolved asks leave the bar");
    }

    #[test]
    fn filter_narrows_every_section() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line(
            "%3", "@2", "seat:a", "alpha", "nopal",
        )));
        app.reduce_tmux(&subscription(&seat_line(
            "%4", "@3", "seat:b", "beta", "rondo",
        )));
        app.mode = Mode::Filter("rnd".to_owned());
        let rows = app.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "%4");
    }

    #[test]
    fn help_zoom_flag_tracks_enter_and_leave() {
        let mut app = app();
        assert!(app.enter_help(), "sidebar-only help takes a zoom");
        assert_eq!(app.mode, Mode::Help);
        assert!(app.help_zoomed);
        assert!(app.leave_help(), "dismissal releases the zoom it took");
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.help_zoomed);
        // A leave that never zoomed releases nothing.
        assert!(!app.leave_help());
    }

    #[test]
    fn help_does_not_claim_an_execution_only_stages_zoom() {
        let mut app = app();
        app.stage_open = true;

        assert!(!app.enter_help());
        assert!(!app.help_zoomed);
        assert!(!app.leave_help());
        assert!(app.stage_open);
    }

    fn candidate(label: &str, path: &str, project: &str) -> Candidate {
        Candidate {
            label: label.to_owned(),
            path: path.to_owned(),
            project: project.to_owned(),
            project_root: path.to_owned(),
            kind: crate::seat::CandidateKind::ProjectRoot,
        }
    }

    #[test]
    fn filtered_candidates_empty_query_returns_everything() {
        let mut app = app();
        app.spawn_candidates = vec![
            candidate("teotl", "/a/teotl", "teotl"),
            candidate("rondo", "/a/rondo", "rondo"),
        ];
        assert_eq!(app.filtered_candidates("").len(), 2);
    }

    #[test]
    fn filtered_candidates_matches_label_path_or_project_case_insensitively() {
        let mut app = app();
        app.spawn_candidates = vec![
            candidate("teotl", "/a/teotl", "teotl"),
            candidate("nopal-task-38-x", "/a/teotl/nopal-task-38-x", "teotl"),
            candidate("rondo", "/a/rondo", "rondo"),
        ];
        let by_label = app.filtered_candidates("TEOTL");
        assert_eq!(by_label.len(), 2, "matches both teotl-project rows");
        let by_path = app.filtered_candidates("task-38");
        assert_eq!(by_path.len(), 1);
        assert_eq!(by_path[0].label, "nopal-task-38-x");
        assert!(app.filtered_candidates("nope").is_empty());
    }

    #[test]
    fn selection_wraps_and_survives_updates() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line("%3", "@2", "seat:a", "a", "")));
        app.reduce_tmux(&subscription(&seat_line("%4", "@3", "seat:b", "b", "")));
        app.move_selection(1);
        assert_eq!(app.selected.as_deref(), Some("%3"));
        app.move_selection(-1);
        assert_eq!(app.selected.as_deref(), Some("%4"), "wraps backwards");
        app.move_selection(1);
        assert_eq!(app.selected.as_deref(), Some("%3"));
    }

    // --- next_section: Tab/Shift+Tab section-cycle math ---

    fn row(section: Section, key: &str) -> Row {
        Row {
            section,
            key: key.to_owned(),
        }
    }

    #[test]
    fn next_section_skips_empty_sections_and_wraps() {
        let rows = vec![row(Section::Seats, "s1"), row(Section::Asks, "a1")];
        // Seats -> Asks forward, skipping the empty AfkRuns section.
        assert_eq!(
            next_section(&rows, Some(Section::Seats), 1),
            Some(Section::Asks)
        );
        // Asks -> Seats forward wraps around, again skipping AfkRuns.
        assert_eq!(
            next_section(&rows, Some(Section::Asks), 1),
            Some(Section::Seats)
        );
        // Reversing from Seats wraps back to Asks.
        assert_eq!(
            next_section(&rows, Some(Section::Seats), -1),
            Some(Section::Asks)
        );
    }

    #[test]
    fn next_section_starts_at_the_first_populated_section_with_nothing_selected() {
        let rows = vec![row(Section::AfkRuns, "r1"), row(Section::Asks, "a1")];
        assert_eq!(next_section(&rows, None, 1), Some(Section::AfkRuns));
        // Backward with nothing selected starts just after the last
        // section, so it lands on the last populated one.
        assert_eq!(next_section(&rows, None, -1), Some(Section::Asks));
    }

    #[test]
    fn next_section_is_a_no_op_with_fewer_than_two_populated_sections() {
        let rows = vec![row(Section::Seats, "s1"), row(Section::Seats, "s2")];
        assert_eq!(next_section(&rows, Some(Section::Seats), 1), None);
        assert_eq!(next_section(&[], None, 1), None);
    }

    #[test]
    fn cycle_section_moves_to_the_first_row_of_the_target_section() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line("%3", "@2", "seat:a", "a", "")));
        app.reduce_feed(FeedEvent::Asks(vec![Ask {
            ask_id: "a1".to_owned(),
            action: "git.push".to_owned(),
            reason: "r".to_owned(),
            session_id: "s".to_owned(),
            repo: "nopal".to_owned(),
            state: "pending".to_owned(),
            created_at: String::new(),
            expires_at: String::new(),
        }]));
        app.selected = Some("%3".to_owned());
        app.cycle_section(1);
        assert_eq!(app.selected.as_deref(), Some("a1"), "Seats -> Asks");
        app.cycle_section(1);
        assert_eq!(app.selected.as_deref(), Some("%3"), "wraps back to Seats");
    }

    // --- nth_seat_row / select_nth_seat: 1-9 seat jump ---

    #[test]
    fn nth_seat_row_is_1_indexed_over_seats_only() {
        let rows = vec![
            row(Section::Seats, "s1"),
            row(Section::AfkRuns, "r1"),
            row(Section::Seats, "s2"),
        ];
        assert_eq!(nth_seat_row(&rows, 1).map(|r| r.key.as_str()), Some("s1"));
        assert_eq!(nth_seat_row(&rows, 2).map(|r| r.key.as_str()), Some("s2"));
        assert!(nth_seat_row(&rows, 0).is_none(), "n=0 is out of range");
        assert!(nth_seat_row(&rows, 3).is_none(), "past the last seat");
    }

    #[test]
    fn select_nth_seat_updates_selection_and_reports_success() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line("%3", "@2", "seat:a", "a", "")));
        app.reduce_tmux(&subscription(&seat_line("%4", "@3", "seat:b", "b", "")));
        assert!(app.select_nth_seat(2));
        assert_eq!(app.selected.as_deref(), Some("%4"));
        assert!(!app.select_nth_seat(9), "out of range is a no-op");
        assert_eq!(app.selected.as_deref(), Some("%4"), "selection unchanged");
    }

    // --- ContextAction / move_context_cursor: context-menu dispatch table ---

    #[test]
    fn move_context_cursor_clamps_instead_of_wrapping() {
        let len = ContextAction::WINDOWED_SEAT.len();
        assert_eq!(move_context_cursor(0, -1, len), 0, "stops before the first");
        assert_eq!(
            move_context_cursor(len - 1, 1, len),
            len - 1,
            "stops after the last"
        );
        assert_eq!(move_context_cursor(1, 1, len), 2);
    }

    #[test]
    fn move_context_cursor_clamps_to_a_shorter_visible_list() {
        // The split-seat menu is shorter than the windowed one; a cursor
        // that would be valid there must still clamp to the shorter list's
        // bounds rather than assuming `ContextAction::ALL`'s length.
        let len = ContextAction::SPLIT_SEAT.len();
        assert_eq!(move_context_cursor(len - 1, 1, len), len - 1);
        assert_eq!(move_context_cursor(0, -1, len), 0);
    }

    #[test]
    fn context_action_at_resolves_every_cursor_position() {
        let visible = ContextAction::visible_for(false);
        assert_eq!(ContextAction::at(0, visible), ContextAction::Open);
        assert_eq!(ContextAction::at(1, visible), ContextAction::Kill);
        assert_eq!(ContextAction::at(2, visible), ContextAction::Relaunch);
        assert_eq!(
            ContextAction::at(visible.len() - 1, visible),
            ContextAction::SpawnHere,
            "spawn-here is always the last entry"
        );
        assert_eq!(
            ContextAction::at(99, visible),
            ContextAction::SpawnHere,
            "an out-of-range cursor clamps to the last visible action"
        );
    }

    #[test]
    fn visible_for_splits_the_action_set_by_seat_state() {
        let windowed = ContextAction::visible_for(false);
        let split = ContextAction::visible_for(true);
        assert!(windowed.contains(&ContextAction::SplitRight));
        assert!(windowed.contains(&ContextAction::SplitBelow));
        assert!(
            !windowed.contains(&ContextAction::Return),
            "an ordinary seat never left a window to return to"
        );
        assert!(split.contains(&ContextAction::Return));
        assert!(
            !split.contains(&ContextAction::SplitRight),
            "an already-split seat cannot be split again"
        );
        assert!(
            !split.contains(&ContextAction::SplitBelow),
            "an already-split seat cannot be split again"
        );
        // Both offer the seat-agnostic actions.
        for action in [
            ContextAction::Open,
            ContextAction::Kill,
            ContextAction::Relaunch,
            ContextAction::BreakToWindow,
            ContextAction::SwapIntoSlot,
            ContextAction::SpawnHere,
        ] {
            assert!(
                windowed.contains(&action),
                "{action:?} missing from windowed set"
            );
            assert!(split.contains(&action), "{action:?} missing from split set");
        }
    }

    #[test]
    fn context_menu_actions_reflects_live_seat_state() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line(
            "%3", "@2", "seat:a", "alpha", "nopal",
        )));
        assert_eq!(
            app.context_menu_actions("%3"),
            ContextAction::visible_for(false)
        );
        app.seats.get_mut("%3").unwrap().role = "split".to_owned();
        assert_eq!(
            app.context_menu_actions("%3"),
            ContextAction::visible_for(true)
        );
        // A vanished seat falls back to the windowed set rather than
        // panicking or returning nothing.
        assert_eq!(
            app.context_menu_actions("%999"),
            ContextAction::visible_for(false)
        );
    }

    // --- goto_candidates: fuzzy seat filter for the `g` picker ---

    #[test]
    fn goto_candidates_fuzzy_filters_by_name_or_repo() {
        let mut app = app();
        app.reduce_tmux(&subscription(&seat_line(
            "%3", "@2", "seat:a", "alpha", "nopal",
        )));
        app.reduce_tmux(&subscription(&seat_line(
            "%4", "@3", "seat:b", "beta", "rondo",
        )));
        let by_name = app.goto_candidates("alph");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].pane_id, "%3");
        let by_repo = app.goto_candidates("rnd");
        assert_eq!(by_repo.len(), 1);
        assert_eq!(by_repo[0].pane_id, "%4");
        assert_eq!(app.goto_candidates("").len(), 2, "empty query keeps all");
        assert!(app.goto_candidates("zzz").is_empty());
    }

    // --- inspector collapse: z only applies with a live Session open ---

    #[test]
    fn toggle_inspector_is_a_no_op_without_an_embed() {
        let mut app = app();
        app.toggle_inspector();
        assert!(!app.inspector_collapsed, "no inspector is visible");
    }

    // --- DropZone / RowDrag / resolve_row_drag: row-drag drop-zone state
    // machine ---

    #[test]
    fn drop_zone_join_flags_match_the_design_docs_mapping() {
        assert_eq!(DropZone::Right.join_flags(), Some((true, false)));
        assert_eq!(DropZone::Left.join_flags(), Some((true, true)));
        assert_eq!(DropZone::Bottom.join_flags(), Some((false, false)));
        assert_eq!(DropZone::Top.join_flags(), Some((false, true)));
        assert_eq!(DropZone::Center.join_flags(), None);
    }

    #[test]
    fn row_drag_advance_promotes_armed_to_dragging_and_refreshes_hover() {
        let armed = RowDrag::Armed {
            pane_id: "%3".to_owned(),
        };
        let dragging = armed.advance(Some(DropZone::Right));
        assert_eq!(
            dragging,
            RowDrag::Dragging {
                pane_id: "%3".to_owned(),
                hover: Some(DropZone::Right),
            }
        );
        let redragging = dragging.advance(None);
        assert_eq!(
            redragging,
            RowDrag::Dragging {
                pane_id: "%3".to_owned(),
                hover: None,
            },
            "a later Drag event refreshes the hover zone; pane id unchanged"
        );
    }

    #[test]
    fn row_drag_pane_id_is_stable_across_states() {
        assert_eq!(
            RowDrag::Armed {
                pane_id: "%9".to_owned()
            }
            .pane_id(),
            "%9"
        );
        assert_eq!(
            RowDrag::Dragging {
                pane_id: "%9".to_owned(),
                hover: None,
            }
            .pane_id(),
            "%9"
        );
    }

    #[test]
    fn resolve_row_drag_armed_always_opens() {
        let state = RowDrag::Armed {
            pane_id: "%3".to_owned(),
        };
        assert_eq!(
            resolve_row_drag(&state),
            RowDropOutcome::Open,
            "a plain click (no drag) opens"
        );
    }

    #[test]
    fn resolve_row_drag_dragging_resolves_by_hover_zone() {
        let dragging = |hover| RowDrag::Dragging {
            pane_id: "%3".to_owned(),
            hover,
        };
        assert_eq!(
            resolve_row_drag(&dragging(None)),
            RowDropOutcome::Cancel,
            "outside the panel (or no embed to drop against) cancels"
        );
        assert_eq!(
            resolve_row_drag(&dragging(Some(DropZone::Center))),
            RowDropOutcome::Open
        );
        assert_eq!(
            resolve_row_drag(&dragging(Some(DropZone::Left))),
            RowDropOutcome::Split {
                horizontal: true,
                before: true
            }
        );
        assert_eq!(
            resolve_row_drag(&dragging(Some(DropZone::Bottom))),
            RowDropOutcome::Split {
                horizontal: false,
                before: false
            }
        );
    }
}
