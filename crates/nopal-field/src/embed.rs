//! Embedded-seat view: a Herdr-class VT panel rendered inside the field.
//!
//! The seat stays a real tmux pane in its own session; this module only
//! *mirrors* it. We backfill with `capture-pane -e -p -S -N` (colors/attrs
//! preserved, `N` lines of tmux scrollback so the mirror is scrollable from
//! the first frame), then stream live pane output through a per-pane
//! `pipe-pane -O` into a fifo - keeping the output firehose out of the
//! `-f no-output` sidecar. Bytes are parsed by `alacritty_terminal` (an
//! Apache-2.0 VT engine; the technique is ported from herdr, never its
//! AGPL source) into a `Term` grid that [`crate::ui`] renders to ratatui,
//! scrollback and all. Input, while the panel holds focus,
//! is re-encoded to the exact bytes a terminal would send and delivered
//! with `send-keys -H`; the mouse wheel and drag-select reuse the same
//! `send-keys -H` path when the seat itself needs the bytes (mouse
//! reporting on). The seat's real geometry is never touched: we size our
//! own grid to the pane's reported width/height and clip or center it into
//! whatever space we have. `/` over the mirror with view focus (not seat
//! input focus) opens a scrollback search ([`EmbedSearch`])
//! built on alacritty's own `term::search::RegexSearch` - already in the
//! dependency tree, no new crate.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi;

use crate::AppEvent;

/// Scrollback depth for the embedded mirror, in lines. Generous enough to
/// browse a good chunk of a seat's history; cheap enough that the open-time
/// backfill (`capture-pane -S -N`) and the grid's own memory stay bounded.
/// tmux itself clamps `-S` to whatever history a pane actually has, so this
/// is a ceiling, not a promise every pane grows to it.
const SCROLLBACK_LINES: usize = 5000;

/// Lines scrolled per wheel notch, matching herdr's documented
/// `mouse_scroll_lines = 3` default (the plan this pass implements pins to
/// that convention rather than inventing a new one).
pub const WHEEL_SCROLL_LINES: i32 = 3;

