//! The field event loop: one channel, many producers (sidecar reader,
//! feed pollers, terminal input), one reducer, throttled renders.
//!
//! Concurrency model: plain threads and `std::sync::mpsc`, no async
//! runtime. Each producer blocks on its own source; the loop blocks on
//! the channel with a timeout. Nothing here needs structured concurrency
//! or backpressure beyond a bounded drain per iteration.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};

use crate::feeds::agents::AgentPanesFeed;
use crate::feeds::asks::AskClient;
use crate::feeds::field::FieldFeed;
use crate::feeds::rondo::{RondoFeed, RunSpec};
use crate::feeds::{self, run_json_command};
use crate::keys::{KeyAction, Scope};
use crate::seat::{self, Candidate, CandidateKind, CandidateSource};
use crate::state::{
    App, ContextAction, Mode, PlotSession, RowDrag, RowDropOutcome, Section, move_context_cursor,
    resolve_row_drag,
};
use crate::tmux::{Backend, sidecar::Sidecar};
use crate::{AppEvent, ui};

/// Minimum interval between frames; events between frames coalesce.
const FRAME_BUDGET: Duration = Duration::from_millis(33);

/// Everything the UI needs to run inside its pane.
#[derive(Debug, Clone)]
pub struct Options {
    pub session: String,
    pub nopal_bin: PathBuf,
    /// State root for the field query and ask resolution.
    pub state_dir: Option<PathBuf>,
    /// Optional rondo.core/v1 feed forwarded to `nopal field --rondo-events`.
    pub rondo_events: Option<PathBuf>,
    pub rondo_dir: PathBuf,
    pub rondo_runs: Vec<RunSpec>,
    /// Recorded as `--by` on ask resolution.
    pub resolve_by: String,
    /// Start with the sidebar showing every session, not just managed ones.
    pub show_all: bool,
}

/// Run the field UI. Must execute inside the field's tmux pane.
pub fn run_ui(options: &Options) -> io::Result<()> {
    let field_pane = std::env::var("TMUX_PANE").map_err(|_| {
        io::Error::other(
            "nopal field ui must run inside tmux (no TMUX_PANE); use `nopal field` to launch",
        )
    })?;

    // Idempotent self-repair: resurrect restores drop pane user options,
    // so the UI re-tags its own pane on every start.
    Backend::tag_field_pane(&field_pane)?;

    // Re-apply the managed marker to every still-live session nopal owns,
    // healing a tmux-resurrect restore that drops session user options
    // (see registry.rs). Also (re)mark and record the field's own
    // session so it survives the same way.
    restamp_managed_sessions(options);

    let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
    let mut sidecar = Sidecar::attach(&options.session, tx.clone())?;
    sidecar.reconcile()?;
    spawn_feeds(options, &tx);
    let embed_tx = tx.clone();
    spawn_input_thread(tx);

    let mut app = App::new(field_pane, options.session.clone());
    app.show_all = options.show_all;
    // Keybindings are parsed once here, not lazily like the spawn picker's
    // own config reload (that one intentionally re-reads on every `n` so a
    // project added mid-session shows up) - a remap only ever needs to
    // take effect on the next launch.
    let key_cfg = seat::config::load(options.state_dir.as_deref());
    let (key_registry, key_problems) = crate::keys::KeyRegistry::build(&key_cfg.keys);
    app.keys = key_registry;
    if let Some(message) = crate::keys::summarize_problems(&key_problems) {
        app.status_line = format!("keybindings: {message}");
    }
    let backend = Backend::new(options.session.clone());
    let resolver = AskClient::new(options.nopal_bin.clone(), options.state_dir.clone());
    bootstrap_plot(&mut app, options);

    let mut terminal = ratatui::init();
    // Mouse capture is not part of ratatui::init's contract; take it here
    // and release it below (the release runs on the error path too).
    let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
    let result = event_loop(
        &mut terminal,
        &mut app,
        &rx,
        &mut sidecar,
        &backend,
        &resolver,
        options,
        &embed_tx,
    );
    // Leave no zoomed window behind if we quit mid-embed or mid-help.
    if app.stage_open || app.embed.is_some() || app.help_zoomed {
        let _ = Backend::set_zoom(&app.field_pane_id, false);
    }
    let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Ensure the first real interactive Field has one durable Provisional Plot
/// and one ordinary Nopal/Pi seat bound to it. This runs only inside the
/// hidden tmux UI process, after `launch` has passed its TTY boundary.
fn bootstrap_plot(app: &mut App, options: &Options) {
    let env = nopal_core::plot_store::PlotEnv::discover(options.state_dir.as_deref());
    let plot = match nopal_core::plot_store::ensure_provisional(&env, &options.session) {
        Ok(plot) => plot,
        Err(err) => {
            app.status_line = format!("Plot unavailable: {err}");
            return;
        }
    };

    if let Some(session) = plot
        .selected_session_id
        .as_ref()
        .and_then(|selected| {
            plot.sessions
                .iter()
                .find(|session| &session.session_id == selected)
        })
        .or_else(|| plot.sessions.first())
        && Backend::session_active_pane(&session.host_session).is_ok()
    {
        match Backend::stamp_plot_identity(
            &session.host_session,
            &plot.plot_id,
            &session.session_id,
        ) {
            Ok(()) => {
                app.pending_embed_session = Some(session.host_session.clone());
                return;
            }
            Err(err) => {
                app.status_line = format!("saved Session identity unavailable: {err}");
                return;
            }
        }
    }

    let path = match std::env::current_dir() {
        Ok(path) => path,
        Err(err) => {
            app.status_line = format!("cannot open first Session: {err}");
            return;
        }
    };
    let Some(spawned) = spawn_seat_at(app, &path.to_string_lossy(), options) else {
        return;
    };
    match nopal_core::plot_store::bind_session(
        &env,
        &plot.plot_id,
        &spawned.host_session,
        Some(&spawned.host_pane),
    ) {
        Ok(updated) => {
            if let Some(session_id) = updated.selected_session_id
                && let Err(err) = Backend::stamp_plot_identity(
                    &spawned.host_session,
                    &updated.plot_id,
                    &session_id,
                )
            {
                app.status_line = format!("Session started; Plot identity pending: {err}");
            }
        }
        Err(err) => app.status_line = format!("Session started but Plot binding failed: {err}"),
    }
}

/// Re-stamp `@nopal_managed` on the field's own session and every session
/// in the durable registry that still exists; record the field session so
/// a future launch heals it too.
fn restamp_managed_sessions(options: &Options) {
    let path = crate::registry::registry_path(options.state_dir.as_deref());
    for entry in crate::registry::load(&path) {
        let _ = Backend::mark_session_managed(&entry.session, &entry.repo);
    }
    let _ = Backend::mark_session_managed(&options.session, "");
    crate::registry::record(
        &path,
        crate::registry::ManagedSeat {
            session: options.session.clone(),
            repo: String::new(),
            recorded_at: now_stamp(),
            // The field's own session is never spawned through the seat
            // picker; it has no meaningful spawn path to record.
            path: String::new(),
        },
    );
}

fn now_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

fn spawn_feeds(options: &Options, tx: &Sender<AppEvent>) {
    feeds::spawn(
        Box::new(FieldFeed::new(
            options.nopal_bin.clone(),
            options.state_dir.clone(),
            options.rondo_events.clone(),
        )),
        tx.clone(),
    );
    if !options.rondo_runs.is_empty() {
        feeds::spawn(
            Box::new(RondoFeed::new(
                options.rondo_dir.clone(),
                options.rondo_runs.clone(),
            )),
            tx.clone(),
        );
    }
    // The pi needle mirrors the launcher's exec convention (`NOPAL_PI_BIN`
    // or `pi`): once `nopal cli` execs, the nopal path is gone from the
    // process tree and pi's rewritten title is all that identifies the
    // agent (zero-config launch E2E).
    let pi_bin = std::env::var("NOPAL_PI_BIN")
        .ok()
        .filter(|bin| !bin.is_empty())
        .unwrap_or_else(|| "pi".to_owned());
    feeds::spawn(
        Box::new(AgentPanesFeed::new(options.nopal_bin.clone(), pi_bin)),
        tx.clone(),
    );
}

fn spawn_input_thread(tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if tx.send(AppEvent::Input(event)).is_err() {
                break;
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &Receiver<AppEvent>,
    sidecar: &mut Sidecar,
    backend: &Backend,
    resolver: &AskClient,
    options: &Options,
    embed_tx: &Sender<AppEvent>,
) -> io::Result<()> {
    let mut dirty = true;
    let mut last_frame = Instant::now() - FRAME_BUDGET;
    let mut last_reconcile = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                handle_event(app, event, backend, resolver, options, embed_tx)?;
                dirty = true;
                // Drain whatever queued behind it; one render per burst.
                for event in rx.try_iter().collect::<Vec<_>>() {
                    handle_event(app, event, backend, resolver, options, embed_tx)?;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        // Foreign-session pane state never pushes; refresh the snapshot
        // on topology events and on a coarse timer (one cheap server-local
        // command through the existing control channel).
        if app.wants_reconcile || last_reconcile.elapsed() >= Duration::from_secs(5) {
            app.wants_reconcile = false;
            last_reconcile = Instant::now();
            sidecar.reconcile()?;
        }
        if app.should_quit {
            return Ok(());
        }
        if dirty && last_frame.elapsed() >= FRAME_BUDGET {
            terminal.draw(|frame| ui::draw(frame, app))?;
            app.frames_rendered += 1;
            last_frame = Instant::now();
            dirty = false;
        }
    }
}

fn handle_event(
    app: &mut App,
    event: AppEvent,
    backend: &Backend,
    resolver: &AskClient,
    options: &Options,
    embed_tx: &Sender<AppEvent>,
) -> io::Result<()> {
    match event {
        AppEvent::Tmux(notification) => {
            app.reduce_tmux(&notification);
            // A just-spawned seat opens in the embedded panel as soon as
            // reconcile delivers it (spawn never switch-clients away).
            fulfill_pending_embed(app, embed_tx);
            if app.stage_open {
                sync_plot_transport(app, embed_tx);
            }
        }
        AppEvent::Feed(feed_event) => {
            app.reduce_feed(feed_event);
            if app.stage_open {
                sync_plot_transport(app, embed_tx);
            }
        }
        AppEvent::Embed(chunk) => {
            if let Some(embed) = &mut app.embed
                && embed.pane_id == chunk.pane_id
            {
                embed.advance(&chunk.data);
            }
        }
        AppEvent::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => {
            handle_key(app, key, backend, resolver, options, embed_tx);
        }
        AppEvent::Input(Event::Mouse(mouse)) => {
            handle_mouse(app, mouse, backend, embed_tx);
        }
        AppEvent::Input(_) => {}
    }
    Ok(())
}

fn handle_key(
    app: &mut App,
    key: KeyEvent,
    backend: &Backend,
    resolver: &AskClient,
    options: &Options,
    embed_tx: &Sender<AppEvent>,
) {
    // The help overlay swallows input until dismissed.
    if app.mode == Mode::Help {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
        ) {
            dismiss_help(app);
        }
        return;
    }
    // Esc cancels an in-flight row-drag before anything
    // else sees it - in particular before the embed-open short-circuit
    // just below, whose own Esc closes the embed outright. A drag can be
    // in flight with an embed open (dragging a row over its own panel), so
    // this must be checked first or the drag would never get its own
    // cancel and would instead take the embed down with it.
    if app.row_drag.is_some() && key.code == KeyCode::Esc {
        app.row_drag = None;
        app.status_line.clear();
        return;
    }
    // The durable Plot stage owns routing even when no Session transport is
    // available. This must precede the legacy embed check because execution
    // activity deliberately has no embed at all.
    if app.stage_open {
        handle_stage_key(app, key, backend, embed_tx);
        return;
    }
    // An embedded seat owns the keyboard: input focus forwards to the seat,
    // nav handles the panel. Global Ctrl-C quit is deliberately not reached
    // here so it can travel to the seat.
    if app.embed.is_some() {
        handle_embed_key(app, key, backend, embed_tx);
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    match app.mode.clone() {
        Mode::Filter(text) => handle_filter_key(app, key, text),
        Mode::SpawnPicker { query, cursor } => {
            handle_spawn_picker_key(app, key, query, cursor, options)
        }
        Mode::GotoPicker { query, cursor } => {
            handle_goto_picker_key(app, key, query, cursor, embed_tx)
        }
        Mode::WorktreeName { project, buffer } => {
            handle_worktree_name_key(app, key, project, buffer, options)
        }
        Mode::ConfirmKill(row_key) => handle_confirm_kill_key(app, key, row_key, backend),
        Mode::ContextMenu { row_key, cursor } => {
            handle_context_menu_key(app, key, row_key, cursor, backend, options, embed_tx)
        }
        Mode::RunDetail(_) => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Normal,
            _ => {}
        },
        Mode::Help => {}
        Mode::Normal => handle_normal_key(app, key, backend, resolver, options, embed_tx),
    }
}

fn handle_stage_key(app: &mut App, key: KeyEvent, backend: &Backend, embed_tx: &Sender<AppEvent>) {
    let has_session_transport = selected_session_transport(app).is_some();
    let input_focus =
        has_session_transport && app.embed.as_ref().is_some_and(|embed| embed.input_focus);
    if input_focus {
        if app.keys.effective(KeyAction::ReleaseInput).matches(&key) {
            if let Some(embed) = &mut app.embed {
                embed.input_focus = false;
            }
            app.status_line = sidebar_focus_hint(app);
            return;
        }
        if let Some(embed) = &app.embed
            && let Err(err) = crate::embed::send_key(&embed.pane_id, key)
        {
            app.status_line = format!("send failed: {err}");
        }
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    if has_session_transport
        && app
            .embed
            .as_ref()
            .is_some_and(|embed| embed.search.is_some())
    {
        handle_embed_search_key(app, key);
        return;
    }
    if key.code == KeyCode::Esc {
        close_stage(app);
        return;
    }
    if let KeyCode::Char(c) = key.code
        && c.is_ascii_digit()
        && c != '0'
    {
        let n = (c as u8 - b'0') as usize;
        if app.select_nth_plot(n) {
            sync_plot_transport(app, embed_tx);
        }
        return;
    }
    let Some(action) = app.keys.action_for(Scope::Stage, &key) else {
        return;
    };
    match action {
        KeyAction::Help => open_help(app),
        KeyAction::CloseEmbed => close_stage(app),
        KeyAction::MoveDown => {
            app.move_plot_selection(1);
            sync_plot_transport(app, embed_tx);
        }
        KeyAction::MoveUp => {
            app.move_plot_selection(-1);
            sync_plot_transport(app, embed_tx);
        }
        KeyAction::ActivityNext => {
            app.cycle_plot_activity(1);
            sync_plot_transport(app, embed_tx);
        }
        KeyAction::ActivityPrev => {
            app.cycle_plot_activity(-1);
            sync_plot_transport(app, embed_tx);
        }
        KeyAction::Collapse => app.toggle_inspector(),
        KeyAction::OpenView | KeyAction::InputFocus if has_session_transport => {
            if let Some(embed) = &mut app.embed {
                embed.input_focus = true;
            }
            let label = embed_label(app);
            app.status_line = seat_input_hint(app, &label);
        }
        KeyAction::EmbedSearch if has_session_transport => enter_embed_search(app),
        KeyAction::Focus if has_session_transport => {
            let pane = app.embed.as_ref().map(|embed| embed.pane_id.clone());
            close_stage(app);
            if let Some(pane) = pane {
                app.selected = Some(pane);
                focus_selected_seat(app, backend);
            }
        }
        KeyAction::SplitRight if has_session_transport => {
            if let Some(row) = app.selected_row() {
                split_selected_seat(app, &row.key, true);
            }
        }
        KeyAction::SplitBelow if has_session_transport => {
            if let Some(row) = app.selected_row() {
                split_selected_seat(app, &row.key, false);
            }
        }
        KeyAction::SwapIntoSlot if has_session_transport => {
            if let Some(row) = app.selected_row() {
                swap_selected_seat(app, &row.key);
            }
        }
        KeyAction::BreakToWindow if has_session_transport => {
            if let Some(row) = app.selected_row() {
                let row_key = row.key.clone();
                close_stage(app);
                break_seat_to_window(app, backend, &row_key);
            }
        }
        _ => {}
    }
}

/// Keys while an embedded seat is open. With input focus, every key but the
/// leave chord is re-encoded to the seat via `send-keys -H`; without it, the
/// panel navigates and the sidebar selection retargets the mirror. Dispatch
/// resolves through `app.keys` - see `crate::keys` for the
/// action table and why `release_input` and Esc get special treatment
/// below rather than living inside it like everything else.
fn handle_embed_key(app: &mut App, key: KeyEvent, backend: &Backend, embed_tx: &Sender<AppEvent>) {
    let input_focus = app.embed.as_ref().map(|e| e.input_focus).unwrap_or(false);
    if input_focus {
        if app.keys.effective(KeyAction::ReleaseInput).matches(&key) {
            if let Some(embed) = &mut app.embed {
                embed.input_focus = false;
            }
            app.status_line = sidebar_focus_hint(app);
            return;
        }
        if let Some(embed) = &app.embed
            && let Err(err) = crate::embed::send_key(&embed.pane_id, key)
        {
            app.status_line = format!("send failed: {err}");
        }
        return;
    }
    // Scrollback search (`/`) owns the keyboard from the
    // moment the prompt opens through an active n/N search - typing a
    // query must not fall through to j/k/digit navigation below, and this
    // is only reachable once input focus is released (checked above), the
    // same exclusivity the seat-input branch already has. The mirror keeps
    // rendering behind it; only key dispatch is intercepted.
    if app.embed.as_ref().is_some_and(|e| e.search.is_some()) {
        handle_embed_search_key(app, key);
        return;
    }
    // Esc always closes the embed - the same permanent, non-remappable
    // "cancel/back" convention this file uses for the row-drag cancel
    // above and the help-overlay dismiss in `handle_key`. `close_embed`'s
    // registry entry (default `q`, see the match below) is the
    // *remappable alias* for the same action; Esc is checked here, ahead
    // of the registry lookup, so a remap can never take away the panel's
    // guaranteed way back to the sidebar.
    if key.code == KeyCode::Esc {
        close_embed(app);
        return;
    }
    // Digits are a fixed jump table, never remapped (see `crate::keys`'s
    // module doc), checked before the registry so no remap can shadow them.
    if let KeyCode::Char(c) = key.code
        && c.is_ascii_digit()
        && c != '0'
    {
        // `c` is guaranteed an ascii '1'..='9' digit by the guard above, so
        // the byte subtraction is exact - no `Option`/`expect` needed.
        let n = (c as u8 - b'0') as usize;
        if app.select_nth_plot(n) {
            retarget_selected_plot(app, embed_tx);
        }
        return;
    }
    let Some(action) = app.keys.action_for(Scope::Embed, &key) else {
        return;
    };
    match action {
        KeyAction::Help => open_help(app),
        KeyAction::OpenView | KeyAction::InputFocus => {
            if let Some(embed) = &mut app.embed {
                embed.input_focus = true;
            }
            let label = embed_label(app);
            app.status_line = seat_input_hint(app, &label);
        }
        KeyAction::CloseEmbed => close_embed(app),
        KeyAction::Focus => {
            // Promote to the zero-overhead flagship path, then leave embed.
            let pane = app.embed.as_ref().map(|e| e.pane_id.clone());
            close_embed(app);
            if let Some(pane) = pane {
                app.selected = Some(pane);
                focus_selected_seat(app, backend);
            }
        }
        // `|`/`-`/`w` never take tmux's active-pane focus
        // (unlike `f`/`b`), so they leave the embed exactly as it is:
        // still open, still mirroring whatever pane it mirrored before -
        // a pane id, not a window position, so a split/swap elsewhere in
        // the tmux tree never invalidates it. See `swap_selected_seat`'s
        // doc for the full reasoning on `w` in particular.
        KeyAction::SplitRight => {
            if let Some(row) = app.selected_row() {
                split_selected_seat(app, &row.key, true);
            }
        }
        KeyAction::SplitBelow => {
            if let Some(row) = app.selected_row() {
                split_selected_seat(app, &row.key, false);
            }
        }
        KeyAction::SwapIntoSlot => {
            if let Some(row) = app.selected_row() {
                swap_selected_seat(app, &row.key);
            }
        }
        KeyAction::BreakToWindow => {
            // `break_seat_to_window` always ends by giving the seat real
            // tmux focus - it is the mouse equivalent of `f` - so close
            // the embed first exactly like the `Focus` arm above, or its
            // zoom would keep covering the real pane this just focused.
            if let Some(row) = app.selected_row() {
                let row_key = row.key.clone();
                close_embed(app);
                break_seat_to_window(app, backend, &row_key);
            }
        }
        KeyAction::MoveDown => {
            app.move_plot_selection(1);
            retarget_selected_plot(app, embed_tx);
        }
        KeyAction::MoveUp => {
            app.move_plot_selection(-1);
            retarget_selected_plot(app, embed_tx);
        }
        // Section cycle is a pure selection move (no Mode change), so -
        // unlike `goto_picker` or the right-click context menu, both new
        // overlay Modes only entered from the sidebar-only view - it
        // carries over into the embed-open, sidebar-navigating state the
        // same way move_down/move_up already do above.
        KeyAction::SectionNext => {
            app.cycle_section(1);
            retarget_embed(app, embed_tx);
        }
        KeyAction::SectionPrev => {
            app.cycle_section(-1);
            retarget_embed(app, embed_tx);
        }
        // `collapse` only ever does anything with an embed open (see
        // `App::toggle_inspector`), so this is the only scope it is
        // a member of - unlike the other sidebar-nav actions above, there
        // is nothing to mirror in `Scope::Normal`.
        KeyAction::Collapse => app.toggle_inspector(),
        // `search`: scrollback search over the mirror.
        // Only a `Scope::Embed` member - the sidebar-only `/` stays the
        // row filter (`filter`, `Scope::Normal`); the two never collide
        // because dispatch is already split on `app.embed.is_some()`.
        KeyAction::EmbedSearch => enter_embed_search(app),
        // Everything else in `KeyAction` belongs to `Scope::Normal` (or,
        // for `release_input`, is handled above before this lookup ever
        // runs) - `action_for(Scope::Embed, ..)` cannot produce it, but
        // the match must stay exhaustive against the whole enum.
        _ => {}
    }
}

/// The status-line hint shown once seat input focus is released back to
/// the sidebar - the `esc`/close half is hardcoded (see `handle_embed_key`'s
/// Esc handling), the `input_focus` half renders whatever key currently
/// grants it back.
fn sidebar_focus_hint(app: &App) -> String {
    seat_preview_hint(app, "seat")
}

fn embed_label(app: &App) -> String {
    app.embed
        .as_ref()
        .map(|embed| embed.label.clone())
        .unwrap_or_else(|| "seat".to_owned())
}

fn seat_input_hint(app: &App, label: &str) -> String {
    format!(
        "typing in {label} - {} returns to sidebar",
        app.keys.label(KeyAction::ReleaseInput)
    )
}

fn seat_type_hint(app: &App) -> String {
    let open = app.keys.label(KeyAction::OpenView);
    let input = app.keys.label(KeyAction::InputFocus);
    if open == input {
        open
    } else {
        format!("{open}/{input}")
    }
}

fn seat_preview_hint(app: &App, label: &str) -> String {
    format!(
        "previewing {label} - {} type, {} full focus, esc close",
        seat_type_hint(app),
        app.keys.label(KeyAction::Focus)
    )
}

/// Open the scrollback-search prompt over the embedded mirror. The next
/// key reaching `handle_embed_key` routes to [`handle_embed_search_key`]
/// instead (see the `embed.search.is_some()` check above it) until Esc or
/// an empty-query Enter closes it.
fn enter_embed_search(app: &mut App) {
    let Some(embed) = &mut app.embed else {
        return;
    };
    embed.start_search();
    app.status_line = embed.search_status().unwrap_or_default();
}

/// Keys while the scrollback-search prompt or an active search holds the
/// keyboard: composing the query (chars, Backspace, Enter, Esc) before the
/// first successful match, then `n`/`N`/Esc once [`crate::embed::Embed`]
/// reports [`crate::embed::EmbedSearch::Active`]. Delegates every state
/// transition to `Embed` itself (it owns the compiled pattern and the
/// grid) and only ever touches `app.status_line` here.
fn handle_embed_search_key(app: &mut App, key: KeyEvent) {
    let Some(embed) = &mut app.embed else {
        return;
    };
    let active = matches!(embed.search, Some(crate::embed::EmbedSearch::Active { .. }));
    if active {
        match key.code {
            KeyCode::Char('n') => embed.search_advance(true),
            KeyCode::Char('N') => embed.search_advance(false),
            KeyCode::Esc => embed.close_search(),
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Esc => embed.close_search(),
            KeyCode::Enter => embed.execute_search(),
            KeyCode::Backspace => embed.search_backspace(),
            KeyCode::Char(c) => embed.search_push(c),
            _ => {}
        }
    }
    app.status_line = app
        .embed
        .as_ref()
        .and_then(|e| e.search_status())
        .unwrap_or_else(|| sidebar_focus_hint(app));
}

/// Enter the help overlay, zooming the field pane so the popup gets the
/// full window instead of the sidebar strip (an embed already holds the
/// zoom; the state layer tracks whose it is).
fn open_help(app: &mut App) {
    if app.enter_help() {
        let _ = Backend::set_zoom(&app.field_pane_id, true);
    }
}

/// Dismiss the help overlay, releasing only a zoom help itself took.
fn dismiss_help(app: &mut App) {
    if app.leave_help() {
        let _ = Backend::set_zoom(&app.field_pane_id, false);
    }
}

/// Mouse input, resolved against the hitmap the last frame recorded.
/// Clicks select; a seat row also opens on release so a drag can be
/// disambiguated from a click - see
/// [`RowDrag`]/[`resolve_row_drag`]), a run/ask row opens on a second
/// click; a seat row can also be dragged onto an open panel to open/
/// retarget it (center) or real-split it (an edge band);
/// drag-select and double-click inside the grid copy grid text to the
/// clipboard; the wheel moves the sidebar selection, or -
/// inside an open embed's grid - scrolls its local scrollback, or reaches
/// the seat directly when it has mouse reporting on;
/// right-click opens a seat row's context menu.
fn handle_mouse(app: &mut App, mouse: MouseEvent, backend: &Backend, embed_tx: &Sender<AppEvent>) {
    // The help overlay swallows the mouse like it does keys: any click
    // dismisses it.
    if app.mode == Mode::Help {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            dismiss_help(app);
        }
        return;
    }
    // Run detail fills the pane; no sidebar rows are on screen to hit.
    if matches!(app.mode, Mode::RunDetail(_)) {
        return;
    }
    // These modal prompts take over the sidebar body; the mouse has
    // nothing well-defined to hit while one is open.
    if matches!(
        app.mode,
        Mode::SpawnPicker { .. }
            | Mode::WorktreeName { .. }
            | Mode::ConfirmKill(_)
            | Mode::GotoPicker { .. }
    ) {
        return;
    }
    // The context menu is keyboard-driven (j/k + enter); the mouse's only
    // role is dismissal - any click closes it, matching the help overlay.
    if matches!(app.mode, Mode::ContextMenu { .. }) {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
        ) {
            app.mode = Mode::Normal;
        }
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            handle_left_click(app, mouse.column, mouse.row, backend, embed_tx);
        }
        MouseEventKind::Down(MouseButton::Right) => {
            handle_right_click(app, mouse.column, mouse.row);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            handle_left_drag(app, mouse.column, mouse.row);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            handle_left_up(app, embed_tx);
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            handle_wheel(app, mouse, embed_tx);
        }
        MouseEventKind::Moved => {
            handle_hover(app, mouse.column, mouse.row, embed_tx);
        }
        // The other buttons are deliberately ignored.
        _ => {}
    }
}