/// Double-click window: a second left-press within this long, on the same
/// or an adjacent cell, is treated as a double-click (token-select) rather
/// than the start of a fresh single-cell selection.
pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// A fixed-size grid for the embedded terminal, plus its scrollback depth.
/// `Term::new` only reads [`Dimensions::columns`] and
/// [`Dimensions::screen_lines`] from this (the real scrollback capacity
/// comes from `Config::scrolling_history`, consulted directly); we still
/// report an honest `total_lines` since `Dimensions` is a public trait other
/// alacritty code paths call generically.
#[derive(Debug, Clone, Copy)]
struct GridSize {
    columns: usize,
    screen_lines: usize,
    scrollback: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines + self.scrollback
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// One live embedded seat: the parsed VT grid plus the pipe reader feeding
/// it. Dropping it stops the pipe and removes the fifo.
pub struct Embed {
    /// The mirrored seat's pane id (`%NN`).
    pub pane_id: String,
    /// Human label for the panel header.
    pub label: String,
    /// True while keystrokes route to the seat via `send-keys -H`.
    pub input_focus: bool,
    /// Grid dimensions, snapshotted from the seat at attach time.
    pub cols: u16,
    pub rows: u16,
    term: Term<VoidListener>,
    parser: ansi::Processor,
    fifo_path: std::path::PathBuf,
    reader: Option<JoinHandle<()>>,
    /// The fixed anchor point of an in-progress mouse selection (where the
    /// button went down), tracked separately from alacritty's own
    /// `Selection`. Alacritty derives a point's `Side` (which half of its
    /// cell was struck) from sub-cell mouse pixel data; crossterm only ever
    /// gives us whole terminal cells, so we cannot recompute that per
    /// update. Instead [`Self::update_selection`] rebuilds the `Selection`
    /// on every drag step, always putting whichever of {anchor, current
    /// point} sorts first at `Side::Left` (included) and the other at
    /// `Side::Right` (included) - the one assignment alacritty's own
    /// boundary-trim logic (`Selection::range_simple`) treats as fully
    /// inclusive on both ends, so a drag that reverses direction mid-flight
    /// stays character-exact instead of drifting by a cell.
    selection_anchor: Option<Point>,
    /// The mirror's scrollback search (`/`), when one is
    /// open. Lives here rather than in [`crate::state::Mode`] for two
    /// reasons: alacritty's `RegexSearch` is not `Eq` (`Mode` derives it),
    /// and a search is inherently per-mirror - retargeting or closing the
    /// embed always drops or replaces the whole `Embed`, which clears any
    /// in-flight search for free instead of needing an explicit reset.
    pub search: Option<EmbedSearch>,
}

impl fmt::Debug for Embed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Embed")
            .field("pane_id", &self.pane_id)
            .field("input_focus", &self.input_focus)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl Embed {
    #[cfg(test)]
    pub(crate) fn test_for_app(pane_id: &str, label: &str, input_focus: bool) -> Self {
        Self::test_for_app_with_size(pane_id, label, input_focus, 80, 24)
    }

    #[cfg(test)]
    pub(crate) fn test_for_app_with_size(
        pane_id: &str,
        label: &str,
        input_focus: bool,
        cols: u16,
        rows: u16,
    ) -> Self {
        let dims = GridSize {
            columns: cols as usize,
            screen_lines: rows as usize,
            scrollback: SCROLLBACK_LINES,
        };
        let term = Term::new(
            Config {
                scrolling_history: SCROLLBACK_LINES,
                ..Default::default()
            },
            &dims,
            VoidListener,
        );
        Self {
            pane_id: pane_id.to_owned(),
            label: label.to_owned(),
            input_focus,
            cols,
            rows,
            term,
            parser: ansi::Processor::new(),
            fifo_path: std::env::temp_dir().join("nopal-field-test-unused.fifo"),
            reader: None,
            selection_anchor: None,
            search: None,
        }
    }

    /// Attach to `pane_id`: snapshot its size, build a grid, backfill the
    /// current screen, and start streaming live output into it. The caller
    /// owns window geometry (zoom); we never resize the seat.
    pub fn open(pane_id: &str, label: &str, tx: Sender<AppEvent>) -> io::Result<Self> {
        let (cols, rows) = pane_size(pane_id)?;
        let dims = GridSize {
            columns: cols as usize,
            screen_lines: rows as usize,
            scrollback: SCROLLBACK_LINES,
        };
        let mut term = Term::new(
            Config {
                scrolling_history: SCROLLBACK_LINES,
                ..Default::default()
            },
            &dims,
            VoidListener,
        );
        let mut parser = ansi::Processor::new();

        // Backfill: reproduce the pane's tmux scrollback plus its current
        // screen before the live stream arrives, so the mirror is
        // scrollable immediately rather than only after new output pushes
        // lines into history. capture-pane emits one physical line per row;
        // a bare LF would keep the column, so re-home and CR/LF-join the
        // rows. Feeding this through the same VT parser as live bytes is
        // what actually builds alacritty's scrollback (there is no direct
        // "seed history" API) - display_offset ends at 0 either way, so the
        // panel opens showing the live tail, not scrolled back.
        if let Ok(screen) = capture_pane(pane_id, SCROLLBACK_LINES) {
            parser.advance(&mut term, b"\x1b[H\x1b[2J");
            let mut first = true;
            for line in screen.split(|&b| b == b'\n') {
                if !first {
                    parser.advance(&mut term, b"\r\n");
                }
                parser.advance(&mut term, line);
                first = false;
            }
        }

        let fifo_path = fifo_path_for(pane_id);
        make_fifo(&fifo_path)?;
        start_pipe(pane_id, &fifo_path).inspect_err(|_| {
            let _ = std::fs::remove_file(&fifo_path);
        })?;

        let reader = spawn_reader(pane_id.to_owned(), fifo_path.clone(), tx);

        Ok(Self {
            pane_id: pane_id.to_owned(),
            label: label.to_owned(),
            input_focus: false,
            cols,
            rows,
            term,
            parser,
            fifo_path,
            reader: Some(reader),
            selection_anchor: None,
            search: None,
        })
    }

    /// Feed live pane bytes into the VT grid.
    pub fn advance(&mut self, data: &[u8]) {
        self.parser.advance(&mut self.term, data);
    }

    /// The parsed terminal, for rendering.
    pub fn term(&self) -> &Term<VoidListener> {
        &self.term
    }

    /// Whether the seat program has turned on mouse reporting - any of
    /// click (DECSET 1000), cell-motion (1002), or all-motion (1003)
    /// tracking. When set, the wheel is forwarded to the seat as raw mouse
    /// bytes instead of moving the local scrollback view: alt-screen TUIs
    /// like vim/less expect to see and handle the wheel themselves (herdr's
    /// documented rule - forwarding a scroll to an app that already redraws
    /// its own viewport on wheel input would double-move it).
    ///
    /// tmux's `mouse_any_flag` is the source of truth (see
    /// [`pane_mouse_flags`]): the seat almost always enabled mouse mode
    /// before this mirror attached, so our own `Term` - which only saw the
    /// text backfill plus post-attach bytes - would miss it. We fall back to
    /// the `Term` mode only when tmux can't be reached, which keeps the
    /// pure-logic path testable without a live pane. The `Term` check must
    /// be `intersects`, not `contains`: `contains` would demand all three
    /// mouse bits at once, but a real app typically sets exactly one.
    pub fn mouse_reporting(&self) -> bool {
        match pane_mouse_flags(&self.pane_id) {
            Some((any, _)) => any,
            None => self.term.mode().intersects(TermMode::MOUSE_MODE),
        }
    }

    /// Move the local scrollback view. Positive `lines` scrolls toward
    /// history (up/older); negative moves back toward the live tail.
    /// `Grid::scroll_display` clamps at both ends internally, so no extra
    /// bounds-checking belongs here.
    pub fn scroll_lines(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Forward one wheel notch to the seat as the exact bytes its own
    /// terminal would have sent, via `send-keys -H` - used only while
    /// [`Self::mouse_reporting`] is true. `local_col`/`local_row` are
    /// 0-based coordinates within the seat's own grid (see
    /// [`screen_to_local`]).
    pub fn send_wheel(&self, local_col: u16, local_row: u16, up: bool) -> io::Result<()> {
        // Prefer tmux's view of SGR mode for the same reason as
        // `mouse_reporting`: DECSET 1006 is usually negotiated before attach.
        let sgr = match pane_mouse_flags(&self.pane_id) {
            Some((_, sgr)) => sgr,
            None => self.term.mode().contains(TermMode::SGR_MOUSE),
        };
        let bytes = encode_wheel_mouse(sgr, up, local_col, local_row);
        send_hex(&self.pane_id, &hex_tokens(&bytes))
    }

    /// Begin a new selection anchored at a screen cell, replacing any
    /// previous one (a plain click that never drags leaves the anchor
    /// equal to itself, which alacritty's own `Selection::is_empty` then
    /// reports as nothing to copy - see [`Self::selection_text`]).
    /// `double` requests alacritty's semantic (word/token) expansion
    /// instead of a precise cell-by-cell selection.
    pub fn begin_selection(&mut self, local_col: u16, local_row: u16, double: bool) {
        let point = self.grid_point(local_col, local_row);
        let ty = if double {
            SelectionType::Semantic
        } else {
            SelectionType::Simple
        };
        self.selection_anchor = Some(point);
        self.term.selection = Some(Selection::new(ty, point, Side::Left));
    }

    /// Extend the active selection to a new screen cell (mouse drag). A
    /// no-op if nothing is selected yet. Rebuilds the `Selection` from the
    /// stored anchor rather than mutating it in place - see
    /// [`Self::selection_anchor`] for why that is the only way to keep a
    /// direction-reversing drag character-exact without sub-cell mouse
    /// data.
    pub fn update_selection(&mut self, local_col: u16, local_row: u16) {
        let Some(anchor) = self.selection_anchor else {
            return;
        };
        let point = self.grid_point(local_col, local_row);
        let ty = self
            .term
            .selection
            .as_ref()
            .map(|s| s.ty)
            .unwrap_or(SelectionType::Simple);
        // Whichever of {anchor, point} sorts first (Point's Ord compares
        // line then column) is the selection's geometric start; it takes
        // Side::Left, the other Side::Right - the assignment
        // `Selection::range_simple` treats as fully inclusive on both ends
        // regardless of which one is the fixed click and which is the
        // moving pointer.
        let (start, end) = if point >= anchor {
            (anchor, point)
        } else {
            (point, anchor)
        };
        let mut selection = Selection::new(ty, start, Side::Left);
        selection.update(end, Side::Right);
        self.term.selection = Some(selection);
    }

    /// The active selection's text, or `None` when there is no selection or
    /// it is empty (a plain click with no drag and no double-click).
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Map a screen cell to the seat's own grid coordinates. Line 0 is the
    /// top of the live screen; scrolled-back rows are negative - the same
    /// coordinate space [`crate::ui::draw_embed`] reads off
    /// `renderable_content().display_iter`, shifted by the current
    /// scrollback [`alacritty_terminal::grid::Grid::display_offset`].
    fn grid_point(&self, local_col: u16, local_row: u16) -> Point {
        let display_offset = self.term.grid().display_offset() as i32;
        Point::new(
            Line(local_row as i32 - display_offset),
            Column(local_col as usize),
        )
    }

    // --- scrollback search (`/`) ---

    /// Open the search prompt, remembering the current scroll position so
    /// [`Self::close_search`] can restore it - whether the prompt is
    /// cancelled outright or an active search is later abandoned, Esc
    /// always lands back exactly where the operator was.
    pub fn start_search(&mut self) {
        self.search = Some(EmbedSearch::Prompt {
            query: String::new(),
            error: None,
            pre_offset: self.term.grid().display_offset(),
        });
    }

    /// Append one character to the query while composing the prompt. A
    /// no-op once search is [`EmbedSearch::Active`] - `n`/`N` own the
    /// keyboard then, not text entry (`app.rs` never routes a char key
    /// there, but this stays a safe no-op regardless of caller discipline).
    pub fn search_push(&mut self, c: char) {
        if let Some(EmbedSearch::Prompt { query, error, .. }) = &mut self.search {
            query.push(c);
            *error = None;
        }
    }

    /// Delete the last character of the query while composing the prompt.
    pub fn search_backspace(&mut self) {
        if let Some(EmbedSearch::Prompt { query, error, .. }) = &mut self.search {
            query.pop();
            *error = None;
        }
    }

    /// Execute the prompt (Enter): compile the query - see
    /// [`wrap_search_pattern`] for the case-insensitive-by-default rule -
    /// find the nearest match at-or-above the current view, and jump there
    /// (see [`center_offset_for_line`] for the exact landing rule: the
    /// match is vertically centered, not merely scrolled into frame, so
    /// there is context on both sides). An empty query cancels exactly
    /// like Esc. An invalid pattern or a query with no matches anywhere in
    /// the buffer reports the error on the prompt and stays there, so the
    /// operator can retype without losing the pre-search anchor -
    /// [`Self::search_status`] surfaces it.
    pub fn execute_search(&mut self) {
        let Some(EmbedSearch::Prompt {
            query, pre_offset, ..
        }) = &self.search
        else {
            return;
        };
        if query.is_empty() {
            self.close_search();
            return;
        }
        let query = query.clone();
        let pre_offset = *pre_offset;
        let pattern = wrap_search_pattern(&query);
        // Boxed: `RegexSearch` carries four DFA+cache pairs and is large
        // enough that `EmbedSearch::Active` next to `EmbedSearch::Prompt`
        // (a couple of `String`/`Option<String>` fields) trips clippy's
        // `large_enum_variant` - indirection here is cheap (one search per
        // keystroke of `n`/`N`, not per frame) and keeps every `EmbedSearch`
        // the size of its smallest variant.
        // `RegexSearch::new`'s error is a raw `regex_automata::BuildError`
        // Debug dump (e.g. `BuildError { kind: NFA(BuildErr...`) - useless
        // to an operator who just fat-fingered a bracket. A short, stable
        // message is more useful than the exact failure kind here: the fix
        // is always "edit the query," never something the error detail
        // would help diagnose.
        let mut regex = match RegexSearch::new(&pattern) {
            Ok(regex) => Box::new(regex),
            Err(_) => {
                self.set_prompt_error("invalid regex".to_owned());
                return;
            }
        };
        // Search backward (toward history) from the top of the current
        // viewport - the same direction `n` travels, so Enter is simply
        // the first step of that walk. `search_next` with no `max_lines`
        // bound scans the *entire* buffer in one call and wraps at the
        // ends on its own (see the module doc on `RegexIter`), so "no
        // match anywhere" is the only way this comes back empty.
        let origin = self.grid_point(0, 0);
        let Some(current) =
            self.term
                .search_next(&mut regex, origin, Direction::Left, Side::Left, None)
        else {
            self.set_prompt_error("no matches".to_owned());
            return;
        };
        self.jump_to_match(&current);
        let visible = self.visible_matches(&mut regex);
        self.search = Some(EmbedSearch::Active {
            query,
            regex,
            current,
            visible,
            pre_offset,
        });
    }

    fn set_prompt_error(&mut self, message: String) {
        if let Some(EmbedSearch::Prompt { error, .. }) = &mut self.search {
            *error = Some(message);
        }
    }

    /// Cycle to the next (`forward = true`, `n`, toward older history) or
    /// previous (`forward = false`, `N`, toward the live tail) match. The
    /// wraparound at both ends is alacritty's own `search_next` behavior
    /// (an unbounded call scans the whole buffer), not logic reimplemented
    /// here - see [`step_past_match`] for the one piece of arithmetic this
    /// does own: nudging the origin past the current match so the same
    /// occurrence is never rematched. A no-op outside
    /// [`EmbedSearch::Active`], and - defensively - if the buffer changed
    /// out from under the compiled pattern so thoroughly that even the
    /// current match's own neighborhood no longer resolves (search stays
    /// on the match it already had rather than losing its place).
    pub fn search_advance(&mut self, forward: bool) {
        let Some(EmbedSearch::Active {
            query,
            mut regex,
            current,
            visible,
            pre_offset,
        }) = self.search.take()
        else {
            return;
        };
        let direction = if forward {
            Direction::Left
        } else {
            Direction::Right
        };
        let side = if forward { Side::Left } else { Side::Right };
        let origin = step_past_match(&current, direction, &self.term);
        match self
            .term
            .search_next(&mut regex, origin, direction, side, None)
        {
            Some(next) => {
                self.jump_to_match(&next);
                let visible = self.visible_matches(&mut regex);
                self.search = Some(EmbedSearch::Active {
                    query,
                    regex,
                    current: next,
                    visible,
                    pre_offset,
                });
            }
            None => {
                self.search = Some(EmbedSearch::Active {
                    query,
                    regex,
                    current,
                    visible,
                    pre_offset,
                });
            }
        }
    }

    /// Leave search entirely - cancelling the prompt (Esc, or Enter on an
    /// empty query) or abandoning an active one (Esc) both restore the
    /// view exactly where it was before `/` was pressed and drop every
    /// highlight, since [`Self::search`] is what [`crate::ui::draw_embed`]
    /// reads to paint them.
    pub fn close_search(&mut self) {
        let Some(search) = self.search.take() else {
            return;
        };
        let pre_offset = match search {
            EmbedSearch::Prompt { pre_offset, .. } | EmbedSearch::Active { pre_offset, .. } => {
                pre_offset
            }
        };
        let current = self.term.grid().display_offset() as i32;
        self.term
            .scroll_display(Scroll::Delta(pre_offset as i32 - current));
    }

    /// The active search's current match, for highlight rendering. `None`
    /// outside [`EmbedSearch::Active`] - matches only render once a search
    /// has actually executed, never while merely composing the prompt.
    pub fn search_current_match(&self) -> Option<&Match> {
        match &self.search {
            Some(EmbedSearch::Active { current, .. }) => Some(current),
            _ => None,
        }
    }

    /// Every match visible in the viewport as of the last jump
    /// (Enter/`n`/`N`), for highlight rendering. A snapshot, not a live
    /// query: see the module doc on [`EmbedSearch::Active`] for why new
    /// output arriving mid-search does not retroactively update it. Empty
    /// outside [`EmbedSearch::Active`].
    pub fn search_visible_matches(&self) -> &[Match] {
        match &self.search {
            Some(EmbedSearch::Active { visible, .. }) => visible,
            _ => &[],
        }
    }

    /// The scrollback-search status-line text, or `None` outside search
    /// entirely (the caller falls back to its own default hint then).
    pub fn search_status(&self) -> Option<String> {
        match &self.search {
            Some(EmbedSearch::Prompt { query, error, .. }) => Some(match error {
                Some(message) => format!("search: {query}_  {message}"),
                None => format!("search: {query}_  enter jump, esc cancel"),
            }),
            Some(EmbedSearch::Active { query, .. }) => {
                Some(format!("search: {query}  n next  N prev  esc done"))
            }
            None => None,
        }
    }

    /// Scroll so `m`'s start line lands vertically centered in the
    /// viewport (see [`center_offset_for_line`]). `Scroll::Delta` only
    /// takes a relative step and clamps internally to the available
    /// scrollback, so the delta from the current offset is all that is
    /// needed - no separate bounds check here.
    fn jump_to_match(&mut self, m: &Match) {
        let current_offset = self.term.grid().display_offset() as i32;
        let history = self.term.grid().history_size();
        let target = center_offset_for_line(m.start().line.0, self.rows, history) as i32;
        self.term
            .scroll_display(Scroll::Delta(target - current_offset));
    }

    /// Every match inside the current viewport, scanning forward from its
    /// top row and bounding each hop's `max_lines` to whatever remains
    /// below it - so a search never scans past the visible rows looking
    /// for highlights. `VISIBLE_MATCH_CAP` is a defensive outer bound (not
    /// expected to bind in practice: each hop makes forward progress by at
    /// least one cell, so the loop already terminates on its own once the
    /// origin passes the last visible line).
    fn visible_matches(&self, regex: &mut RegexSearch) -> Vec<Match> {
        const VISIBLE_MATCH_CAP: usize = 500;
        let display_offset = self.term.grid().display_offset() as i32;
        let bottom_line = self.rows as i32 - 1 - display_offset;
        let mut matches = Vec::new();
        let mut origin = Point::new(Line(-display_offset), Column(0));
        for _ in 0..VISIBLE_MATCH_CAP {
            if origin.line.0 > bottom_line {
                break;
            }
            let remaining = (bottom_line - origin.line.0 + 1) as usize;
            let Some(m) =
                self.term
                    .search_next(regex, origin, Direction::Right, Side::Left, Some(remaining))
            else {
                break;
            };
            if m.start().line.0 > bottom_line {
                break;
            }
            origin = step_past_match(&m, Direction::Right, &self.term);
            matches.push(m);
        }
        matches
    }
}

/// One scrollback-search session over a mirror's grid (`/`): `Prompt` while
/// the query is being composed, `Active` once Enter has
/// found at least one match and `n`/`N` take over the keyboard. Lives on
/// [`Embed`] rather than [`crate::state::Mode`] - see the field doc on
/// [`Embed::search`] for why - so retargeting or closing the mirror always
/// clears it along with the rest of the `Embed` it belongs to. New output
/// arriving mid-search never invalidates either variant's state: alacritty
/// pins the display offset while scrolled back (so the view itself never
/// jumps), and [`Embed::visible_matches`]/a fresh `search_next` call are
/// what actually re-read the grid on the next navigation step - there is
/// no cached full match list to go stale.
#[derive(Debug)]
pub enum EmbedSearch {
    /// Composing the query. `error` holds the last invalid-pattern or
    /// no-matches message, cleared on the next edit.
    Prompt {
        query: String,
        error: Option<String>,
        /// The mirror's scroll position when `/` was pressed, restored by
        /// [`Embed::close_search`].
        pre_offset: usize,
    },
    /// A pattern has matched at least once. `regex` is the compiled DFA
    /// pair (mutable purely for its internal cache - alacritty's own API
    /// shape, not this module's choice), boxed so this variant does not
    /// dwarf `Prompt`'s (see the constructor sites' comment for why);
    /// `visible` is the last-computed on-screen match list
    /// [`crate::ui::draw_embed`] highlights.
    Active {
        query: String,
        regex: Box<RegexSearch>,
        current: Match,
        visible: Vec<Match>,
        pre_offset: usize,
    },
}

/// Wrap a user-typed search pattern for case-insensitive matching by
/// default: prefix `(?i)` unless the pattern already opens with a regex
/// flag group (`(?...)`), in which case the operator's own flags win
/// untouched. Deliberately simpler than alacritty's own built-in
/// "smart case" (`RegexSearch::new` makes a pattern case-*sensitive* the
/// moment it contains any uppercase letter): v1's rule here is "always
/// insensitive unless you opt out yourself" - `TODO` and `todo` match the
/// same lines - which is easier to explain than smart case and good
/// enough for a v1 mirror search. Pure - unit tested.
pub fn wrap_search_pattern(pattern: &str) -> String {
    if pattern.starts_with("(?") {
        pattern.to_owned()
    } else {
        format!("(?i){pattern}")
    }
}

/// The display offset that vertically centers grid line `line` (0 is the
/// top of the live screen, negative is scrollback - the same convention
/// [`Embed::grid_point`] uses) in a `rows`-tall viewport, clamped to the
/// available scrollback (`history_size`). Pure - unit tested against known
/// line/offset pairs.
pub fn center_offset_for_line(line: i32, rows: u16, history_size: usize) -> usize {
    let target_row = i32::from(rows / 2);
    let offset = target_row - line;
    offset.clamp(0, history_size as i32) as usize
}

/// The next search origin: one cell past `current`'s boundary in the
/// travel direction, so a repeat `Term::search_next` call cannot rematch
/// the same occurrence - alacritty's own search treats the origin as
/// included in a match (see `term::search::regex_search_left`/`_right`'s
/// doc comments upstream). `Direction::Left` (`n`, toward older history)
/// steps back past the match's start; `Direction::Right` (`N`, toward the
/// live tail) steps forward past its end. Takes a `Dimensions` impl rather
/// than a bare `&Embed` so it stays testable against any grid, live pane
/// or not.
fn step_past_match(current: &Match, direction: Direction, dims: &impl Dimensions) -> Point {
    match direction {
        Direction::Left => current.start().sub(dims, Boundary::None, 1),
        Direction::Right => current.end().add(dims, Boundary::None, 1),
    }
}

impl Drop for Embed {
    fn drop(&mut self) {
        // Toggle the pipe off; tmux closes its writer, the reader hits EOF
        // and exits, then the fifo is safe to remove. Kill nothing.
        let _ = Command::new("tmux")
            .args(["pipe-pane", "-t", &self.pane_id])
            .output();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = std::fs::remove_file(&self.fifo_path);
    }
}

/// Payload for one chunk of embedded-seat output, tagged with its pane so
/// stale chunks (after a retarget or close) are dropped by the reducer.
#[derive(Debug)]
pub struct EmbedChunk {
    pub pane_id: String,
    pub data: Vec<u8>,
}

fn spawn_reader(pane_id: String, fifo: std::path::PathBuf, tx: Sender<AppEvent>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // Opening the read end blocks until tmux's `cat >> fifo` opens the
        // write end (already started by the caller), so this rendezvous is
        // safe. EOF arrives when the pipe is toggled off in Drop.
        let Ok(mut file) = std::fs::File::open(&fifo) else {
            return;
        };
        let mut buf = [0u8; 8192];
        loop {
            match file.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = EmbedChunk {
                        pane_id: pane_id.clone(),
                        data: buf[..n].to_vec(),
                    };
                    if tx.send(AppEvent::Embed(chunk)).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// The pane's current mouse-tracking state as tmux itself tracks it:
/// `(any, sgr)` where `any` is true when the seat program has *any* mouse
/// mode on (DECSET 1000/1002/1003 - tmux's `mouse_any_flag`) and `sgr` is
/// true when it also negotiated SGR extended coordinates (DECSET 1006 -
/// `mouse_sgr_flag`). `None` when the query fails (e.g. the pane is gone).
///
/// tmux is the source of truth here, not our own `Term`: a seat typically
/// enables mouse mode at startup, *before* the operator opens this mirror,
/// so the enabling escape is long gone by attach time - the text-only
/// `capture-pane` backfill never carries it, and the live pipe only sees
/// bytes from attach onward. tmux, by contrast, tracks the pane's mode as
/// durable state and reports it regardless of when it was set.
fn pane_mouse_flags(pane_id: &str) -> Option<(bool, bool)> {
    let out = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{mouse_any_flag} #{mouse_sgr_flag}",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let any = parts.next()? == "1";
    let sgr = parts.next()? == "1";
    Some((any, sgr))
}

/// Query a pane's real width/height (columns, rows). We size our grid to
/// this and never write it back, preserving tmux's ignore-size guarantee.
fn pane_size(pane_id: &str) -> io::Result<(u16, u16)> {
    let out = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{pane_width} #{pane_height}",
        ])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("tmux display-message failed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let cols = parts.next().and_then(|v| v.parse().ok());
    let rows = parts.next().and_then(|v| v.parse().ok());
    match (cols, rows) {
        (Some(c), Some(r)) if c > 0 && r > 0 => Ok((c, r)),
        _ => Err(io::Error::other(format!("bad pane size: {text:?}"))),
    }
}

/// Capture the pane's current screen plus up to `scrollback_lines` of its
/// tmux history (`-S -N` starts the capture that many lines back; tmux
/// clamps to whatever history actually exists, so a young pane is captured
/// in full rather than erroring).
fn capture_pane(pane_id: &str, scrollback_lines: usize) -> io::Result<Vec<u8>> {
    let start = format!("-{scrollback_lines}");
    let out = Command::new("tmux")
        .args(["capture-pane", "-e", "-p", "-S", &start, "-t", pane_id])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("capture-pane failed"));
    }
    Ok(out.stdout)
}

/// Per-pane pipe: pane output only (`-O`) into the fifo. This keeps the
/// output firehose on a dedicated channel, never through the `-f no-output`
/// state sidecar (the load-bearing design point from BENCHMARKS.md).
fn start_pipe(pane_id: &str, fifo: &std::path::Path) -> io::Result<()> {
    let sink = format!("cat >> {}", shell_single_quote(&fifo.to_string_lossy()));
    let out = Command::new("tmux")
        .args(["pipe-pane", "-O", "-t", pane_id, &sink])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "pipe-pane failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn fifo_path_for(pane_id: &str) -> std::path::PathBuf {
    let sanitized: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir()
        .join(format!("nopal-field-{}", std::process::id()))
        .join(format!("embed-{sanitized}.fifo"))
}

fn make_fifo(path: &std::path::Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(path);
    let out = Command::new("mkfifo").arg(path).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "mkfifo failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Encode a crossterm key event as the exact byte sequence a terminal would
/// send, returned as `send-keys -H` hex tokens. `None` means the key has no
/// byte encoding we forward. Pure logic - unit tested.
pub fn key_to_hex(key: crossterm::event::KeyEvent) -> Option<Vec<String>> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Control maps letters/@[\]^_ to their C0 code (& 0x1f).
                let b = (c.to_ascii_uppercase() as u32) & 0x1f;
                vec![b as u8]
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![0x0d],
        KeyCode::Tab => vec![0x09],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => vec![0x1b, b'[', b'A'],
        KeyCode::Down => vec![0x1b, b'[', b'B'],
        KeyCode::Right => vec![0x1b, b'[', b'C'],
        KeyCode::Left => vec![0x1b, b'[', b'D'],
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::F(n) if (1..=4).contains(&n) => vec![0x1b, b'O', b'P' + (n - 1)],
        _ => return None,
    };

    // Alt/Meta prefixes the sequence with ESC (unless it is already ESC).
    let bytes = if alt && key.code != KeyCode::Esc {
        let mut prefixed = vec![0x1b];
        prefixed.extend(bytes);
        prefixed
    } else {
        bytes
    };

    Some(hex_tokens(&bytes))
}

/// Render raw bytes as `send-keys -H` hex tokens (two hex digits each).
/// Shared by keyboard encoding and mouse-wheel forwarding, which both end at
/// the same `tmux send-keys -H` delivery mechanism.
fn hex_tokens(bytes: &[u8]) -> Vec<String> {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Send a key to a seat pane as literal bytes (`-H` hex, no key-name
/// interpretation), so control chars and escapes pass through exactly.
pub fn send_key(pane_id: &str, key: crossterm::event::KeyEvent) -> io::Result<()> {
    let Some(hex) = key_to_hex(key) else {
        return Ok(());
    };
    send_hex(pane_id, &hex)
}

/// Deliver pre-encoded hex byte tokens to a pane via `send-keys -H`. The
/// shared tail of [`send_key`] and [`Embed::send_wheel`].
fn send_hex(pane_id: &str, hex: &[String]) -> io::Result<()> {
    let mut args = vec!["send-keys", "-H", "-t", pane_id];
    let refs: Vec<&str> = hex.iter().map(String::as_str).collect();
    args.extend(refs);
    let out = Command::new("tmux").args(&args).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other("send-keys failed"))
    }
}