/// Hover a seat row while an embedded panel is open: preview that seat in
/// the panel without an extra click. This only runs when the panel is in
/// sidebar/view focus; while the seat owns input, ordinary pointer motion
/// must not steal keystrokes by retargeting the live terminal.
fn handle_hover(app: &mut App, x: u16, y: u16, embed_tx: &Sender<AppEvent>) {
    if app.embed.as_ref().is_some_and(|embed| !embed.input_focus)
        && let Some(row) = app.hit.row_at(x, y).cloned()
        && row.section == Section::Plots
    {
        if app.selected_plot_id.as_deref() != Some(row.key.as_str()) {
            select_plot_id(app, &row.key);
            retarget_selected_plot(app, embed_tx);
        }
        return;
    }
    handle_hover_with_retarget(app, x, y, |app| retarget_embed(app, embed_tx));
}

fn handle_hover_with_retarget(app: &mut App, x: u16, y: u16, mut retarget: impl FnMut(&mut App)) {
    let Some(embed) = app.embed.as_ref() else {
        return;
    };
    if embed.input_focus {
        return;
    }
    let Some(row) = app.hit.row_at(x, y).cloned() else {
        return;
    };
    if row.section != Section::Seats || app.selected.as_deref() == Some(row.key.as_str()) {
        return;
    }
    let Some(seat) = app.seats.get(&row.key) else {
        return;
    };
    if seat.is_split() {
        return;
    }
    app.selected = Some(row.key);
    retarget(app);
}

/// Right-click a seat row: open the context menu - a context-dependent
/// subset of [`ContextAction`] (see [`App::context_menu_actions`]) -
/// selecting the row first so the menu's actions operate on it. A non-seat
/// row just selects (no menu - runs/asks have no seat actions to offer). A
/// no-op on empty space, and while an embed is open:
/// [`Mode::ContextMenu`] is only entered from the sidebar-only view - the
/// same scoping as [`enter_goto_picker`] and for the same reason (every
/// overlay-entering key in this file keeps that invariant so `handle_key`'s
/// `embed.is_some()` short-circuit never has to arbitrate between an open
/// embed and a modal Mode).
fn handle_right_click(app: &mut App, x: u16, y: u16) {
    if app.embed.is_some() {
        return;
    }
    let Some(row) = app.hit.row_at(x, y).cloned() else {
        return;
    };
    if row.section != Section::Seats {
        app.selected = Some(row.key);
        return;
    }
    app.selected = Some(row.key.clone());
    app.mode = Mode::ContextMenu {
        row_key: row.key,
        cursor: 0,
    };
}

/// One left press: inside an open embed's grid, (re)grant seat input focus
/// and start a fresh selection anchored at the clicked cell (a second
/// press within [`crate::embed::DOUBLE_CLICK_WINDOW`] on the same or an
/// adjacent cell selects the token there instead - see
/// [`crate::embed::is_double_click`]), and
/// disjoint from everything below it (a drag starting in the grid is text
/// selection, never a row-drag). Outside the grid, a seat row *arms*
/// instead of opening immediately: [`handle_left_up`]
/// resolves an un-dragged press to the click-to-open action
/// ([`open_seat_row`]), and [`handle_left_drag`] promotes an armed press
/// into a row-drag on the first `Drag` event - the disambiguation a drag
/// gesture starting on the same press needs. Runs/asks rows are unaffected:
/// they keep the select-then-click-again-to-activate shape they always had
/// (mirroring Enter), in both embed states.
fn handle_left_click(
    app: &mut App,
    x: u16,
    y: u16,
    backend: &Backend,
    embed_tx: &Sender<AppEvent>,
) {
    if app.stage_open {
        if let Some(activity) = app.hit.activity_at(x, y).cloned() {
            if app.select_plot_activity(activity) {
                sync_plot_transport(app, embed_tx);
            }
            return;
        }
        if let Some(tab) = app.hit.inspector_tab_at(x, y) {
            app.select_inspector_tab(tab);
            return;
        }
    }
    if app.embed.is_some() {
        if app.hit.in_embed_grid(x, y) {
            let now = Instant::now();
            let double = app.embed_last_click.is_some_and(|(last, lx, ly)| {
                crate::embed::is_double_click(
                    now.saturating_duration_since(last),
                    x.abs_diff(lx),
                    y.abs_diff(ly),
                )
            });
            app.embed_last_click = Some((now, x, y));
            begin_embed_selection(app, x, y, double);
            if let Some(embed) = &mut app.embed {
                embed.input_focus = true;
            }
            let label = embed_label(app);
            app.status_line = seat_input_hint(app, &label);
            return;
        }
        // Leaving the grid drops the double-click anchor so a later
        // in-grid click never compares itself against a stale position.
        app.embed_last_click = None;
    }
    let Some(row) = app.hit.row_at(x, y).cloned() else {
        return;
    };
    if row.section == Section::Plots {
        select_plot_id(app, &row.key);
        if let Some(embed) = &mut app.embed {
            embed.input_focus = false;
        }
        retarget_selected_plot(app, embed_tx);
        return;
    }
    if row.section == Section::Seats {
        app.selected = Some(row.key.clone());
        app.row_drag = Some(RowDrag::Armed { pane_id: row.key });
        return;
    }
    if app.embed.is_some() {
        // Non-seat row while an embed is open: unchanged parity behavior -
        // release input focus and reselect (`retarget_embed` itself is a
        // no-op for a non-seat row; kept for parity with the pre-pass-2
        // path rather than special-cased away).
        if let Some(embed) = &mut app.embed {
            embed.input_focus = false;
        }
        app.selected = Some(row.key);
        app.status_line = sidebar_focus_hint(app);
        retarget_embed(app, embed_tx);
        return;
    }
    if app.selected.as_deref() == Some(row.key.as_str()) {
        activate_selection(app, backend);
    } else {
        app.selected = Some(row.key);
    }
}

/// Extend the active grid selection on a `Drag` event, or - if a seat row
/// is armed or already being dragged - advance the row-drag state machine
/// instead: promote `Armed` to `Dragging` on the first such
/// event, refresh the hover zone on every later one. The two paths are
/// mutually exclusive by construction: `handle_left_click` only arms
/// `app.row_drag` for a press that started on a sidebar row, never inside
/// the grid, so a row-drag in flight never falls through to
/// `handle_embed_drag` below.
fn handle_left_drag(app: &mut App, x: u16, y: u16) {
    if let Some(state) = app.row_drag.take() {
        let hover = app
            .hit
            .panel
            .and_then(|panel| ui::drop_zone_at(panel, x, y));
        app.status_line = if app.hit.panel.is_none() {
            // No embed open to drop against at all (design decision 2):
            // the panel is the only drop surface, so there is nothing to
            // highlight - just tell the operator why nothing is happening.
            "open a seat to drop-split against".to_owned()
        } else {
            String::new()
        };
        app.row_drag = Some(state.advance(hover));
        return;
    }
    handle_embed_drag(app, x, y);
}

/// Left button release: resolve an in-flight row-drag via
/// [`resolve_row_drag`] - `Open` for a plain click or a center drop
/// ([`open_seat_row`]), `Split` for an edge drop ([`drop_split_seat`]),
/// `Cancel` for a drop outside the panel (or nowhere for the drag to land
/// at all) - or, with no row-drag in flight, fall through to the unchanged
/// embedded grid-selection release ([`handle_embed_release`]).
fn handle_left_up(app: &mut App, embed_tx: &Sender<AppEvent>) {
    let Some(state) = app.row_drag.take() else {
        handle_embed_release(app);
        return;
    };
    let pane_id = state.pane_id().to_owned();
    match resolve_row_drag(&state) {
        RowDropOutcome::Open => open_seat_row(app, pane_id, embed_tx),
        RowDropOutcome::Split { horizontal, before } => {
            drop_split_seat(app, &pane_id, horizontal, before)
        }
        RowDropOutcome::Cancel => app.status_line.clear(),
    }
}

/// Open (or retarget) a seat row's embed: the click-to-open action, shared
/// by a plain click (`handle_left_up` with no intervening drag) and a
/// row-drag's center drop (design decision: "same effect as clicking its
/// row"). With no embed open, this is the single-click path
/// ([`open_embed_for_selected`], which also zooms and applies
/// the split-seat guard); with one already open, it releases its input
/// focus and retargets the mirror ([`retarget_embed`]) - the embedded-pane
/// sidebar-row-while-embedded path, previously reached on `Down` and now
/// reached on release like everything else here.
fn open_seat_row(app: &mut App, row_key: String, embed_tx: &Sender<AppEvent>) {
    if app.embed.is_some() {
        if let Some(embed) = &mut app.embed {
            embed.input_focus = false;
        }
        app.selected = Some(row_key);
        app.status_line = sidebar_focus_hint(app);
        retarget_embed(app, embed_tx);
        return;
    }
    app.selected = Some(row_key);
    open_embed_for_selected(app, embed_tx);
}

/// Resolve a row-drag's edge drop: joining an already split-in seat again
/// would be meaningless (it is already a real pane next to the panel), so
/// this guards it the same way the context menu's split entries never
/// offer themselves for one (design decision 4) - a status hint, no tmux
/// call. Otherwise joins via [`join_seat_into_slot`] and, only on success,
/// closes the embed through the ordinary close path so the zoom releases
/// and the fresh split is immediately visible (design decision 3: an edge
/// drop is a deliberate request for a real split, not a preview).
fn drop_split_seat(app: &mut App, pane_id: &str, horizontal: bool, before: bool) {
    if app.seats.get(pane_id).is_some_and(|seat| seat.is_split()) {
        app.status_line = "already split in".to_owned();
        return;
    }
    if join_seat_into_slot(app, pane_id, horizontal, before) {
        close_embed(app);
    }
}

/// Extend the active grid selection to the drag's current cell. A no-op
/// outside an open embed's grid; [`crate::embed::Embed::update_selection`]
/// itself no-ops without an anchor, so a drag that starts on a sidebar row
/// and wanders over the grid never picks up a stale selection.
fn handle_embed_drag(app: &mut App, x: u16, y: u16) {
    if !app.hit.in_embed_grid(x, y) {
        return;
    }
    let Some(grid) = app.hit.embed_grid else {
        return;
    };
    let Some(embed) = &mut app.embed else {
        return;
    };
    if let Some((col, row)) = crate::embed::screen_to_local(grid, embed.cols, embed.rows, x, y) {
        embed.update_selection(col, row);
    }
}

/// Left button release: copy a non-empty active grid selection to the
/// system clipboard and toast the result. A plain click (no drag, no
/// double-click) always resolves to an empty selection
/// (`Embed::selection_text` filters it out), so this is a pure no-op for
/// the ordinary "click to focus" gesture - copying never surprises a user
/// who only meant to click.
fn handle_embed_release(app: &mut App) {
    let Some(embed) = &app.embed else {
        return;
    };
    let Some(text) = embed.selection_text() else {
        return;
    };
    match crate::embed::copy_to_clipboard(&text) {
        Ok(()) => {
            app.status_line = format!("copied {} chars to clipboard", text.chars().count());
        }
        Err(err) => app.status_line = format!("copy failed: {err}"),
    }
}

/// Wheel notch, resolved against whichever surface the pointer is over.
/// Inside an open embed's grid: scroll the mirror's local scrollback, or,
/// when the seat has mouse reporting on (alt-screen TUIs like pi/vim/less),
/// forward the notch to the seat as a real mouse escape sequence instead.
/// That is herdr's documented passthrough rule: an app that already
/// redraws its own viewport on wheel input would otherwise see the view
/// double-move. Everywhere else: move the sidebar selection, same as v0.
fn handle_wheel(app: &mut App, mouse: MouseEvent, embed_tx: &Sender<AppEvent>) {
    let up = mouse.kind == MouseEventKind::ScrollUp;
    if app.embed.is_some() && app.hit.in_embed_grid(mouse.column, mouse.row) {
        forward_or_scroll_embed(app, mouse.column, mouse.row, up);
        return;
    }
    if app.stage_open && app.hit.in_inspector(mouse.column, mouse.row) {
        app.inspector_scroll = scroll_offset(app.inspector_scroll, up);
        return;
    }
    if app.stage_open && app.hit.in_main(mouse.column, mouse.row) {
        app.main_scroll = scroll_offset(app.main_scroll, up);
        return;
    }
    let delta = if up { -1 } else { 1 };
    app.move_selection(delta);
    if app.embed.is_some() {
        retarget_embed(app, embed_tx);
    }
}

fn scroll_offset(offset: usize, up: bool) -> usize {
    if up {
        offset.saturating_sub(crate::embed::WHEEL_SCROLL_LINES as usize)
    } else {
        offset.saturating_add(crate::embed::WHEEL_SCROLL_LINES as usize)
    }
}

/// One wheel notch already known to be over the embedded grid: forward it
/// to the seat when mouse reporting is on, otherwise scroll the local
/// scrollback view (herdr's `mouse_scroll_lines = 3` convention).
fn forward_or_scroll_embed(app: &mut App, x: u16, y: u16, up: bool) {
    let Some(grid) = app.hit.embed_grid else {
        return;
    };
    let Some(embed) = &mut app.embed else {
        return;
    };
    if embed.mouse_reporting() {
        if let Some((col, row)) = crate::embed::screen_to_local(grid, embed.cols, embed.rows, x, y)
        {
            let _ = embed.send_wheel(col, row, up);
        }
        return;
    }
    let lines = if up {
        crate::embed::WHEEL_SCROLL_LINES
    } else {
        -crate::embed::WHEEL_SCROLL_LINES
    };
    embed.scroll_lines(lines);
}

/// Begin a new grid selection at a screen cell already known to be inside
/// the embed's rendered rectangle. A no-op if the cell falls in the
/// centering margin `draw_embed` leaves around a seat grid smaller than
/// its panel (`screen_to_local` returns `None` there).
fn begin_embed_selection(app: &mut App, x: u16, y: u16, double: bool) {
    let Some(grid) = app.hit.embed_grid else {
        return;
    };
    let Some(embed) = &mut app.embed else {
        return;
    };
    if let Some((col, row)) = crate::embed::screen_to_local(grid, embed.cols, embed.rows, x, y) {
        embed.begin_selection(col, row, double);
    }
}

fn handle_filter_key(app: &mut App, key: KeyEvent, mut text: String) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Enter => {
            // Jump to the first match, then leave filter mode.
            app.selected = app.rows().first().map(|row| row.key.clone());
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            text.pop();
            app.mode = Mode::Filter(text);
        }
        KeyCode::Char(c) => {
            text.push(c);
            app.mode = Mode::Filter(text);
        }
        _ => {}
    }
}

/// Enter the spawn picker, (re)building its candidate list from the seat
/// config, discovered projects, their `git worktree`s, and registry
/// recents. Always refreshes, even when re-entered
/// after `Esc` from [`Mode::WorktreeName`], so a project added mid-session
/// or a worktree just created shows up.
fn enter_spawn_picker(app: &mut App, options: &Options) {
    let cfg = seat::config::load(options.state_dir.as_deref());
    let registry_entries = crate::registry::load(&crate::registry::registry_path(
        options.state_dir.as_deref(),
    ));
    let projects = seat::discover_projects(&cfg);
    let recents = seat::RegistrySource::new(registry_entries).candidates();
    let projects_candidates = seat::ProjectSource::new(&cfg).candidates();
    let worktrees = seat::WorktreeSource::new(projects).candidates();
    app.spawn_candidates = seat::merge(recents, projects_candidates, worktrees);
    app.mode = Mode::SpawnPicker {
        query: String::new(),
        cursor: 0,
    };
}

fn handle_spawn_picker_key(
    app: &mut App,
    key: KeyEvent,
    query: String,
    cursor: usize,
    options: &Options,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Up => {
            app.mode = Mode::SpawnPicker {
                cursor: cursor.saturating_sub(1),
                query,
            };
        }
        KeyCode::Down => {
            let cursor = next_cursor(app.filtered_candidates(&query).len(), cursor);
            app.mode = Mode::SpawnPicker { query, cursor };
        }
        KeyCode::Char('k') if ctrl => {
            app.mode = Mode::SpawnPicker {
                cursor: cursor.saturating_sub(1),
                query,
            };
        }
        KeyCode::Char('j') if ctrl => {
            let cursor = next_cursor(app.filtered_candidates(&query).len(), cursor);
            app.mode = Mode::SpawnPicker { query, cursor };
        }
        KeyCode::Backspace => {
            let mut query = query;
            query.pop();
            app.mode = Mode::SpawnPicker { query, cursor: 0 };
        }
        KeyCode::Enter => activate_spawn_picker_selection(app, &query, cursor, options),
        KeyCode::Char(c) if !ctrl => {
            let mut query = query;
            query.push(c);
            app.mode = Mode::SpawnPicker { query, cursor: 0 };
        }
        _ => {}
    }
}

fn next_cursor(len: usize, cursor: usize) -> usize {
    if len == 0 {
        0
    } else {
        (cursor + 1).min(len - 1)
    }
}

/// Enter the goto-seat picker (`g`): fuzzy-jump to an existing seat by
/// name, repo, or window/session name (see [`App::goto_candidates`]).
/// Distinct from [`enter_spawn_picker`]'s `n` - this narrows live seats and
/// never spawns anything.
fn enter_goto_picker(app: &mut App) {
    app.mode = Mode::GotoPicker {
        query: String::new(),
        cursor: 0,
    };
}

/// Keys inside the goto picker: typing narrows the fuzzy filter, up/down
/// (or the ctrl-j/ctrl-k chords the spawn picker also accepts) moves the
/// cursor, enter opens the seat under it in the embedded panel, esc cancels
/// back to the sidebar with the selection untouched.
fn handle_goto_picker_key(
    app: &mut App,
    key: KeyEvent,
    query: String,
    cursor: usize,
    embed_tx: &Sender<AppEvent>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Up => {
            app.mode = Mode::GotoPicker {
                cursor: cursor.saturating_sub(1),
                query,
            };
        }
        KeyCode::Down => {
            let cursor = next_cursor(app.goto_candidates(&query).len(), cursor);
            app.mode = Mode::GotoPicker { query, cursor };
        }
        KeyCode::Char('k') if ctrl => {
            app.mode = Mode::GotoPicker {
                cursor: cursor.saturating_sub(1),
                query,
            };
        }
        KeyCode::Char('j') if ctrl => {
            let cursor = next_cursor(app.goto_candidates(&query).len(), cursor);
            app.mode = Mode::GotoPicker { query, cursor };
        }
        KeyCode::Backspace => {
            let mut query = query;
            query.pop();
            app.mode = Mode::GotoPicker { query, cursor: 0 };
        }
        // This mode is only ever entered without an embed open (see
        // `Mode::GotoPicker`'s doc), so the seat picked here always opens a
        // fresh embed - there is nothing yet to retarget.
        KeyCode::Enter => {
            let pane_id = app
                .goto_candidates(&query)
                .get(cursor)
                .map(|seat| seat.pane_id.clone());
            app.mode = Mode::Normal;
            if let Some(pane_id) = pane_id {
                app.selected = Some(pane_id);
                open_embed_for_selected(app, embed_tx);
            }
        }
        KeyCode::Char(c) if !ctrl => {
            let mut query = query;
            query.push(c);
            app.mode = Mode::GotoPicker { query, cursor: 0 };
        }
        _ => {}
    }
}

/// Enter activates the candidate under the cursor. An empty filtered list
/// falls back to the pre-picker free-entry behavior: a query starting with
/// `/` or `~` spawns at that literal (expanded) path.
fn activate_spawn_picker_selection(app: &mut App, query: &str, cursor: usize, options: &Options) {
    let candidate = app.filtered_candidates(query).get(cursor).copied().cloned();
    if let Some(candidate) = candidate {
        activate_candidate(app, candidate, options);
        return;
    }
    let trimmed = query.trim();
    if !(trimmed.starts_with('/') || trimmed.starts_with('~')) {
        app.status_line = "no match; enter a / or ~ path to spawn there".to_owned();
        return;
    }
    let path = expand_tilde(trimmed);
    if !Path::new(&path).is_dir() {
        app.status_line = format!("not a directory: {path}");
        return;
    }
    let _ = spawn_seat_at(app, &path, options);
}

/// Activate a picked candidate: real paths ([`CandidateKind::ProjectRoot`],
/// [`CandidateKind::Worktree`], [`CandidateKind::Recent`]) spawn directly;
/// [`CandidateKind::NewWorktree`] collects a name first. Its canonical
/// `project_root` identifies the exact source repository even when several
/// configured repositories share the same display basename.
fn activate_candidate(app: &mut App, candidate: Candidate, options: &Options) {
    match candidate.kind {
        CandidateKind::ProjectRoot | CandidateKind::Worktree | CandidateKind::Recent => {
            let _ = spawn_seat_at(app, &candidate.path, options);
        }
        CandidateKind::NewWorktree => {
            app.mode = Mode::WorktreeName {
                project: candidate.project_root,
                buffer: String::new(),
            };
        }
    }
}

fn handle_worktree_name_key(
    app: &mut App,
    key: KeyEvent,
    project: String,
    buffer: String,
    options: &Options,
) {
    match key.code {
        KeyCode::Esc => enter_spawn_picker(app, options),
        KeyCode::Backspace => {
            let mut buffer = buffer;
            buffer.pop();
            app.mode = Mode::WorktreeName { project, buffer };
        }
        KeyCode::Enter => {
            let cfg = seat::config::load(options.state_dir.as_deref());
            match seat::worktree::create(Path::new(&project), &buffer, &cfg) {
                Ok(path) => {
                    let _ = spawn_seat_at(app, &path.to_string_lossy(), options);
                }
                Err(err) => {
                    app.status_line = format!("worktree create failed: {err}");
                    app.mode = Mode::WorktreeName { project, buffer };
                }
            }
        }
        KeyCode::Char(c) => {
            let mut buffer = buffer;
            buffer.push(c);
            app.mode = Mode::WorktreeName { project, buffer };
        }
        _ => {}
    }
}

/// `~/` expands against `$HOME`; any other path passes through unchanged.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{rest}", std::env::var("HOME").unwrap_or_default())
    } else {
        path.to_owned()
    }
}

fn handle_confirm_kill_key(app: &mut App, key: KeyEvent, row_key: String, backend: &Backend) {
    if key.code != KeyCode::Char('y') {
        app.mode = Mode::Normal;
        app.status_line.clear();
        return;
    }
    app.mode = Mode::Normal;
    let Some(seat) = app.seats.get(&row_key).cloned() else {
        return;
    };
    let name = seat
        .display_name(app.field_session_id.as_deref())
        .to_owned();
    // A seat sharing the field's own session is a window inside it (the
    // slot or an adopted window); everything else is a whole-session seat.
    let result = if app.field_session_id.as_deref() == Some(seat.session_id.as_str()) {
        backend.kill_pane(&seat.pane_id)
    } else {
        Backend::kill_session_named(&seat.session_name)
    };
    match result {
        Ok(()) => app.status_line = format!("killed {name}"),
        Err(err) => app.status_line = format!("kill failed: {err}"),
    }
    app.prune_selection();
    app.wants_reconcile = true;
}