/// Map a screen cell inside the embedded grid's rendered rectangle to a
/// 0-based local column/row within the seat's own screen coordinates.
/// `None` when the cell falls in the centering margin (`draw_embed` centers
/// a seat grid smaller than its panel) or is otherwise out of the seat's
/// bounds. Takes the same `Rect` [`crate::ui::HitMap::embed_grid`] already
/// hands callers, rather than four separate scalars - one struct that
/// cannot have its fields transposed by accident, instead of a positional
/// x/y/width/height quartet that can. Pure logic - unit tested.
pub fn screen_to_local(
    grid: ratatui::layout::Rect,
    cols: u16,
    rows: u16,
    x: u16,
    y: u16,
) -> Option<(u16, u16)> {
    let x_off = grid.width.saturating_sub(cols) / 2;
    let y_off = grid.height.saturating_sub(rows) / 2;
    let local_col = x.checked_sub(grid.x)?.checked_sub(x_off)?;
    let local_row = y.checked_sub(grid.y)?.checked_sub(y_off)?;
    if local_col >= cols || local_row >= rows {
        return None;
    }
    Some((local_col, local_row))
}

/// Encode one wheel notch as the exact escape sequence a real terminal
/// would send while the app underneath has mouse reporting on. `column`/
/// `row` are 0-based local coordinates within the seat's grid; xterm mouse
/// coordinates are 1-based, so we add one before encoding. Pure logic -
/// unit tested against known SGR/X10 byte strings.
///
/// Button codes 64 (wheel up) and 65 (wheel down) are xterm's mouse-tracking
/// convention; the wheel has no separate press/release pair, so a single
/// report per notch is exactly what a real terminal emits.
pub fn encode_wheel_mouse(sgr: bool, up: bool, column: u16, row: u16) -> Vec<u8> {
    let button: u16 = if up { 64 } else { 65 };
    let (col1, row1) = (column.saturating_add(1), row.saturating_add(1));
    if sgr {
        format!("\x1b[<{button};{col1};{row1}M").into_bytes()
    } else {
        // X10/legacy: three raw bytes after `ESC [ M`, each offset by 32;
        // xterm caps coordinates at 223 so `32 + coordinate` never exceeds
        // a single byte's valid (non-wrapping) range.
        let cx = (col1.min(223) + 32) as u8;
        let cy = (row1.min(223) + 32) as u8;
        vec![0x1b, b'[', b'M', (button + 32) as u8, cx, cy]
    }
}