/// Keys inside the right-click context menu: j/k moves the cursor over
/// [`App::context_menu_actions`]'s currently visible list for `row_key`
/// (clamped, not wrapping - see [`move_context_cursor`], which now takes
/// that list's length since the menu is context-dependent),
/// enter dispatches the action under it to the exact same handler the
/// equivalent normal-mode key already uses (or a dedicated split/break/
/// return/swap handler for the new plumbing actions), esc/q cancels. Dispatching
/// re-selects `row_key` first since the handlers below all read
/// `app.selected_row()`, not a menu-specific target.
fn handle_context_menu_key(
    app: &mut App,
    key: KeyEvent,
    row_key: String,
    cursor: usize,
    backend: &Backend,
    options: &Options,
    embed_tx: &Sender<AppEvent>,
) {
    let visible_len = app.context_menu_actions(&row_key).len();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => {
            app.mode = Mode::ContextMenu {
                row_key,
                cursor: move_context_cursor(cursor, 1, visible_len),
            };
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.mode = Mode::ContextMenu {
                row_key,
                cursor: move_context_cursor(cursor, -1, visible_len),
            };
        }
        KeyCode::Enter => {
            let action = ContextAction::at(cursor, app.context_menu_actions(&row_key));
            app.mode = Mode::Normal;
            app.selected = Some(row_key.clone());
            match action {
                // `Open` routes through the same choke point the single-
                // click, Enter, and goto-picker "open" paths all use, so
                // the split-seat guard (select the real pane, never mirror
                // it) lives in exactly one place - see
                // `open_embed_for_selected`.
                ContextAction::Open => open_embed_for_selected(app, embed_tx),
                ContextAction::Kill => request_kill_selected(app),
                ContextAction::Relaunch => launch_or_relaunch_selected(app, options),
                ContextAction::SplitRight => split_selected_seat(app, &row_key, true),
                ContextAction::SplitBelow => split_selected_seat(app, &row_key, false),
                ContextAction::BreakToWindow => break_seat_to_window(app, backend, &row_key),
                ContextAction::Return => return_seat_to_window(app, &row_key),
                // The context menu only ever opens with no embed present
                // (`handle_right_click` returns early otherwise), so unlike
                // the keyboard `w` binding's own doc, there is no embed
                // state to reason about here.
                ContextAction::SwapIntoSlot => swap_selected_seat(app, &row_key),
                // Per the design doc's explicit handler list, "spawn-here"
                // routes to the same generic picker `n` opens - it is not
                // scoped to the right-clicked seat's own path.
                ContextAction::SpawnHere => enter_spawn_picker(app, options),
            }
        }
        _ => {}
    }
}

/// Split the seat at `row_key` into the field window next to the current
/// slot pane (`horizontal` picks `join-pane -h`/"split right" vs `-v`/
/// "split below"; the context menu only ever joins on the trailing side, so
/// `before` is always `false` here - a row-drag's edge drop is the only
/// caller that passes `true`, via [`drop_split_seat`]). Thin wrapper over
/// [`join_seat_into_slot`], which carries the shared plumbing and its own
/// no-op guards.
fn split_selected_seat(app: &mut App, row_key: &str, horizontal: bool) {
    join_seat_into_slot(app, row_key, horizontal, false);
}

/// Shared plumbing for "make this seat a real tmux split of the slot":
/// freezes an adopted seat's name (a join is about to replace the window/
/// session name its label falls back to), guards against splitting the
/// slot with itself, then joins - the exact sequence `split_selected_seat`
/// used to inline, now factored so a row-drag's edge drop
/// ([`drop_split_seat`]) can reuse it for every `(horizontal, before)`
/// combination [`crate::state::DropZone::join_flags`] produces, not just
/// the context menu's trailing-side two. A no-op with a status message
/// when there is no slot to split against, when the target already *is*
/// the slot (splitting a pane with itself is meaningless - reachable by
/// splitting the slot's own `(shell)` row, since neither the context menu
/// nor a row-drag offers a split action for an already-split seat; see
/// [`ContextAction::visible_for`] and [`drop_split_seat`]'s own guard), or
/// when the target is already split in - the context menu and row-drag
/// both already refuse to offer a split action for one, but the `|` and `-`
/// keys dispatch straight from the selected row with no such
/// filter, so a split-in seat is newly reachable here and needs its own
/// explicit no-op rather than re-joining a pane that is already joined.
/// Returns whether the join succeeded, so a caller that wants to react to
/// success (closing the embed, in the row-drag case) can.
fn join_seat_into_slot(app: &mut App, row_key: &str, horizontal: bool, before: bool) -> bool {
    let Some(seat) = app.seats.get(row_key) else {
        return false;
    };
    if seat.is_split() {
        app.status_line = "already split in".to_owned();
        return false;
    }
    let pane = seat.pane_id.clone();
    if seat.name.is_empty() {
        let label = seat
            .display_name(app.field_session_id.as_deref())
            .to_owned();
        let _ = Backend::stamp_seat_name(&pane, &label);
    }
    let Some(slot) = app.focused_seat().map(|s| s.pane_id.clone()) else {
        app.status_line = "no focused-seat slot pane; is the field window intact?".into();
        return false;
    };
    if pane == slot {
        app.status_line = "already in the slot".to_owned();
        return false;
    }
    match Backend::join_seat_split(&pane, &slot, &app.field_pane_id, horizontal, before) {
        Ok(()) => {
            app.status_line = String::new();
            app.wants_reconcile = true;
            true
        }
        Err(err) => {
            app.status_line = format!("split failed: {err}");
            false
        }
    }
}

/// "Return to its window": break a split-in seat back out to a fresh
/// background window without also focusing it (unlike
/// [`break_seat_to_window`], which does). The context menu only offers this
/// for seats currently split in (see [`ContextAction::visible_for`]).
fn return_seat_to_window(app: &mut App, row_key: &str) {
    let Some(seat) = app.seats.get(row_key) else {
        return;
    };
    let (pane, name) = (
        seat.pane_id.clone(),
        seat.display_name(app.field_session_id.as_deref())
            .to_owned(),
    );
    match Backend::break_seat_out(&pane, &name) {
        Ok(_window_id) => {
            app.status_line = String::new();
            app.wants_reconcile = true;
        }
        Err(err) => app.status_line = format!("return failed: {err}"),
    }
}

/// "Break to window": the mouse equivalent of `f`, for a seat that may
/// currently be split into the field window. An ordinary windowed seat
/// takes the exact same path `f` already does
/// ([`focus_selected_seat`]/[`activate_selection`]). A split seat is broken
/// out first, then swapped into the slot the same way `activate_selection`
/// would for any other windowed seat - but using the fresh window id
/// [`Backend::break_seat_out`] just reported, not `app.seats`' cached
/// `window_id`, as the swap's rename target. The cache has not yet
/// reconciled the break-pane this function itself just issued, so reading
/// it here would target a stale (or, worse, the field's own) window;
/// the id `break_seat_out` returns is live by construction.
fn break_seat_to_window(app: &mut App, backend: &Backend, row_key: &str) {
    let Some(seat) = app.seats.get(row_key) else {
        return;
    };
    if !seat.is_split() {
        app.selected = Some(row_key.to_owned());
        focus_selected_seat(app, backend);
        return;
    }
    let (pane, name) = (
        seat.pane_id.clone(),
        seat.display_name(app.field_session_id.as_deref())
            .to_owned(),
    );
    let window_id = match Backend::break_seat_out(&pane, &name) {
        Ok(id) => id,
        Err(err) => {
            app.status_line = format!("break failed: {err}");
            return;
        }
    };
    let Some(displaced) = app.focused_seat() else {
        app.status_line = "no focused-seat slot pane; is the field window intact?".into();
        app.wants_reconcile = true;
        return;
    };
    let slot = displaced.pane_id.clone();
    let displaced_label = if displaced.name.is_empty() {
        "shell".to_owned()
    } else {
        displaced.name.clone()
    };
    let window_name = format!("seat:{name}");
    match backend.focus_seat(&pane, &slot, &window_id, &window_name, &displaced_label) {
        Ok(()) => app.status_line = String::new(),
        Err(err) => app.status_line = format!("focus failed: {err}"),
    }
    app.wants_reconcile = true;
}

/// Give a split-in seat's real pane tmux's active-pane focus directly,
/// without opening a VT mirror of it or running it through the
/// swap-into-slot dance meant for a seat living in its own window. Shared
/// by "open" ([`open_embed_for_selected`]) and `f`/"break to window"'s
/// already-normal-seat check ([`activate_selection`]) - both mean the same
/// thing for a pane that is already visible next to the field UI.
fn focus_split_seat(app: &mut App, pane_id: &str) {
    match Backend::select_pane(pane_id) {
        Ok(()) => app.status_line = String::new(),
        Err(err) => app.status_line = format!("select failed: {err}"),
    }
}

/// "Swap into slot": trade the seat at `row_key`'s pane
/// with the current focused-seat slot occupant, without also taking
/// tmux's active-pane focus there. Half of what `f`/[`activate_selection`]
/// does for a same-session windowed seat - the swap, not the
/// select-window/select-pane focus tail - via
/// [`Backend::swap_seat_into_slot`], which [`Backend::focus_seat`] itself
/// now also calls for exactly that shared half (see its doc). The sidebar
/// keeps focus and no embed is opened or retargeted: this never moves
/// `app.selected` or touches `app.embed`.
///
/// Works for a split-in seat too, with no extra branching: `swap-pane -d`
/// trades panes regardless of whether they already share the field
/// window (the split case, where `vacated_window`/`vacated_window_name`
/// below resolve to the field window's own id/name, so the rename guard
/// in `swap_seat_into_slot` is a no-op - a split seat's window is never
/// `seat:*`-named) or live in separate windows (the ordinary case, where
/// the rename fires exactly as it does for `f`).
///
/// If an embed is open, it mirrors a pane id via a pipe-pane fifo, not a
/// window/slot position - `swap-pane` never changes a pane's id or tty, so
/// the mirror keeps following its pane wherever the swap moves it. That
/// holds even when the swapped seat is the one currently embedded: the VT
/// panel is drawn by the ratatui app into the field's own (zoomed) pane,
/// entirely independent of which real tmux window its mirrored pane now
/// sits in. Nothing here needs to touch the embed.
fn swap_selected_seat(app: &mut App, row_key: &str) {
    let Some(seat) = app.seats.get(row_key) else {
        return;
    };
    let pane = seat.pane_id.clone();
    let (vacated_window, vacated_name) = (seat.window_id.clone(), seat.window_name.clone());
    let Some(displaced) = app.focused_seat() else {
        app.status_line = "no focused-seat slot pane; is the field window intact?".into();
        return;
    };
    let slot = displaced.pane_id.clone();
    if pane == slot {
        app.status_line = "already in the slot".to_owned();
        return;
    }
    let displaced_label = if displaced.name.is_empty() {
        "shell".to_owned()
    } else {
        displaced.name.clone()
    };
    match Backend::swap_seat_into_slot(
        &pane,
        &slot,
        &vacated_window,
        &vacated_name,
        &displaced_label,
    ) {
        Ok(()) => {
            app.status_line = String::new();
            app.wants_reconcile = true;
        }
        Err(err) => app.status_line = format!("swap failed: {err}"),
    }
}

/// Keys with no embed open and no modal `Mode` active. Dispatch resolves
/// through `app.keys` - see `crate::keys` for the action
/// table this and `handle_embed_key` share.
fn handle_normal_key(
    app: &mut App,
    key: KeyEvent,
    backend: &Backend,
    resolver: &AskClient,
    options: &Options,
    embed_tx: &Sender<AppEvent>,
) {
    // Digits are a fixed jump table, never remapped (see `crate::keys`'s
    // module doc), checked before the registry so no remap can shadow
    // them. No embed to retarget here (this arm only runs with none open -
    // see `handle_embed_key` for the retargeting twin of this jump).
    if let KeyCode::Char(c) = key.code
        && c.is_ascii_digit()
        && c != '0'
    {
        // `c` is guaranteed an ascii '1'..='9' digit by the guard above, so
        // the byte subtraction is exact - no `Option`/`expect` needed.
        let n = (c as u8 - b'0') as usize;
        app.select_nth_seat(n);
        return;
    }
    let Some(action) = app.keys.action_for(Scope::Normal, &key) else {
        return;
    };
    match action {
        KeyAction::Quit => app.should_quit = true,
        KeyAction::Help => open_help(app),
        KeyAction::MoveDown => app.move_selection(1),
        KeyAction::MoveUp => app.move_selection(-1),
        KeyAction::SectionNext => app.cycle_section(1),
        KeyAction::SectionPrev => app.cycle_section(-1),
        KeyAction::Filter => app.mode = Mode::Filter(String::new()),
        KeyAction::SpawnPicker => enter_spawn_picker(app, options),
        KeyAction::Kill => request_kill_selected(app),
        KeyAction::Relaunch => launch_or_relaunch_selected(app, options),
        KeyAction::AskJump => {
            // Jump straight to the ask queue (still resolved via
            // ask_approve/ask_deny).
            app.selected = app
                .rows()
                .into_iter()
                .find(|row| row.section == Section::Asks)
                .map(|row| row.key);
            if app.selected.is_none() {
                app.status_line = "no pending asks".into();
            }
        }
        KeyAction::ShowAll => {
            // Escape hatch: reveal unmanaged sessions so one can be adopted.
            app.show_all = !app.show_all;
            app.status_line = if app.show_all {
                format!(
                    "showing all sessions ({} to scope to nopal; {} to adopt)",
                    app.keys.label(KeyAction::ShowAll),
                    app.keys.label(KeyAction::Adopt)
                )
            } else {
                format!(
                    "nopal-managed sessions only ({} to reveal all)",
                    app.keys.label(KeyAction::ShowAll)
                )
            };
            app.prune_selection();
        }
        // `goto_picker` is the goto picker (herdr's picker-jump
        // convention); `adopt` keeps its own separate mnemonic free
        // by design.
        KeyAction::GotoPicker => enter_goto_picker(app),
        KeyAction::Adopt => adopt_selected_seat(app, options),
        KeyAction::Reconcile => app.wants_reconcile = true,
        KeyAction::Profiling => {
            app.status_line = format!(
                "events reduced {} frames {}",
                app.events_reduced, app.frames_rendered
            );
        }
        KeyAction::AskApprove => {
            resolve_selected_ask(app, resolver, "approve", &options.resolve_by)
        }
        KeyAction::AskDeny => resolve_selected_ask(app, resolver, "deny", &options.resolve_by),
        // Opens the selected seat live in the main panel; runs and asks
        // keep their detail/context behavior.
        KeyAction::OpenView => match app.selected_row().map(|r| r.section) {
            Some(Section::Seats) => open_embed_for_selected(app, embed_tx),
            Some(Section::Plots) => {
                if let Some(plot_id) = app.selected_row().map(|row| row.key.clone()) {
                    select_plot_id(app, &plot_id);
                    retarget_selected_plot(app, embed_tx);
                }
            }
            _ => activate_selection(app, backend),
        },
        // The zero-overhead flagship path: hand full focus to the seat's
        // real pane (swap-pane locally, switch-client across sessions).
        KeyAction::Focus => focus_selected_seat(app, backend),
        // Keyboard routes to mouse-driven tmux plumbing: `split_right` and
        // `split_below` default to the divider
        // tmux-community's own split keys draw, `break_to_window` mirrors
        // the context menu's "break to window" entry. All three dispatch
        // straight from `app.selected_row()` - the non-seat-row and
        // already-split/already-slot guards live inside the handlers they
        // call (see `join_seat_into_slot`'s doc), so there is nothing to
        // check here.
        KeyAction::SplitRight => {
            if let Some(row) = app.selected_row() {
                split_selected_seat(app, &row.key, true);
            }
        }
        KeyAction::SplitBelow => {
            if let Some(row) = app.selected_row() {
                split_selected_seat(app, &row.key, false);
            }
        }
        KeyAction::BreakToWindow => {
            if let Some(row) = app.selected_row() {
                break_seat_to_window(app, backend, &row.key);
            }
        }
        // "Swap into slot": the sidebar keeps focus, no embed involved -
        // see `swap_selected_seat`'s doc for why it needs no embed-closing
        // dance the way `break_to_window`'s `handle_embed_key` arm does.
        KeyAction::SwapIntoSlot => {
            if let Some(row) = app.selected_row() {
                swap_selected_seat(app, &row.key);
            }
        }
        // Everything else in `KeyAction` belongs to `Scope::Embed` -
        // `action_for(Scope::Normal, ..)` cannot produce it, but the match
        // must stay exhaustive against the whole enum.
        _ => {}
    }
}

/// Adopt the selected unmanaged seat: stamp `@nopal_managed` on its session,
/// record it durably, and mark it locally so it stays visible.
fn adopt_selected_seat(app: &mut App, options: &Options) {
    let Some(row) = app.selected_row() else {
        app.status_line = "select a seat to adopt".to_owned();
        return;
    };
    if row.section != Section::Seats {
        app.status_line = format!(
            "{} adopts the selected seat",
            app.keys.label(KeyAction::Adopt)
        );
        return;
    }
    let Some(seat) = app.seats.get(&row.key) else {
        return;
    };
    let (session, repo, name, path) = (
        seat.session_name.clone(),
        seat.repo_tag(),
        seat.display_name(app.field_session_id.as_deref())
            .to_owned(),
        seat.path.clone(),
    );
    match Backend::mark_session_managed(&session, &repo) {
        Ok(()) => {
            crate::registry::record(
                &crate::registry::registry_path(options.state_dir.as_deref()),
                crate::registry::ManagedSeat {
                    session,
                    repo,
                    recorded_at: now_stamp(),
                    path,
                },
            );
            app.mark_seat_managed(&row.key);
            app.status_line = format!("adopted {name} into nopal");
        }
        Err(err) => app.status_line = format!("adopt failed: {err}"),
    }
}

/// Open the selected seat as a live embedded panel: zoom the field pane
/// for room, then attach the VT mirror. A seat currently split into the
/// field window is already visible right next to it - opening a VT
/// mirror of a pane sitting beside the mirror would show the same content
/// twice - so "open" on one just gives it terminal focus instead
/// ([`focus_split_seat`]). Every "open" entry point (single click, Enter,
/// the goto picker, the context menu) funnels through this one function, so
/// the guard only has to live here once.
fn open_embed_for_selected(app: &mut App, embed_tx: &Sender<AppEvent>) {
    let Some(row) = app.selected_row() else {
        return;
    };
    if row.section != Section::Seats {
        return;
    }
    if let Some(seat) = app.seats.get(&row.key)
        && seat.is_split()
    {
        let pane = seat.pane_id.clone();
        focus_split_seat(app, &pane);
        return;
    }
    if bind_stage_to_selected_seat(app) {
        let _ = Backend::set_zoom(&app.field_pane_id, true);
        sync_plot_transport(app, embed_tx);
        if let Some(embed) = &mut app.embed {
            embed.input_focus = true;
        }
        return;
    }
    let Some(seat) = app.seats.get(&row.key) else {
        return;
    };
    let label = seat
        .display_name(app.field_session_id.as_deref())
        .to_owned();
    let pane = seat.pane_id.clone();
    let _ = Backend::set_zoom(&app.field_pane_id, true);
    match crate::embed::Embed::open(&pane, &label, embed_tx.clone()) {
        Ok(mut embed) => {
            // Opening grants input focus immediately: the point of the
            // embed is to type at the seat; the release-input chord steps
            // back out.
            embed.input_focus = true;
            app.embed = Some(embed);
            app.status_line = seat_input_hint(app, &label);
        }
        Err(err) => {
            let _ = Backend::set_zoom(&app.field_pane_id, false);
            app.status_line = format!("embed failed: {err}");
        }
    }
}

fn bind_stage_to_selected_seat(app: &mut App) -> bool {
    let Some(row_key) = app.selected.clone() else {
        return false;
    };
    let Some(seat) = app.seats.get(&row_key) else {
        return false;
    };
    let (Some(plot_id), Some(session_id)) = (seat.plot_id.clone(), seat.plot_session_id.clone())
    else {
        return false;
    };
    if !select_plot_id(app, &plot_id)
        || !app.select_plot_activity(crate::state::PlotActivityKey::Session(session_id))
    {
        return false;
    }
    app.stage_open = true;
    true
}

/// Close the embedded panel and restore the sidebar-only layout.
fn close_embed(app: &mut App) {
    // Dropping the Embed stops its pipe and removes its fifo.
    app.embed = None;
    // A hidden inspector only means anything with a live panel beside it.
    app.inspector_collapsed = false;
    let _ = Backend::set_zoom(&app.field_pane_id, false);
    app.status_line.clear();
}

fn close_stage(app: &mut App) {
    app.embed = None;
    app.stage_open = false;
    app.inspector_collapsed = false;
    let _ = Backend::set_zoom(&app.field_pane_id, false);
    app.status_line.clear();
}

fn selected_session_transport(app: &App) -> Option<(&PlotSession, &crate::embed::Embed)> {
    let crate::state::PlotActivityKey::Session(session_id) = app.selected_plot_activity.as_ref()?
    else {
        return None;
    };
    let session = app
        .selected_plot()?
        .sessions
        .iter()
        .find(|session| &session.session_id == session_id)?;
    let embed = app.embed.as_ref()?;
    let pane = pane_for_plot_session(app, session)?;
    (embed.pane_id == pane).then_some((session, embed))
}

/// Align ephemeral tmux transport with the durable selected activity.
/// Executions and unavailable Sessions intentionally retain the stage while
/// dropping the mirror, so the rendered facts remain inspectable.
fn sync_plot_transport(app: &mut App, embed_tx: &Sender<AppEvent>) {
    if !app.stage_open {
        return;
    }
    let session = match app.selected_plot_activity.as_ref() {
        Some(crate::state::PlotActivityKey::Session(session_id)) => app
            .selected_plot()
            .and_then(|plot| {
                plot.sessions
                    .iter()
                    .find(|session| &session.session_id == session_id)
            })
            .cloned(),
        _ => None,
    };
    let Some(session) = session else {
        app.embed = None;
        return;
    };
    let Some(pane_id) = pane_for_plot_session(app, &session) else {
        app.embed = None;
        app.status_line = "selected Session has no live tmux pane".to_owned();
        return;
    };
    if app.embed.as_ref().map(|embed| embed.pane_id.as_str()) == Some(pane_id.as_str()) {
        app.selected = Some(pane_id);
        return;
    }
    app.embed = None;
    app.selected = Some(pane_id);
    retarget_embed(app, embed_tx);
}

/// Retarget the embedded mirror onto the currently selected seat, if the
/// selection moved to a different seat. Non-seat selections leave it be.
fn retarget_embed(app: &mut App, embed_tx: &Sender<AppEvent>) {
    let Some(row) = app.selected_row() else {
        return;
    };
    if row.section != Section::Seats {
        return;
    }
    let Some(seat) = app.seats.get(&row.key) else {
        return;
    };
    if app.embed.as_ref().map(|e| e.pane_id.as_str()) == Some(seat.pane_id.as_str()) {
        return;
    }
    if seat.is_split() {
        // Already visible next to the field UI as a real split pane -
        // the same guard `open_embed_for_selected` applies for a fresh
        // open (see its doc). Every retarget path (j/k, wheel, the goto
        // picker's Enter reaching an already-open embed, and now a row-
        // drag's center drop) funnels through here, so this is the one
        // place that guard has to live for a *retarget*, same as
        // `open_embed_for_selected` is the one place it lives for an
        // initial open.
        let pane = seat.pane_id.clone();
        focus_split_seat(app, &pane);
        return;
    }
    let label = seat
        .display_name(app.field_session_id.as_deref())
        .to_owned();
    let pane = seat.pane_id.clone();
    app.embed = None; // tear down the old mirror before starting the new one
    match crate::embed::Embed::open(&pane, &label, embed_tx.clone()) {
        Ok(embed) => {
            app.embed = Some(embed);
            app.status_line = seat_preview_hint(app, &label);
        }
        Err(err) => {
            app.status_line = format!("embed failed: {err}");
            // No live panel survived, so reset its inspector preference.
            app.inspector_collapsed = false;
        }
    }
}

fn retarget_selected_plot(app: &mut App, embed_tx: &Sender<AppEvent>) {
    app.stage_open = true;
    let _ = Backend::set_zoom(&app.field_pane_id, true);
    sync_plot_transport(app, embed_tx);
}

fn select_plot_id(app: &mut App, plot_id: &str) -> bool {
    let Some(index) = app.plots.keys().position(|id| id == plot_id) else {
        return false;
    };
    app.select_nth_plot(index + 1)
}

fn pane_for_plot_session(app: &App, session: &PlotSession) -> Option<String> {
    let matches_session = |seat: &&crate::state::Seat| {
        seat.plot_session_id.as_deref() == Some(session.session_id.as_str())
    };
    if let Some(host_pane) = &session.host_pane
        && let Some(seat) = app
            .seats
            .values()
            .filter(matches_session)
            .find(|seat| &seat.pane_id == host_pane)
    {
        return Some(seat.pane_id.clone());
    }
    app.seats
        .values()
        .filter(matches_session)
        .find(|seat| seat.active)
        .or_else(|| app.seats.values().find(matches_session))
        .map(|seat| seat.pane_id.clone())
}

/// Hand full focus to the selected seat's real pane (the flagship path).
fn focus_selected_seat(app: &mut App, backend: &Backend) {
    let Some(row) = app.selected_row() else {
        return;
    };
    if row.section == Section::Seats {
        activate_selection(app, backend);
    }
}

fn activate_selection(app: &mut App, backend: &Backend) {
    let Some(row) = app.selected_row() else {
        return;
    };
    match row.section {
        Section::Plots => {}
        Section::Seats => {
            let Some(seat) = app.seats.get(&row.key) else {
                return;
            };
            if seat.is_split() {
                // Already visible next to the field UI as a real split
                // pane: give it focus directly. The swap-into-slot dance
                // below assumes exactly one non-field pane in the
                // field window (the slot) and a seat living in its own
                // window elsewhere - a split pane violates both, so
                // routing it through `focus_seat`'s swap-pane would
                // reposition panes without keeping the `@nopal_role=split`
                // marker aligned with the true slot. The context menu's "return to
                // its window" is the deliberate action for a split seat
                // that wants to leave; "break to window" is the deliberate
                // action for a split seat that wants full focus in its own
                // window - `f` on one just focuses it in place.
                let pane = seat.pane_id.clone();
                focus_split_seat(app, &pane);
                return;
            }
            // A seat in another session: switch the client there (by ids,
            // never names). Getting back is the operator's own session
            // switcher (sesh last / prefix-L).
            if app.field_session_id.as_deref() != Some(seat.session_id.as_str()) {
                let Some(field_session) = app.field_session_id.clone() else {
                    return;
                };
                let (session_id, pane_id) = (seat.session_id.clone(), seat.pane_id.clone());
                match Backend::switch_to_pane(&field_session, &session_id, &pane_id) {
                    Ok(()) => app.status_line = format!("switched to {}", seat.session_name),
                    Err(err) => app.status_line = format!("switch failed: {err}"),
                }
                return;
            }
            // Same-session seat: swap it into the slot beside the sidebar.
            let Some(displaced) = app.focused_seat() else {
                app.status_line = "no focused-seat slot pane; is the field window intact?".into();
                return;
            };
            let slot = displaced.pane_id.clone();
            let displaced_label = if displaced.name.is_empty() {
                "shell".to_owned()
            } else {
                displaced.name.clone()
            };
            // The incoming seat's home window is where the displaced pane
            // lands; the backend renames it only if the field named it.
            let Some((vacated_window, vacated_name)) = app
                .seats
                .get(&row.key)
                .map(|s| (s.window_id.clone(), s.window_name.clone()))
            else {
                return;
            };
            match backend.focus_seat(
                &row.key,
                &slot,
                &vacated_window,
                &vacated_name,
                &displaced_label,
            ) {
                Ok(()) => {
                    app.status_line = String::new();
                    app.wants_reconcile = true;
                }
                Err(err) => app.status_line = format!("focus failed: {err}"),
            }
        }
        Section::AfkRuns => app.mode = Mode::RunDetail(row.key),
        Section::Asks => {
            // Enter on an ask surfaces its full context in the status line;
            // resolution stays a deliberate y/d keystroke (no modal).
            if let Some(ask) = app.asks.iter().find(|ask| ask.ask_id == row.key) {
                app.status_line = format!(
                    "{} by {}: {} (expires {})",
                    ask.action, ask.session_id, ask.reason, ask.expires_at
                );
            }
        }
    }
}