/// Whether a left-press at time `elapsed` after the previous one, `dx`/`dy`
/// cells away from it, counts as a double-click: within
/// [`DOUBLE_CLICK_WINDOW`] and on the same or an adjacent cell (a real
/// click rarely lands on the exact same cell twice). Pure logic - unit
/// tested; the caller (`app.rs`) owns the actual `Instant`/position
/// bookkeeping so this stays free of wall-clock side effects.
pub fn is_double_click(elapsed: Duration, dx: u16, dy: u16) -> bool {
    elapsed <= DOUBLE_CLICK_WINDOW && dx <= 1 && dy <= 1
}

/// Copy `text` to the system clipboard. macOS's `pbcopy` is tried first -
/// a write-only, one-shot external command that does not earn a clipboard
/// crate. Where `pbcopy` does not exist (Linux hosts), fall back to
/// emitting OSC 52, which asks the terminal emulator itself to set the
/// clipboard; that also covers SSH sessions, and tmux's default
/// `set-clipboard external` forwards it from inner applications to the
/// outer terminal. Only spawn-time `NotFound` triggers the fallback: a
/// `pbcopy` that exists but fails is a real error the user should see,
/// not something to paper over with a second mechanism.
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    match copy_via_pbcopy(text) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => copy_via_osc52(text),
        result => result,
    }
}