fn resolve_selected_ask(app: &mut App, resolver: &AskClient, decision: &str, by: &str) {
    let Some(row) = app.selected_row() else {
        app.status_line = "select an ask first".into();
        return;
    };
    if row.section != Section::Asks {
        app.status_line = "y/d act on the selected ask".into();
        return;
    }
    match resolver.resolve(&row.key, decision, by) {
        Ok(()) => {
            app.status_line = format!("{decision}d {}", row.key);
            // Drop it locally; the next poll confirms.
            app.asks.retain(|ask| ask.ask_id != row.key);
            app.selected = None;
        }
        Err(err) => app.status_line = format!("resolve failed: {err}"),
    }
}

/// Spawn or attach a seat at `path` natively: resolve its
/// session name via [`seat::naming::resolve_session_name`], probing
/// live sessions and the registry's recorded paths for collisions, then
/// hand off to `Backend::spawn_session_seat`. A successful spawn is
/// recorded in the registry with its path, so it reappears as a recent
/// candidate and survives a resurrect restore. The placement explanation
/// comes from Nopal Core (`nopal placement`) - the field only routes and
/// reports it. Leaves `Mode::Normal` either way.
struct SpawnedSeat {
    host_session: String,
    host_pane: String,
}

fn spawn_seat_at(app: &mut App, path: &str, options: &Options) -> Option<SpawnedSeat> {
    let registry_path = crate::registry::registry_path(options.state_dir.as_deref());
    let entries = crate::registry::load(&registry_path);
    let live_path_cache: RefCell<BTreeMap<String, Option<String>>> = RefCell::new(BTreeMap::new());
    let known_path = |name: &str| -> Option<String> {
        entries
            .iter()
            .find(|entry| entry.session == name && !entry.path.is_empty())
            .map(|entry| entry.path.clone())
            .or_else(|| {
                let mut cache = live_path_cache.borrow_mut();
                cache
                    .entry(name.to_owned())
                    .or_insert_with(|| Backend::session_active_path(name))
                    .clone()
            })
    };
    let name =
        seat::naming::resolve_session_name(path, crate::tmux::session_exists_named, known_path);
    let placement = placement_note(&options.nopal_bin);
    let agent_cmd = format!(
        "{} cli",
        crate::tmux::shell_quote(&options.nopal_bin.to_string_lossy())
    );
    let size = Backend::pane_window_size(&app.field_pane_id);
    let spawned = match Backend::spawn_session_seat(path, &name, &agent_cmd, size) {
        Ok((how, session, pane_id)) => {
            crate::registry::record(
                &registry_path,
                crate::registry::ManagedSeat {
                    session: session.clone(),
                    repo: crate::state::worktree_repo_tag(path),
                    recorded_at: now_stamp(),
                    path: path.to_owned(),
                },
            );
            app.status_line = format!("spawned via {how}; {placement}");
            app.wants_reconcile = true;
            // Surface the seat in the embedded panel instead of switching
            // the client away; fulfilled here for
            // attaches (seat already known) or on the reconcile that
            // delivers a fresh spawn.
            app.pending_embed_session = Some(session);
            Some(SpawnedSeat {
                host_session: name,
                host_pane: pane_id,
            })
        }
        Err(err) => {
            app.status_line = format!("spawn failed: {err}");
            None
        }
    };
    if let Some(spawned) = &spawned {
        bind_spawned_plot(app, path, options, spawned);
    }
    app.mode = Mode::Normal;
    spawned
}

fn bind_spawned_plot(app: &mut App, path: &str, options: &Options, spawned: &SpawnedSeat) {
    let Some((plot_id, provisional, event)) = app.selected_plot().map(|plot| {
        (
            plot.plot_id.clone(),
            plot.provisional,
            plot.establishment
                .as_ref()
                .map(|establishment| establishment.event.clone()),
        )
    }) else {
        return;
    };
    let env = nopal_core::plot_store::PlotEnv::discover(options.state_dir.as_deref());
    let result = if provisional {
        nopal_core::plot_store::bind_session(
            &env,
            &plot_id,
            &spawned.host_session,
            Some(&spawned.host_pane),
        )
        .map_err(|error| error.to_string())
    } else if let Some(event) = event {
        let report = nopal_core::plot_report::establish(
            options.state_dir.as_deref(),
            Some(&plot_id),
            &options.session,
            &event,
            std::path::Path::new(path),
            &spawned.host_session,
            Some(&spawned.host_pane),
        );
        report.plot.ok_or_else(|| {
            report.diagnostics.first().map_or_else(
                || "Plot Establishment failed".to_owned(),
                |diagnostic| diagnostic.message.clone(),
            )
        })
    } else {
        Err("established Plot has no Establishment event".to_owned())
    };
    match result {
        Ok(plot) => {
            if let Some(session_id) = bound_session_id(&plot, &spawned.host_session)
                && let Err(error) =
                    Backend::stamp_plot_identity(&spawned.host_session, &plot.plot_id, session_id)
            {
                app.status_line = format!("Session started; Plot identity pending: {error}");
            }
        }
        Err(error) => {
            app.status_line = format!("Session started but Plot Workspace binding failed: {error}");
        }
    }
}

fn bound_session_id<'a>(
    plot: &'a nopal_core::plot::PlotDocument,
    host_session: &str,
) -> Option<&'a str> {
    plot.sessions
        .iter()
        .find(|session| session.host_session == host_session)
        .map(|session| session.session_id.as_str())
}

/// Open the embedded panel on a just-spawned seat once it exists in the
/// sidebar. No-op until reconcile delivers the seat; skipped (kept
/// pending) while a modal owns the screen so the panel never opens under
/// help or the picker.
fn fulfill_pending_embed(app: &mut App, embed_tx: &Sender<AppEvent>) {
    let Some(session) = app.pending_embed_session.clone() else {
        return;
    };
    if app.mode != Mode::Normal {
        return;
    }
    let key = app.rows().into_iter().find_map(|row| {
        (row.section == Section::Seats
            && app
                .seats
                .get(&row.key)
                .is_some_and(|seat| seat.session_name == session))
        .then_some(row.key)
    });
    let Some(key) = key else {
        return;
    };
    app.pending_embed_session = None;
    app.selected = Some(key);
    open_embed_for_selected(app, embed_tx);
}

/// `x`: ask before killing. Nothing dies until [`handle_confirm_kill_key`]
/// sees a `y`, preventing accidental seat loss.
fn request_kill_selected(app: &mut App) {
    let Some(row) = app.selected_row() else {
        app.status_line = "select a seat to kill".to_owned();
        return;
    };
    if row.section != Section::Seats {
        app.status_line = format!(
            "{} kills the selected seat",
            app.keys.label(KeyAction::Kill)
        );
        return;
    }
    let Some(seat) = app.seats.get(&row.key) else {
        return;
    };
    let name = seat
        .display_name(app.field_session_id.as_deref())
        .to_owned();
    app.status_line = format!("kill {name}? y/n");
    app.mode = Mode::ConfirmKill(row.key.clone());
}

/// `s`: (re)launch the agent in the selected seat's pane. A seat already
/// running the agent is left alone - relaunching would send another
/// `nopal cli` into a live agent. "Already running" is the same
/// process-tree-or-foreground-command check the seat glyph uses
/// ([`crate::state::App::agent_panes`], falling back to
/// `pane_current_command`): the foreground command alone is fooled by
/// shell-integration wrappers (kiro-cli-term and similar) that keep the
/// pane reporting its login shell while the agent runs as a descendant.
fn launch_or_relaunch_selected(app: &mut App, options: &Options) {
    let Some(row) = app.selected_row() else {
        app.status_line = "select a seat first".to_owned();
        return;
    };
    if row.section != Section::Seats {
        app.status_line = format!(
            "{} launches the agent in the selected seat",
            app.keys.label(KeyAction::Relaunch)
        );
        return;
    }
    let Some(seat) = app.seats.get(&row.key) else {
        return;
    };
    let name = seat
        .display_name(app.field_session_id.as_deref())
        .to_owned();
    if app.agent_panes.contains(&seat.pane_id) || matches!(seat.command.as_str(), "nopal" | "pi") {
        app.status_line = format!("agent already running in {name}");
        return;
    }
    let pane_id = seat.pane_id.clone();
    let agent_cmd = format!(
        "{} cli",
        crate::tmux::shell_quote(&options.nopal_bin.to_string_lossy())
    );
    match Backend::launch_agent_in_pane(&pane_id, &agent_cmd) {
        Ok(()) => app.status_line = format!("launched nopal cli in {name}"),
        Err(err) => app.status_line = format!("launch failed: {err}"),
    }
}