fn copy_via_pbcopy(text: &str) -> io::Result<()> {
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("pbcopy failed"))
    }
}

/// Write the OSC 52 sequence to stdout - the same stream ratatui renders
/// to, so the terminal is guaranteed to be listening on it. The sequence
/// sets the clipboard as a side effect and renders nothing, so it cannot
/// disturb the frame.
fn copy_via_osc52(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(osc52_sequence(text).as_bytes())?;
    out.flush()
}

/// `ESC ] 52 ; c ; <base64 payload> BEL`. `c` targets the system clipboard
/// (as opposed to the primary selection). Some emulators cap the payload
/// around 100KB of base64; a scrollback selection stays well under that.
fn osc52_sequence(text: &str) -> String {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{payload}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn hex(code: KeyCode, mods: KeyModifiers) -> Option<Vec<String>> {
        key_to_hex(KeyEvent::new(code, mods))
    }

    #[test]
    fn plain_chars_encode_to_utf8_hex() {
        assert_eq!(
            hex(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(vec!["61".to_owned()])
        );
        assert_eq!(
            hex(KeyCode::Enter, KeyModifiers::NONE),
            Some(vec!["0d".to_owned()])
        );
        assert_eq!(
            hex(KeyCode::Esc, KeyModifiers::NONE),
            Some(vec!["1b".to_owned()])
        );
    }

    #[test]
    fn osc52_sequence_wraps_standard_base64() {
        // "hello" -> aGVsbG8= is the canonical RFC 4648 vector; padding
        // must be present (some emulators reject unpadded payloads).
        assert_eq!(osc52_sequence("hello"), "\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(osc52_sequence(""), "\x1b]52;c;\x07");
        // Multi-byte UTF-8 is encoded byte-wise, not char-wise.
        assert_eq!(osc52_sequence("é"), "\x1b]52;c;w6k=\x07");
    }

    #[test]
    fn ctrl_letters_map_to_c0_control_codes() {
        // Ctrl-C -> 0x03, Ctrl-O -> 0x0f (the leave chord is intercepted
        // before this ever runs, but the encoding must still be correct).
        assert_eq!(
            hex(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(vec!["03".to_owned()])
        );
        assert_eq!(
            hex(KeyCode::Char('a'), KeyModifiers::CONTROL),
            Some(vec!["01".to_owned()])
        );
    }

    #[test]
    fn arrows_and_alt_prefix() {
        assert_eq!(
            hex(KeyCode::Up, KeyModifiers::NONE),
            Some(vec!["1b".to_owned(), "5b".to_owned(), "41".to_owned()])
        );
        // Alt-b prefixes ESC (0x1b) before 'b' (0x62).
        assert_eq!(
            hex(KeyCode::Char('b'), KeyModifiers::ALT),
            Some(vec!["1b".to_owned(), "62".to_owned()])
        );
    }

    // --- screen_to_local: screen cell -> seat-grid coordinates ---

    #[test]
    fn screen_to_local_subtracts_origin_and_centering_offset() {
        // A 10x4 seat centered in a 20x10 panel at (5, 2): x_off = (20-10)/2
        // = 5, y_off = (10-4)/2 = 3. The panel's top-left visible cell is
        // therefore seat-local (0, 0) at screen (5+5, 2+3) = (10, 5).
        let grid = ratatui::layout::Rect::new(5, 2, 20, 10);
        assert_eq!(screen_to_local(grid, 10, 4, 10, 5), Some((0, 0)));
        // The seat's bottom-right cell (9, 3) sits at screen
        // (10+9, 5+3) = (19, 8).
        assert_eq!(screen_to_local(grid, 10, 4, 19, 8), Some((9, 3)));
    }

    #[test]
    fn screen_to_local_rejects_the_centering_margin_and_out_of_bounds() {
        // Same geometry as above (seat cells occupy screen x in 10..=19,
        // y in 5..=8); (9, 5) is one cell left of the seat's left edge
        // (inside the centering margin), and (20, 5) is one past its right
        // edge.
        let grid = ratatui::layout::Rect::new(5, 2, 20, 10);
        assert_eq!(screen_to_local(grid, 10, 4, 9, 5), None);
        assert_eq!(screen_to_local(grid, 10, 4, 20, 5), None);
        // Above the panel entirely underflows the subtraction.
        assert_eq!(screen_to_local(grid, 10, 4, 10, 0), None);
    }

    // --- encode_wheel_mouse: SGR / X10 wheel byte sequences ---

    #[test]
    fn encodes_sgr_wheel_up_and_down() {
        // SGR: ESC [ < Cb ; Px ; Py M, 1-based coordinates, no release byte
        // (the wheel has no separate press/release like a real button).
        assert_eq!(
            encode_wheel_mouse(true, true, 0, 0),
            b"\x1b[<64;1;1M".to_vec()
        );
        assert_eq!(
            encode_wheel_mouse(true, false, 9, 4),
            b"\x1b[<65;10;5M".to_vec()
        );
    }

    #[test]
    fn encodes_x10_wheel_with_32_offset_bytes() {
        // X10/legacy: ESC [ M <button+32> <col+32> <row+32>, all single
        // bytes. Column 0, row 0 (1-based 1,1) -> 32+1 = 33 = 0x21.
        assert_eq!(
            encode_wheel_mouse(false, true, 0, 0),
            vec![0x1b, b'[', b'M', 64 + 32, 33, 33]
        );
        assert_eq!(
            encode_wheel_mouse(false, false, 0, 0),
            vec![0x1b, b'[', b'M', 65 + 32, 33, 33]
        );
    }

    #[test]
    fn x10_wheel_clamps_far_coordinates() {
        // xterm's legacy encoding caps at 223 so 32+coordinate never
        // overflows a byte; a seat wider than that still gets a valid,
        // if imprecise, report rather than a wrapped/garbage byte.
        let bytes = encode_wheel_mouse(false, true, 9000, 9000);
        assert_eq!(bytes[4], (223 + 32) as u8);
        assert_eq!(bytes[5], (223 + 32) as u8);
    }

    // --- is_double_click: timing + adjacency window ---

    #[test]
    fn double_click_within_window_and_adjacent_cell() {
        assert!(is_double_click(Duration::from_millis(150), 0, 0));
        assert!(is_double_click(Duration::from_millis(400), 1, 1));
    }

    #[test]
    fn double_click_rejects_stale_or_distant_clicks() {
        assert!(!is_double_click(Duration::from_millis(401), 0, 0));
        assert!(!is_double_click(Duration::from_millis(50), 2, 0));
        assert!(!is_double_click(Duration::from_millis(50), 0, 2));
    }

    // --- selection + scrollback, exercised against a real Term/Embed ---

    /// A detached `Embed` for tests: the exact `Term`/`Config` construction
    /// `Embed::open` uses, but with no tmux pane, pipe, or fifo behind it
    /// (`Drop` still shells out to `tmux pipe-pane` defensively; the pane id
    /// does not exist, so that call harmlessly fails and is ignored, same
    /// as any other backend error `Embed::open`'s callers already handle).
    fn test_embed(cols: u16, rows: u16) -> Embed {
        Embed::test_for_app_with_size("%test-nonexistent", "test", false, cols, rows)
    }

    #[test]
    fn selection_text_extracts_a_simple_drag_range() {
        let mut embed = test_embed(20, 3);
        embed.advance(b"hello world");
        // Drag from the 'h' to the second 'o' selects "hello".
        embed.begin_selection(0, 0, false);
        embed.update_selection(4, 0);
        assert_eq!(embed.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn double_click_selects_the_token_under_the_pointer() {
        let mut embed = test_embed(20, 3);
        embed.advance(b"hello world");
        // A double-click lands mid-word in "world" (columns 6..=10); no
        // drag update needed - semantic expansion happens on `to_range`.
        embed.begin_selection(8, 0, true);
        assert_eq!(embed.selection_text().as_deref(), Some("world"));
    }

    #[test]
    fn a_plain_click_with_no_drag_selects_nothing() {
        let mut embed = test_embed(10, 3);
        embed.advance(b"hello");
        embed.begin_selection(2, 0, false);
        // Same start and end point: alacritty's own `Selection::is_empty`
        // says there is nothing to copy, and `selection_text` filters it.
        assert_eq!(embed.selection_text(), None);
    }

    #[test]
    fn scrolled_back_selection_reads_history_not_the_live_screen() {
        // 5 columns x 2 rows; three 5-char lines pushes the first one
        // ("AAAAA") into scrollback and the second ("BBBBB") to just above
        // the live screen.
        let mut embed = test_embed(5, 2);
        embed.advance(b"AAAAA\r\nBBBBB\r\nCCCCC\r\n");
        assert_eq!(embed.term().grid().display_offset(), 0);

        // Scroll up two notches (1 line each) to bring "AAAAA" to the top
        // row of the viewport.
        embed.scroll_lines(1);
        embed.scroll_lines(1);
        assert_eq!(embed.term().grid().display_offset(), 2);

        embed.begin_selection(0, 0, false);
        embed.update_selection(4, 0);
        assert_eq!(
            embed.selection_text().as_deref(),
            Some("AAAAA"),
            "row 0 while scrolled back 2 lines must read history, not the live tail"
        );
    }

    #[test]
    fn mouse_reporting_falls_back_to_term_mode_without_a_live_pane() {
        // `%test-nonexistent` has no tmux pane, so `pane_mouse_flags` fails
        // and `mouse_reporting` falls back to the parsed `Term` mode. (The
        // tmux-flag primary path is proven live in
        // tests/tmux_integration.rs::embed_detects_mouse_reporting.)
        let mut embed = test_embed(10, 3);
        assert!(!embed.mouse_reporting());
        // DECSET 1000 (X10/normal mouse tracking) turns on click reporting.
        embed.advance(b"\x1b[?1000h");
        assert!(embed.mouse_reporting());
        embed.advance(b"\x1b[?1000l");
        assert!(!embed.mouse_reporting());
    }

    // --- scrollback search: pure helpers ---

    #[test]
    fn wrap_search_pattern_prefixes_case_insensitive_by_default() {
        assert_eq!(wrap_search_pattern("todo"), "(?i)todo");
    }

    #[test]
    fn wrap_search_pattern_respects_an_explicit_flag_group() {
        // The operator opted into their own flags; do not stack ours on
        // top (a double `(?i)(?-i)...` is not what "unless you opt out
        // yourself" means).
        assert_eq!(wrap_search_pattern("(?-i)TODO"), "(?-i)TODO");
        assert_eq!(wrap_search_pattern("(?m)^foo"), "(?m)^foo");
    }

    #[test]
    fn center_offset_for_line_centers_a_history_line_in_the_viewport() {
        // 10-row viewport, target row = rows/2 = 5; a match on grid line
        // -20 needs offset 25 so local_row = line + offset = -20+25 = 5.
        assert_eq!(center_offset_for_line(-20, 10, 5000), 25);
    }

    #[test]
    fn center_offset_for_line_clamps_to_available_history() {
        // Only 3 lines of scrollback exist; centering a line far back
        // clamps to that ceiling rather than an offset the grid cannot
        // honor (`Grid::scroll_display` would silently clamp too, but the
        // pure function should already report the same answer).
        assert_eq!(center_offset_for_line(-500, 10, 3), 3);
        // A live-screen line (0 or positive) never needs to scroll back.
        assert_eq!(center_offset_for_line(9, 10, 5000), 0);
    }

    #[test]
    fn step_past_match_moves_one_cell_beyond_each_boundary() {
        let embed = test_embed(10, 3);
        let m: Match = Point::new(Line(0), Column(2))..=Point::new(Line(0), Column(4));
        assert_eq!(
            step_past_match(&m, Direction::Left, embed.term()),
            Point::new(Line(0), Column(1)),
            "n steps back past the match's start"
        );
        assert_eq!(
            step_past_match(&m, Direction::Right, embed.term()),
            Point::new(Line(0), Column(5)),
            "N steps forward past the match's end"
        );
    }

    // --- scrollback search: against a real Term with scrollback content ---

    #[test]
    fn execute_search_finds_a_match_in_scrollback_and_centers_it() {
        let mut embed = test_embed(20, 3);
        for line in [
            "alpha one",
            "beta two",
            "gamma three",
            "delta four",
            "epsilon five",
            "zeta six",
        ] {
            embed.advance(line.as_bytes());
            embed.advance(b"\r\n");
        }
        assert_eq!(
            embed.term().grid().display_offset(),
            0,
            "a fresh mirror starts at the live tail"
        );

        embed.start_search();
        for c in "alpha".chars() {
            embed.search_push(c);
        }
        embed.execute_search();

        assert!(
            matches!(embed.search, Some(EmbedSearch::Active { .. })),
            "a real match must activate search, got {:?}",
            embed.search
        );
        assert!(
            embed.term().grid().display_offset() > 0,
            "\"alpha\" only appears in scrollback, so the view must have scrolled back to show it"
        );
        let current = embed
            .search_current_match()
            .expect("active search always carries a current match");
        assert!(
            current.start().line.0 < 0,
            "the match landed in history, not the live screen"
        );
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let mut embed = test_embed(20, 3);
        embed.advance(b"Hello World\r\n");
        embed.start_search();
        for c in "hello".chars() {
            embed.search_push(c);
        }
        embed.execute_search();
        assert!(
            matches!(embed.search, Some(EmbedSearch::Active { .. })),
            "a lowercase query must still match the capitalized word by default"
        );
    }

    #[test]
    fn execute_search_with_no_matches_reports_the_prompt_error_and_stays_open() {
        let mut embed = test_embed(20, 3);
        embed.advance(b"hello world\r\n");
        embed.start_search();
        for c in "zzz".chars() {
            embed.search_push(c);
        }
        embed.execute_search();
        match &embed.search {
            Some(EmbedSearch::Prompt { error, .. }) => {
                assert_eq!(error.as_deref(), Some("no matches"));
            }
            other => panic!("expected the prompt to stay open with an error, got {other:?}"),
        }
    }

    #[test]
    fn execute_search_with_an_invalid_pattern_reports_the_prompt_error_and_stays_open() {
        let mut embed = test_embed(20, 3);
        embed.advance(b"hello\r\n");
        embed.start_search();
        for c in "[unterminated".chars() {
            embed.search_push(c);
        }
        embed.execute_search();
        match &embed.search {
            Some(EmbedSearch::Prompt { error, .. }) => {
                assert_eq!(error.as_deref(), Some("invalid regex"));
            }
            other => panic!("expected the prompt to stay open with an error, got {other:?}"),
        }
    }

    #[test]
    fn execute_search_on_an_empty_query_cancels_like_esc() {
        let mut embed = test_embed(20, 3);
        embed.advance(b"hello\r\n");
        let before = embed.term().grid().display_offset();
        embed.start_search();
        embed.execute_search();
        assert!(embed.search.is_none());
        assert_eq!(embed.term().grid().display_offset(), before);
    }

    #[test]
    fn search_advance_n_then_shift_n_round_trips_to_the_same_match() {
        // Every line starts with "dog", so more than one match exists and
        // `n`/`N` have somewhere to travel.
        let mut embed = test_embed(20, 2);
        for i in 0..5 {
            embed.advance(format!("dog line {i}").as_bytes());
            embed.advance(b"\r\n");
        }
        embed.start_search();
        for c in "dog".chars() {
            embed.search_push(c);
        }
        embed.execute_search();
        let first = embed
            .search_current_match()
            .cloned()
            .expect("execute_search found a match");

        embed.search_advance(true); // n: toward older history
        let second = embed
            .search_current_match()
            .cloned()
            .expect("n always finds another match when more than one exists");
        assert_ne!(first, second, "n must not rematch the same occurrence");

        embed.search_advance(false); // N: back toward the live tail
        let back = embed
            .search_current_match()
            .cloned()
            .expect("N always finds a match");
        assert_eq!(
            back, first,
            "N immediately after n returns to the original match"
        );
    }

    #[test]
    fn search_advance_wraps_back_to_the_only_match_when_there_is_just_one() {
        let mut embed = test_embed(20, 2);
        embed.advance(b"unique needle here\r\nfiller one\r\nfiller two\r\n");
        embed.start_search();
        for c in "needle".chars() {
            embed.search_push(c);
        }
        embed.execute_search();
        let first = embed
            .search_current_match()
            .cloned()
            .expect("execute_search found the only match");

        embed.search_advance(true);
        let second = embed
            .search_current_match()
            .cloned()
            .expect("cycling past the only match must still land on a match");
        assert_eq!(
            first, second,
            "wraparound with a single match returns to itself"
        );
    }

    #[test]
    fn close_search_restores_the_pre_search_scroll_position() {
        let mut embed = test_embed(20, 2);
        for line in ["aaa", "bbb", "ccc", "dog", "eee"] {
            embed.advance(line.as_bytes());
            embed.advance(b"\r\n");
        }
        assert_eq!(embed.term().grid().display_offset(), 0);

        embed.start_search();
        for c in "bbb".chars() {
            embed.search_push(c);
        }
        embed.execute_search();
        assert!(
            embed.term().grid().display_offset() > 0,
            "\"bbb\" is in history, so the view must have scrolled"
        );

        embed.close_search();
        assert_eq!(
            embed.term().grid().display_offset(),
            0,
            "esc restores exactly the pre-search offset"
        );
        assert!(embed.search.is_none());
    }

    #[test]
    fn esc_cancels_the_prompt_without_ever_having_searched() {
        let mut embed = test_embed(20, 2);
        embed.advance(b"hello\r\n");
        let before = embed.term().grid().display_offset();
        embed.start_search();
        embed.search_push('h');
        embed.close_search();
        assert!(embed.search.is_none());
        assert_eq!(embed.term().grid().display_offset(), before);
    }
}