/// Ask nopal core how it would place a seat-spawn action; report-only.
fn placement_note(nopal_bin: &std::path::Path) -> String {
    let argv = vec![
        nopal_bin.to_string_lossy().into_owned(),
        "placement".to_owned(),
        "--action".to_owned(),
        "seat.spawn".to_owned(),
        "--json".to_owned(),
    ];
    match run_json_command(&argv, None) {
        Ok(report) => {
            let placement = report
                .get("placement")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let source = report
                .get("placement_source")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!("core placement: {placement} ({source})")
        }
        Err(err) => format!("placement unavailable: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        Plot, PlotActivityKey, PlotExecution, PlotInspectorTab, PlotSession, Row, Seat,
    };
    use ratatui::layout::Rect;

    fn hover_app(row: Row, input_focus: bool) -> App {
        let mut app = App::new("%field".to_owned(), "nopal".to_owned());
        app.embed = Some(crate::embed::Embed::test_for_app(
            "%current",
            "current",
            input_focus,
        ));
        app.hit.rows.push((Rect::new(0, 0, 20, 1), row));
        app
    }

    fn seat(pane_id: &str, role: &str) -> Seat {
        Seat {
            pane_id: pane_id.to_owned(),
            role: role.to_owned(),
            ..Seat::default()
        }
    }

    fn stage_app() -> App {
        let mut app = App::new("%field".to_owned(), "nopal".to_owned());
        app.plots.insert(
            "plot-a".to_owned(),
            Plot {
                plot_id: "plot-a".to_owned(),
                title: "Plot A".to_owned(),
                provisional: false,
                progress: "active".to_owned(),
                conditions: Vec::new(),
                seed_source: "test".to_owned(),
                seed_text: String::new(),
                intent: String::new(),
                fruit_state: "absent".to_owned(),
                executions: vec![PlotExecution {
                    service_id: "rondo-core".to_owned(),
                    repo_id: "repo-a".to_owned(),
                    run_id: "run-a".to_owned(),
                    manifest_sha256: "a".repeat(64),
                    status: "running".to_owned(),
                    outcome: None,
                    event_cursor: "rondo.core/v1:1".to_owned(),
                    evidence: Vec::new(),
                    created_at: "created".to_owned(),
                    updated_at: "updated".to_owned(),
                }],
                sessions: vec![PlotSession {
                    session_id: "session-a".to_owned(),
                    mode: "interactive".to_owned(),
                    host: "pi".to_owned(),
                    host_session: "nopal-work".to_owned(),
                    host_pane: Some("%session".to_owned()),
                    state: "active".to_owned(),
                    workspace: None,
                }],
                selected_session_id: Some("session-a".to_owned()),
                establishment: None,
                repositories: Vec::new(),
                workspaces: Vec::new(),
            },
        );
        app.selected_plot_id = Some("plot-a".to_owned());
        app.selected_plot_activity = Some(PlotActivityKey::Session("session-a".to_owned()));
        app.stage_open = true;
        app.seats.insert(
            "%session".to_owned(),
            Seat {
                pane_id: "%session".to_owned(),
                plot_id: Some("plot-a".to_owned()),
                plot_session_id: Some("session-a".to_owned()),
                ..Seat::default()
            },
        );
        app.embed = Some(crate::embed::Embed::test_for_app(
            "%session", "session", false,
        ));
        app
    }

    #[test]
    fn execution_selection_drops_transport_but_keeps_the_stage_open() {
        let mut app = stage_app();
        app.selected_plot_activity = Some(PlotActivityKey::Execution {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: "run-a".to_owned(),
        });
        let (tx, _) = std::sync::mpsc::channel();

        sync_plot_transport(&mut app, &tx);

        assert!(app.stage_open);
        assert!(app.embed.is_none());
    }

    #[test]
    fn stage_tab_cycles_activity_and_close_retains_the_selection() {
        let mut app = stage_app();
        let (tx, _) = std::sync::mpsc::channel();
        let backend = Backend::new("nopal".to_owned());

        handle_stage_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &backend,
            &tx,
        );
        let selected = app.selected_plot_activity.clone();
        assert!(matches!(selected, Some(PlotActivityKey::Execution { .. })));
        assert!(app.embed.is_none());

        app.inspector_collapsed = true;
        handle_stage_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &backend,
            &tx,
        );
        assert!(!app.stage_open);
        assert!(!app.inspector_collapsed);
        assert_eq!(app.selected_plot_activity, selected);
    }

    #[test]
    fn stage_mouse_selects_exact_activity_and_inspector_tab_and_scrolls_regions() {
        let mut app = stage_app();
        let execution = PlotActivityKey::Execution {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: "run-a".to_owned(),
        };
        app.hit
            .activity_tabs
            .push((Rect::new(30, 1, 10, 1), execution.clone()));
        app.hit
            .inspector_tabs
            .push((Rect::new(130, 1, 8, 1), PlotInspectorTab::Evidence));
        app.hit.main = Some(Rect::new(28, 3, 98, 40));
        app.hit.inspector = Some(Rect::new(126, 0, 34, 45));
        let (tx, _) = std::sync::mpsc::channel();
        let backend = Backend::new("nopal".to_owned());

        handle_left_click(&mut app, 31, 1, &backend, &tx);
        assert_eq!(app.selected_plot_activity, Some(execution));
        assert!(app.embed.is_none());
        handle_left_click(&mut app, 131, 1, &backend, &tx);
        assert_eq!(app.inspector_tab, PlotInspectorTab::Evidence);

        handle_wheel(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 40,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        handle_wheel(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 140,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert_eq!(app.main_scroll, 3);
        assert_eq!(app.inspector_scroll, 3);
    }

    #[test]
    fn execution_stage_keeps_global_ctrl_c_quit() {
        let mut app = stage_app();
        app.selected_plot_activity = Some(PlotActivityKey::Execution {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: "run-a".to_owned(),
        });
        sync_plot_transport(&mut app, &std::sync::mpsc::channel().0);
        let backend = Backend::new("nopal".to_owned());

        handle_stage_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &backend,
            &std::sync::mpsc::channel().0,
        );

        assert!(app.should_quit);
    }

    #[test]
    fn unbound_legacy_embed_remains_visible_without_a_plot_stage() {
        let mut app = App::new("%field".to_owned(), "nopal".to_owned());
        app.embed = Some(crate::embed::Embed::test_for_app(
            "%legacy", "%legacy", false,
        ));
        let backend = ratatui::backend::TestBackend::new(80, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();

        assert!(!app.stage_open);
        assert!(app.hit.embed_grid.is_some());
        assert!(app.hit.panel.is_some());
    }

    #[test]
    fn plot_bound_seat_selects_its_owning_stage_activity() {
        let mut app = stage_app();
        app.stage_open = false;
        app.selected_plot_activity = None;
        app.selected = Some("%session".to_owned());

        assert!(bind_stage_to_selected_seat(&mut app));

        assert!(app.stage_open);
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a".to_owned()))
        );
    }

    #[test]
    fn plot_refresh_preserves_valid_activity_and_vanished_session_keeps_stage_open() {
        let mut app = stage_app();
        let original = app.plots["plot-a"].clone();
        let (tx, _) = std::sync::mpsc::channel();

        app.reduce_feed(crate::state::FeedEvent::Plots(vec![original.clone()]));
        sync_plot_transport(&mut app, &tx);
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a".to_owned()))
        );
        assert!(app.embed.is_some());

        let mut without_session = original;
        without_session.sessions.clear();
        without_session.selected_session_id = None;
        app.reduce_feed(crate::state::FeedEvent::Plots(vec![without_session]));
        sync_plot_transport(&mut app, &tx);
        assert!(app.stage_open);
        assert!(matches!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Execution { .. })
        ));
        assert!(app.embed.is_none());
    }

    #[test]
    fn vanished_tmux_pane_drops_only_transport_for_the_selected_session() {
        let mut app = stage_app();
        let (tx, _) = std::sync::mpsc::channel();

        app.seats.clear();
        sync_plot_transport(&mut app, &tx);

        assert!(app.stage_open);
        assert_eq!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Session("session-a".to_owned()))
        );
        assert!(app.embed.is_none());
        assert!(app.status_line.contains("no live tmux pane"));
    }

    #[test]
    fn execution_activity_ignores_session_only_focus_and_search_actions() {
        let mut app = stage_app();
        app.selected_plot_activity = Some(PlotActivityKey::Execution {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: "run-a".to_owned(),
        });
        let (tx, _) = std::sync::mpsc::channel();
        sync_plot_transport(&mut app, &tx);
        let backend = Backend::new("nopal".to_owned());

        for key in [KeyCode::Enter, KeyCode::Char('i'), KeyCode::Char('/')] {
            handle_stage_key(
                &mut app,
                KeyEvent::new(key, KeyModifiers::NONE),
                &backend,
                &tx,
            );
        }

        assert!(app.stage_open);
        assert!(app.embed.is_none());
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn stage_plot_navigation_reconciles_activity_before_transport_sync() {
        let mut app = stage_app();
        let plot_a = app.plots.get_mut("plot-a").unwrap();
        plot_a.sessions.clear();
        plot_a.selected_session_id = None;
        app.selected_plot_activity = Some(PlotActivityKey::Execution {
            service_id: "rondo-core".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: "run-a".to_owned(),
        });
        let mut plot_b = app.plots["plot-a"].clone();
        plot_b.plot_id = "plot-b".to_owned();
        plot_b.executions[0].repo_id = "repo-b".to_owned();
        plot_b.executions[0].run_id = "run-b".to_owned();
        app.plots.insert("plot-b".to_owned(), plot_b);
        let (tx, _) = std::sync::mpsc::channel();
        sync_plot_transport(&mut app, &tx);
        let backend = Backend::new("nopal".to_owned());

        handle_stage_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &backend,
            &tx,
        );
        assert_eq!(app.selected_plot_id.as_deref(), Some("plot-b"));
        assert!(matches!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Execution { ref run_id, .. }) if run_id == "run-b"
        ));
        handle_stage_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            &backend,
            &tx,
        );
        assert_eq!(app.selected_plot_id.as_deref(), Some("plot-a"));
        assert!(matches!(
            app.selected_plot_activity,
            Some(PlotActivityKey::Execution { ref run_id, .. }) if run_id == "run-a"
        ));
        assert!(app.embed.is_none());
    }

    #[test]
    fn spawned_plot_identity_uses_the_spawned_session_not_the_plot_selection() {
        let dir = tempfile::tempdir().unwrap();
        let env = nopal_core::plot_store::PlotEnv::discover(Some(dir.path()));
        let plot = nopal_core::plot_store::ensure_provisional(&env, "field").unwrap();
        nopal_core::plot_store::bind_session(&env, &plot.plot_id, "session-a", Some("%1")).unwrap();
        let plot =
            nopal_core::plot_store::bind_session(&env, &plot.plot_id, "session-b", Some("%2"))
                .unwrap();

        assert_eq!(
            bound_session_id(&plot, "session-a"),
            Some(plot.sessions[0].session_id.as_str())
        );
        assert_ne!(
            bound_session_id(&plot, "session-a"),
            plot.selected_session_id.as_deref()
        );
    }

    #[test]
    fn new_worktree_candidate_uses_its_exact_project_root() {
        let mut app = App::new("%field".to_owned(), "nopal".to_owned());
        let candidate = Candidate {
            label: "+ new worktree in service".to_owned(),
            path: String::new(),
            project: "service".to_owned(),
            project_root: "/team-b/service".to_owned(),
            kind: CandidateKind::NewWorktree,
        };
        let options = Options {
            session: "nopal".to_owned(),
            nopal_bin: PathBuf::from("nopal"),
            state_dir: None,
            rondo_events: None,
            rondo_dir: PathBuf::from("rondo"),
            rondo_runs: Vec::new(),
            resolve_by: "operator".to_owned(),
            show_all: false,
        };

        activate_candidate(&mut app, candidate, &options);

        assert_eq!(
            app.mode,
            Mode::WorktreeName {
                project: "/team-b/service".to_owned(),
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn hover_ignores_rows_when_no_embed_is_open() {
        let mut app = hover_app(
            Row {
                section: Section::Seats,
                key: "%seat".to_owned(),
            },
            false,
        );
        app.embed = None;
        app.seats.insert("%seat".to_owned(), seat("%seat", ""));
        let mut retargets = 0;
        handle_hover_with_retarget(&mut app, 1, 0, |_| retargets += 1);
        assert_eq!(app.selected, None);
        assert_eq!(retargets, 0);
    }

    #[test]
    fn hover_ignores_rows_while_seat_has_input_focus() {
        let mut app = hover_app(
            Row {
                section: Section::Seats,
                key: "%seat".to_owned(),
            },
            true,
        );
        app.seats.insert("%seat".to_owned(), seat("%seat", ""));
        let mut retargets = 0;
        handle_hover_with_retarget(&mut app, 1, 0, |_| retargets += 1);
        assert_eq!(app.selected, None);
        assert_eq!(retargets, 0);
    }

    #[test]
    fn hover_ignores_non_seat_rows() {
        let mut app = hover_app(
            Row {
                section: Section::AfkRuns,
                key: "ledger:x".to_owned(),
            },
            false,
        );
        let mut retargets = 0;
        handle_hover_with_retarget(&mut app, 1, 0, |_| retargets += 1);
        assert_eq!(app.selected, None);
        assert_eq!(retargets, 0);
    }

    #[test]
    fn hover_ignores_the_already_selected_seat() {
        let mut app = hover_app(
            Row {
                section: Section::Seats,
                key: "%seat".to_owned(),
            },
            false,
        );
        app.selected = Some("%seat".to_owned());
        app.seats.insert("%seat".to_owned(), seat("%seat", ""));
        let mut retargets = 0;
        handle_hover_with_retarget(&mut app, 1, 0, |_| retargets += 1);
        assert_eq!(app.selected.as_deref(), Some("%seat"));
        assert_eq!(retargets, 0);
    }

    #[test]
    fn hover_ignores_split_seats_so_pointer_motion_does_not_move_tmux_focus() {
        let mut app = hover_app(
            Row {
                section: Section::Seats,
                key: "%seat".to_owned(),
            },
            false,
        );
        app.seats.insert("%seat".to_owned(), seat("%seat", "split"));
        let mut retargets = 0;
        handle_hover_with_retarget(&mut app, 1, 0, |_| retargets += 1);
        assert_eq!(app.selected, None);
        assert_eq!(retargets, 0);
    }

    #[test]
    fn hover_selects_and_retargets_an_ordinary_seat_row() {
        let mut app = hover_app(
            Row {
                section: Section::Seats,
                key: "%seat".to_owned(),
            },
            false,
        );
        app.seats.insert("%seat".to_owned(), seat("%seat", ""));
        let mut retargets = 0;
        handle_hover_with_retarget(&mut app, 1, 0, |_| retargets += 1);
        assert_eq!(app.selected.as_deref(), Some("%seat"));
        assert_eq!(retargets, 1);
    }

    #[test]
    fn plot_session_routing_prefers_bound_then_active_panes() {
        let mut app = App::new("%field".to_owned(), "nopal".to_owned());
        let tagged_seat = |pane: &str, active: bool| Seat {
            pane_id: pane.to_owned(),
            plot_session_id: Some("session-1".to_owned()),
            active,
            ..Seat::default()
        };
        app.seats.insert("%1".to_owned(), tagged_seat("%1", false));
        app.seats.insert("%2".to_owned(), tagged_seat("%2", true));
        app.seats.insert("%3".to_owned(), tagged_seat("%3", false));
        let mut session = PlotSession {
            session_id: "session-1".to_owned(),
            mode: "interactive".to_owned(),
            host: "pi".to_owned(),
            host_session: "nopal-work".to_owned(),
            host_pane: Some("%3".to_owned()),
            state: "active".to_owned(),
            workspace: None,
        };

        assert_eq!(pane_for_plot_session(&app, &session).as_deref(), Some("%3"));
        session.host_pane = None;
        assert_eq!(pane_for_plot_session(&app, &session).as_deref(), Some("%2"));
        app.seats.get_mut("%2").unwrap().active = false;
        assert_eq!(pane_for_plot_session(&app, &session).as_deref(), Some("%1"));
    }
}
